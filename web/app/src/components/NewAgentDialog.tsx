import { useEffect, useState } from "react";
import { api, describeError, isUncertain } from "@/lib/api";
import type { NewResponse, Worktree } from "@/lib/types";
import { kindLabel, statusLabel } from "@/lib/status";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import { MAX_MESSAGE } from "./Composer";

const KINDS = ["pi", "claude", "codex", "cursor"] as const;

export function NewAgentDialog({ project, open, onOpenChange, onCreated }: {
  project: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (result: NewResponse) => void;
}) {
  const [trees, setTrees] = useState<Worktree[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [worktree, setWorktree] = useState("");
  const [kind, setKind] = useState<(typeof KINDS)[number]>("pi");
  const [prompt, setPrompt] = useState("");
  const [state, setState] = useState<"idle" | "submitting" | "uncertain">("idle");

  useEffect(() => {
    if (!open) return;
    const abort = new AbortController();
    setTrees(null); setError(null); setState("idle");
    api<{ worktrees: Worktree[] }>("worktrees", { project }, undefined, { signal: abort.signal })
      .then((data) => {
        setTrees(data.worktrees);
        // A remembered path only survives if it still belongs to this project's checkouts.
        setWorktree((w) => (data.worktrees.some((t) => t.path === w) ? w : data.worktrees[0]?.path ?? ""));
      })
      .catch((failure) => { if (!abort.signal.aborted) setError(describeError(failure)); });
    return () => abort.abort();
  }, [open, project]);

  const selected = trees?.find((w) => w.path === worktree);
  const existing = selected?.sessions.filter((s) => s.alive) ?? [];

  const submit = async () => {
    if (state !== "idle" || !prompt.trim() || !worktree) return;
    setState("submitting"); setError(null);
    try {
      const result = await api<NewResponse>("new", {}, { project, worktree, kind, text: prompt }, { timeoutMs: 40000 });
      setPrompt("");
      setState("idle");
      onCreated(result);
      onOpenChange(false);
    } catch (failure) {
      const uncertain = isUncertain(failure);
      setState(uncertain ? "uncertain" : "idle");
      setError(uncertain
        ? `${describeError(failure)} Pornirea poate să fi reușit: verifică lista de sesiuni înainte de a reîncerca.`
        : describeError(failure));
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Pornește un agent</DialogTitle>
          <DialogDescription>Pe un checkout existent din proiectul {project}, pe server.</DialogDescription>
        </DialogHeader>
        <form className="flex flex-col gap-4" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Checkout</span>
            <select value={worktree} onChange={(e) => setWorktree(e.target.value)} required disabled={!trees}
              className="h-11 rounded-lg border border-input bg-card px-3 text-base outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60">
              {!trees && <option value="">Se încarcă…</option>}
              {trees?.map((w) => <option key={w.path} value={w.path}>{w.branch}{w.is_main ? " (principal)" : ""} · {w.sessions.length} sesiuni</option>)}
            </select>
            {selected && <span className="truncate font-mono text-xs text-muted-foreground">{selected.path}</span>}
          </label>
          {existing.length > 0 && (
            <p role="status" className="rounded-lg border border-border px-3 py-2 text-sm">
              Pe acest checkout lucrează deja: {existing.map((s) => `${s.name} (${kindLabel(s.kind)}, ${statusLabel(s.status)})`).join(", ")}.
              Evită doi agenți care scriu pe același branch.
            </p>
          )}
          <fieldset className="flex flex-col gap-1.5 text-sm">
            <legend className="mb-1.5 font-medium">Agent</legend>
            <div className="grid grid-cols-4 gap-1 rounded-lg border border-border p-1">
              {KINDS.map((k) => (
                <label key={k} className={`flex h-9 cursor-pointer items-center justify-center rounded-md text-sm ${kind === k ? "bg-primary font-medium text-primary-foreground" : "hover:bg-muted"}`}>
                  <input type="radio" name="kind" value={k} checked={kind === k} onChange={() => setKind(k)} className="sr-only" />
                  {kindLabel(k)}
                </label>
              ))}
            </div>
          </fieldset>
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Prima sarcină</span>
            <textarea value={prompt} onChange={(e) => setPrompt(e.target.value)} required maxLength={MAX_MESSAGE} rows={4}
              placeholder="Ce vrei să facă agentul?"
              className="field-sizing-content max-h-60 min-h-24 rounded-lg border border-input bg-card px-3 py-2 text-base outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60" />
          </label>
          {error && <p role="alert" className="text-sm">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>Renunță</Button>
            <Button type="submit" disabled={state === "submitting" || !prompt.trim() || !worktree}>
              {state === "submitting" ? "Se pornește…" : "Pornește"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
