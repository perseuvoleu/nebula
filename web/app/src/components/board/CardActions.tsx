import { useState } from "react";
import { CodeIcon, CopyIcon, PlayIcon } from "lucide-react";
import { describeError } from "@/lib/api";
import { type BoardData, type CardShow, STAGES, STAGE_LABEL, type Stage } from "@/lib/board";
import { cardAction, ulid } from "@/lib/board-actions";
import { Button } from "@/components/ui/button";
import { RunDialog, type RunDraft } from "./RunDialog";

const DEFAULT_HOST = "leaseweb";
const fieldCls = "h-9 min-w-0 rounded-lg border border-input bg-card px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60";

export function CardActions({ show, board }: { show: CardShow; board: BoardData | null }) {
  const hosts = (board?.hosts ?? []).map((h) => ({ host: h.host, writers: h.writers, writerSlots: h.writer_slots }));
  const branch = show.checkout?.branch ?? null;
  const [run, setRun] = useState<{ draft: RunDraft; dispatch: string } | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [opened, setOpened] = useState<{ ok: boolean; host: string; command: string } | null>(null);
  const [blocker, setBlocker] = useState("");
  const [note, setNote] = useState(() => ({ text: "", id: ulid() }));

  const act = async (name: string, request: () => Promise<unknown>) => {
    setPending(name);
    setError(null);
    try {
      await request();
      return true;
    } catch (failure) {
      setError(describeError(failure));
      return false;
    } finally {
      setPending(null);
    }
  };

  const sessions = show.sessions;
  const target = sessions.find((s) => s.writer && s.alive) ?? sessions.at(-1);
  const candidates = (board?.cards ?? []).filter((c) => !c.archived && c.id !== show.id && !show.blocked_by.includes(c.ref));

  const openRun = () => setRun({
    draft: { role: "worker", kind: "claude", host: hosts.some((h) => h.host === DEFAULT_HOST) ? DEFAULT_HOST : hosts[0]?.host ?? DEFAULT_HOST, worktree: "new" },
    dispatch: ulid(),
  });
  const startRun = async () => {
    if (!run) return;
    const worktree = run.draft.worktree === "current" && branch ? branch : "new";
    if (await act("run", () => cardAction(show.ref, "run", { ...run.draft, worktree, dispatch: run.dispatch }))) setRun(null);
  };
  const openCode = async () => {
    if (!target) return;
    const command = `nebula open --session=${target.id}`;
    const ok = await act("open", () => cardAction(show.ref, "open-code", { session: target.id, host: target.host }));
    setOpened({ ok, host: target.host, command });
  };
  const addNote = async () => {
    const sent = note.text;
    if (await act("note", () => cardAction(show.ref, "note", { text: sent.trim(), id: note.id }))) {
      setNote((now) => ({ text: now.text.startsWith(sent) ? now.text.slice(sent.length).trimStart() : now.text, id: ulid() }));
    }
  };
  const block = async () => {
    if (await act("block", () => cardAction(show.ref, "block", { by: blocker }))) setBlocker("");
  };

  return (
    <section aria-label="Acțiuni" className="flex flex-col gap-3 rounded-lg border bg-background px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" onClick={openRun} disabled={show.archived}><PlayIcon aria-hidden />Pornește</Button>
        <Button type="button" size="sm" variant="outline" onClick={openCode} disabled={!target || pending === "open"}>
          <CodeIcon aria-hidden />Deschide în cod
        </Button>
        <label className="ml-auto flex items-center gap-1.5 text-xs text-muted-foreground">
          Etapă
          <select value={show.stage} className={fieldCls} disabled={pending === "move"}
            onChange={(e) => act("move", () => cardAction(show.ref, "move", { stage: e.target.value as Stage, expect: show.stage }))}>
            {STAGES.map((s) => <option key={s} value={s}>{STAGE_LABEL[s]}</option>)}
          </select>
        </label>
      </div>

      {opened && (opened.ok
        ? <p role="status" className="text-sm">Deschis în cod pe <span className="font-mono">@{opened.host}</span>.</p>
        : (
          <div className="flex flex-col gap-1.5 text-sm">
            <p>Nebula nu răspunde pe <span className="font-mono">@{opened.host}</span>. Rulează comanda pe laptop:</p>
            <div className="flex min-w-0 items-center gap-2">
              <code className="min-w-0 flex-1 overflow-x-auto rounded-md border bg-card px-2 py-1.5 font-mono text-xs whitespace-nowrap">{opened.command}</code>
              <Button type="button" size="icon-sm" variant="outline" aria-label="Copiază comanda"
                onClick={() => navigator.clipboard?.writeText(opened.command).catch(() => undefined)}>
                <CopyIcon aria-hidden />
              </Button>
            </div>
          </div>
        ))}

      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <select aria-label="Blocat de cardul" value={blocker} onChange={(e) => setBlocker(e.target.value)} className={`${fieldCls} flex-1`}>
          <option value="">Alege cardul care îl blochează</option>
          {candidates.map((c) => <option key={c.id} value={c.ref}>{c.ref} {c.title}</option>)}
        </select>
        <Button type="button" size="sm" variant="outline" onClick={block} disabled={!blocker || pending === "block"}>Blochează</Button>
        {show.blocked_by.map((ref) => (
          <Button key={ref} type="button" size="sm" variant="ghost" disabled={pending === "unblock"}
            onClick={() => act("unblock", () => cardAction(show.ref, "unblock", { by: ref }))}>
            Deblochează <span className="font-mono">{ref}</span>
          </Button>
        ))}
      </div>

      <div className="flex min-w-0 flex-col gap-1.5">
        <textarea aria-label="Notă nouă" value={note.text} rows={2} placeholder="Scrie o notă în jurnal"
          onChange={(e) => setNote({ ...note, text: e.target.value })}
          className="min-w-0 rounded-lg border border-input bg-card px-2.5 py-2 text-base outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60 sm:text-sm" />
        <Button type="button" size="sm" variant="outline" className="self-end" onClick={addNote} disabled={!note.text.trim() || pending === "note"}>
          Adaugă nota
        </Button>
      </div>

      {error && <p role="alert" className="text-sm">{error}</p>}

      {run && (
        <RunDialog open onOpenChange={(open) => { if (!open) setRun(null); }} cardRef={show.ref} cardTitle={show.title} branch={branch}
          hosts={hosts} draft={run.draft} onDraftChange={(draft) => setRun({ ...run, draft })} onRun={startRun} pending={pending === "run"} />
      )}
    </section>
  );
}
