import { useRef, useState } from "react";
import { CodeIcon, CopyIcon, PlayIcon, XIcon } from "lucide-react";
import { describeError } from "@/lib/api";
import { type OpenResult, type Target, openCode, rememberedLaptop, useOpenCode } from "@/lib/open-code";
import { cn } from "@/lib/utils";
import { type BoardData, type CardShow, STAGES, STAGE_LABEL, type Stage, knownHosts, openTarget } from "@/lib/board";
import { cardAction, ulid } from "@/lib/board-actions";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import { RunDialog, type RunDraft, type RunHost } from "./RunDialog";

const DEFAULT_HOST = "leaseweb";
const fieldCls = "field h-9 min-w-0 px-2 text-sm";

export function RunButton({ cardRef, cardTitle, branch, hosts, disabled, size = "sm", label }: {
  cardRef: string; cardTitle: string; branch: string | null; hosts: RunHost[]; disabled?: boolean; size?: "sm" | "xs"; label?: string;
}) {
  const opener = useRef<HTMLButtonElement>(null);
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
  const close = () => {
    setRun(null);
    setTimeout(() => opener.current?.focus());
  };
  const start = async () => {
    if (!run) return;
    const worktree = run.draft.worktree === "current" && branch ? branch : "new";
    setPending(true);
    setError(null);
    try {
      await cardAction(cardRef, "run", { ...run.draft, worktree, dispatch: run.dispatch });
      close();
    } catch (failure) {
      setError(describeError(failure));
    } finally {
      setPending(false);
    }
  };
  return (
    <>
      <Button ref={opener} type="button" size={size} variant={size === "xs" ? "outline" : "default"} aria-label={label} onClick={open} disabled={disabled}>
        <PlayIcon aria-hidden />Pornește
      </Button>
      {run && (
        <RunDialog open onOpenChange={(next) => { if (!next) close(); }} cardRef={cardRef} cardTitle={cardTitle} branch={branch}
          hosts={hosts} draft={run.draft} onDraftChange={(draft) => setRun({ draft, dispatch: ulid() })} onRun={start} pending={pending}
          error={error} />
      )}
    </>
  );
}

/** The opener stays focusable while busy (aria-disabled), so focus can return to it after the dialog or a failed lookup. */
export function OpenCodeButton({ cardRef, target, resolve, hosts, size = "sm", label, compact = false }: {
  cardRef: string; target?: Target | null; resolve?: () => Promise<Target | null>; hosts: string[]; size?: "sm" | "xs"; label?: string; compact?: boolean;
}) {
  const { pending, results } = useOpenCode();
  const [looking, setLooking] = useState(false);
  const [lookupError, setLookupError] = useState<string | null>(null);
  const busy = looking || pending.includes(cardRef);
  const unavailable = !target && !resolve;
  const pick = async (opener: HTMLElement) => {
    if (busy || unavailable) return;
    setLookupError(null);
    let session = target ?? null;
    if (!session && resolve) {
      setLooking(true);
      try {
        session = await resolve();
        if (!session) setLookupError("Cardul nu are încă nicio sesiune.");
      } catch (failure) {
        setLookupError(describeError(failure));
      } finally {
        setLooking(false);
      }
    }
    const on = rememberedLaptop() ?? hosts.find((h) => h !== DEFAULT_HOST) ?? hosts[0];
    if (session && on) openCode.open({ cardRef, target: session, on, opener });
  };
  const result = compact ? null : results.find((r) => r.cardRef === cardRef);
  return (
    <>
      <Button type="button" size={size} variant="hud" aria-label={label} aria-disabled={busy || unavailable || undefined} aria-busy={busy || undefined}
        onClick={(e) => void pick(e.currentTarget)} className={cn((busy || unavailable) && "opacity-50")}>
        {size !== "xs" && <CodeIcon aria-hidden />}Deschide în cod
      </Button>
      {lookupError && (compact
        ? <span role="alert" title={lookupError} className="max-w-full basis-full truncate text-right text-[11px] text-err">{lookupError}</span>
        : <p role="alert" className="basis-full text-sm text-err">{lookupError}</p>)}
      {result && <OpenResultBlock result={result} />}
    </>
  );
}

