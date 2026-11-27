import { useState } from "react";
import { CodeIcon, CopyIcon, PlayIcon } from "lucide-react";
import { ApiError, describeError } from "@/lib/api";
import { type BoardData, type CardShow, STAGES, STAGE_LABEL, type Stage, knownHosts } from "@/lib/board";
import { cardAction, ulid } from "@/lib/board-actions";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import { RunDialog, type RunDraft, type RunHost } from "./RunDialog";

const DEFAULT_HOST = "leaseweb";
const LAPTOP = "nebula-open-laptop";

function rememberedLaptop(): string | null {
  try {
    return localStorage.getItem(LAPTOP);
  } catch {
    return null;
  }
}
const fieldCls = "field h-9 min-w-0 px-2 text-sm";

export function RunButton({ cardRef, cardTitle, branch, hosts, disabled, size = "sm", label }: {
  cardRef: string; cardTitle: string; branch: string | null; hosts: RunHost[]; disabled?: boolean; size?: "sm" | "xs"; label?: string;
}) {
  const [run, setRun] = useState<{ draft: RunDraft; dispatch: string } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = () => {
    setError(null);
    setRun({
      draft: { role: "worker", kind: "claude", host: hosts.some((h) => h.host === DEFAULT_HOST) ? DEFAULT_HOST : hosts[0]?.host ?? DEFAULT_HOST, worktree: "new" },
      dispatch: ulid(),
    });
  };
  const start = async () => {
    if (!run) return;
    const worktree = run.draft.worktree === "current" && branch ? branch : "new";
    setPending(true);
    setError(null);
    try {
      await cardAction(cardRef, "run", { ...run.draft, worktree, dispatch: run.dispatch });
      setRun(null);
    } catch (failure) {
      setError(describeError(failure));
    } finally {
      setPending(false);
    }
  };
  return (
    <>
      <Button type="button" size={size} variant={size === "xs" ? "outline" : "default"} aria-label={label} onClick={open} disabled={disabled}>
        {size !== "xs" && <PlayIcon aria-hidden />}Pornește
      </Button>
      {run && (
        <RunDialog open onOpenChange={(next) => { if (!next) setRun(null); }} cardRef={cardRef} cardTitle={cardTitle} branch={branch}
          hosts={hosts} draft={run.draft} onDraftChange={(draft) => setRun({ draft, dispatch: ulid() })} onRun={start} pending={pending}
          error={error} />
      )}
    </>
  );
}

export function OpenCodeButton({ cardRef, target, hosts, size = "sm", label }: {
  cardRef: string; target: { id: string; host: string } | null; hosts: string[]; size?: "sm" | "xs"; label?: string;
}) {
  const [opening, setOpening] = useState<string | null>(null);
  const [opened, setOpened] = useState<{ on: string; command: string | null } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remembered = rememberedLaptop();
  const laptops = remembered && !hosts.includes(remembered) ? [...hosts, remembered] : hosts;
  const pickLaptop = () => setOpening(remembered ?? hosts.find((h) => h !== DEFAULT_HOST) ?? hosts[0] ?? null);
  const openCode = async () => {
    const on = opening;
    if (!target || !on) return;
    try { localStorage.setItem(LAPTOP, on); } catch { /* Without storage the choice lasts this visit. */ }
    setOpening(null);
    setOpened(null);
    setPending(true);
    setError(null);
    try {
      await cardAction(cardRef, "open-code", { session: target.id, host: target.host, on });
      setOpened({ on, command: null });
    } catch (failure) {
      const command = failure instanceof ApiError && failure.status === 409 && typeof failure.data.command === "string" ? failure.data.command : null;
      if (command) setOpened({ on, command });
      else setError(describeError(failure));
    } finally {
      setPending(false);
    }
  };
  return (
    <>
      <Button type="button" size={size} variant="hud" aria-label={label} onClick={pickLaptop} disabled={!target || pending}>
        {size !== "xs" && <CodeIcon aria-hidden />}Deschide în cod
      </Button>
      {opened && (opened.command === null
        ? <p role="status" className="basis-full text-sm">Deschis în cod pe <span className="font-mono">@{opened.on}</span>.</p>
        : (
          <div className="flex basis-full flex-col gap-1.5 text-sm">
            <p>Nebula nu răspunde pe <span className="font-mono">@{opened.on}</span>. Rulează comanda pe laptop:</p>
            <div className="flex min-w-0 items-center gap-2">
              <code className="min-w-0 flex-1 overflow-x-auto rounded-sm border bg-bg0 px-2 py-1.5 font-mono text-xs whitespace-nowrap">{opened.command}</code>
              <Button type="button" size="icon-sm" variant="outline" aria-label="Copiază comanda"
                onClick={() => navigator.clipboard?.writeText(opened.command!).catch(() => undefined)}>
                <CopyIcon aria-hidden />
              </Button>
            </div>
          </div>
        ))}
      {error && <p role="alert" className="basis-full text-sm text-err">{error}</p>}
      <Dialog open={opening !== null} onOpenChange={(next) => { if (!next) setOpening(null); }}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Deschide în cod</DialogTitle>
            <DialogDescription>Laptopul pe care TUI-ul deschide sesiunea.</DialogDescription>
          </DialogHeader>
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Laptop</span>
            <select value={opening ?? ""} onChange={(e) => setOpening(e.target.value)} className={`${fieldCls} font-mono`}>
              {laptops.map((h) => <option key={h} value={h}>@{h}</option>)}
            </select>
          </label>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => setOpening(null)}>Renunță</Button>
            <Button type="button" onClick={openCode}>Deschide</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

export function CardActions({ show, board }: { show: CardShow; board: BoardData | null }) {
  const hosts = knownHosts(board, show.sessions.map((s) => s.host));
  const branch = show.checkout?.branch ?? null;
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
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
    <section aria-label="Acțiuni" className="flex flex-col gap-3 rounded-md border bg-bg0 px-[11px] py-[9px]">
      <div className="flex flex-wrap items-center gap-2">
        <RunButton cardRef={show.ref} cardTitle={show.title} branch={branch} hosts={hosts} disabled={show.archived} />
        <OpenCodeButton cardRef={show.ref} target={target ?? null} hosts={hosts.map((h) => h.host)} />
        <label className="ml-auto flex items-center gap-1.5 text-xs text-muted-foreground">
          Etapă
          <select value={show.stage} className={fieldCls} disabled={pending === "move"}
            onChange={(e) => act("move", () => cardAction(show.ref, "move", { stage: e.target.value as Stage, expect: show.stage }))}>
            {STAGES.map((s) => <option key={s} value={s}>{STAGE_LABEL[s]}</option>)}
          </select>
        </label>
      </div>

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
          className="field min-w-0 px-2.5 py-2 text-base sm:text-sm" />
        <Button type="button" size="sm" variant="outline" className="self-end" onClick={addNote} disabled={!note.text.trim() || pending === "note"}>
          Adaugă nota
        </Button>
      </div>

      {error && <p role="alert" className="text-sm text-err">{error}</p>}
    </section>
  );
}
