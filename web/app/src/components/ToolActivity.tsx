import { memo, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import { api, ApiError } from "@/lib/api";
import type { ToolPart, ToolResult } from "@/lib/types";
import { Button } from "@/components/ui/button";
import { CopyButton } from "./Markdown";

const STATE_LABEL: Record<ToolPart["state"], string> = { pending: "în lucru", done: "gata", error: "eroare" };

function summary(part: ToolPart): string {
  try {
    const args = JSON.parse(part.args) as Record<string, unknown>;
    const hint = args.command ?? args.path ?? args.file_path ?? args.pattern ?? args.query ?? args.url;
    if (typeof hint === "string") return hint.split("\n")[0].slice(0, 120);
  } catch {
    // arguments are bounded text, not necessarily JSON (bash!)
    return part.args.split("\n")[0].slice(0, 120);
  }
  return "";
}

export const ToolActivity = memo(function ToolActivity({ part, project, session }: {
  part: ToolPart; project: string; session: string;
}) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState<ToolResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const output = full?.output ?? part.output;
  const truncated = full ? full.truncated : part.truncated;
  const hint = summary(part);

  const loadFull = async () => {
    setLoading(true); setError(null);
    try {
      const data = await api<{ tool: ToolResult }>("conversation/tool", { project, id: session, call: part.callId });
      setFull(data.tool);
    } catch (failure) {
      setError(failure instanceof ApiError ? failure.message : "Nu s-a putut încărca");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="my-1 rounded-lg border border-border bg-card text-sm" data-tool-state={part.state}>
      <button type="button" onClick={() => setOpen((v) => !v)} aria-expanded={open}
        className="flex min-h-11 w-full items-center gap-2 px-3 py-2 text-left hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <ChevronRightIcon aria-hidden className={`size-4 shrink-0 text-muted-foreground transition-transform ${open ? "rotate-90" : ""}`} />
        <span className="font-mono text-[13px] font-medium">{part.name}</span>
        {hint && <span className="min-w-0 flex-1 truncate font-mono text-[13px] text-muted-foreground">{hint}</span>}
        <span className="ml-auto shrink-0 text-xs text-muted-foreground">
          {part.state === "pending" && <span className="mr-1 inline-block size-1.5 animate-pulse rounded-full bg-foreground align-middle" aria-hidden />}
          {STATE_LABEL[part.state]}
        </span>
      </button>
      {open && (
        <div className="border-t border-border px-3 py-2">
          <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
            <span>Argumente</span><CopyButton text={part.args} label="Copiază argumentele" />
          </div>
          <pre className="max-h-48 overflow-auto rounded bg-muted/60 p-2 font-mono text-xs leading-relaxed">{part.args}{part.argsTruncated ? "\n…" : ""}</pre>
          {(output || part.state !== "pending") && (
            <>
              <div className="mt-2 mb-1 flex items-center justify-between text-xs text-muted-foreground">
                <span>Rezultat{part.outputLength ? ` · ${part.outputLength.toLocaleString("ro-RO")} caractere` : ""}</span>
                {output && <CopyButton text={output} label="Copiază rezultatul" />}
              </div>
              <pre className="max-h-80 overflow-auto rounded bg-muted/60 p-2 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words">
                {output || "(fără rezultat)"}
              </pre>
              {truncated && (
                <div className="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
                  <span>Afișat parțial.</span>
                  {!full && <Button type="button" size="xs" variant="outline" onClick={loadFull} disabled={loading}>
                    {loading ? "Se încarcă…" : "Încarcă tot"}
                  </Button>}
                  {error && <span role="alert">{error}</span>}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
});