function OpenResultBlock({ result }: { result: OpenResult }) {
  if (result.error) return <p role="alert" className="basis-full text-sm text-err">{result.error}</p>;
  if (result.command === null) return <p role="status" className="basis-full text-sm">Deschis în cod pe <span className="font-mono">@{result.on}</span>.</p>;
  const command = result.command;
  return (
    <div className="flex basis-full flex-col gap-1.5 text-sm">
      <p>Nebula nu răspunde pe <span className="font-mono">@{result.on}</span>. Rulează comanda pe laptop:</p>
      <div className="flex min-w-0 items-center gap-2">
        <code className="min-w-0 flex-1 overflow-x-auto rounded-sm border bg-bg0 px-2 py-1.5 font-mono text-xs whitespace-nowrap">{command}</code>
        <Button type="button" size="icon-sm" variant="outline" aria-label="Copiază comanda"
          onClick={() => navigator.clipboard?.writeText(command).catch(() => undefined)}>
          <CopyIcon aria-hidden />
        </Button>
      </div>
    </div>
  );
}

/** Mounted once per page; fallback takes focus when the opener left the page while the dialog was open. */
export function OpenCodeDialog({ hosts, fallback }: { hosts: string[]; fallback?: () => HTMLElement | null }) {
  const { dialog } = useOpenCode();
  const [opener, setOpener] = useState<HTMLElement | null>(null);
  if (dialog && dialog.opener !== opener) setOpener(dialog.opener);
  const remembered = rememberedLaptop();
  const laptops = [...new Set([...hosts, ...(remembered ? [remembered] : []), ...(dialog ? [dialog.on] : [])])];
  return (
    <Dialog open={dialog !== null} onOpenChange={(next) => { if (!next) openCode.cancel(); }}>
      <DialogContent className="sm:max-w-sm" onCloseAutoFocus={(e) => {
        e.preventDefault();
        (opener?.isConnected ? opener : fallback?.())?.focus();
      }}>
        <DialogHeader>
          <DialogTitle>Deschide în cod</DialogTitle>
          <DialogDescription>Laptopul pe care TUI-ul deschide sesiunea.</DialogDescription>
        </DialogHeader>
        <label className="flex flex-col gap-1.5 text-sm">
          <span className="font-medium">Laptop</span>
          <select value={dialog?.on ?? ""} onChange={(e) => openCode.choose(e.target.value)} className={`${fieldCls} font-mono`}>
            {laptops.map((h) => <option key={h} value={h}>@{h}</option>)}
          </select>
        </label>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={openCode.cancel}>Renunță</Button>
          <Button type="button" onClick={() => void openCode.confirm()}>Deschide</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** One line for results of tiles and the strip; the card page (shown) prints its own block instead. */
export function OpenCodeToast({ shown, cardHref, onCardPage }: { shown: string | null; cardHref: (ref: string) => string; onCardPage: (ref: string) => void }) {
  const { results } = useOpenCode();
  const result = results.filter((r) => r.cardRef !== shown).at(-1);
  if (!result) return null;
  const tone = result.error ? "text-err" : result.command ? "text-warn" : "text-ok";
  return (
    <div role="status" className={cn("flex shrink-0 items-center gap-2 border-b bg-bg1 px-3.5 py-1 text-xs", tone)}>
      <span className="min-w-0 flex-1 truncate">
        <span className="font-mono">{result.cardRef}</span>{" · "}
        {result.error ? result.error : result.command === null ? <>Deschis pe <span className="font-mono">@{result.on}</span></> : (
          <>@{result.on} nu răspunde: <a href={cardHref(result.cardRef)} onClick={(e) => { e.preventDefault(); onCardPage(result.cardRef); }}
            className="underline underline-offset-2">comanda, pe pagina cardului</a></>
        )}
      </span>
      <Button type="button" variant="ghost" size="icon-xs" aria-label="Închide mesajul" onClick={() => openCode.dismiss(result.cardRef)}><XIcon aria-hidden /></Button>
    </div>
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
  const target = openTarget(sessions);
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
        <OpenCodeButton cardRef={show.ref} target={target} hosts={hosts.map((h) => h.host)} />
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
