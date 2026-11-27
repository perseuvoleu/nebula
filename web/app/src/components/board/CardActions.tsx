import { useRef, useState } from "react";
import { CodeIcon, CopyIcon, PlayIcon, XIcon } from "lucide-react";
import { describeError } from "@/lib/api";
import { type OpenDialog, type Target, focusBack, openCode, rememberedLaptop, resultFor, useOpenCode } from "@/lib/open-code";
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
    openCode.invalidate();
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
export function OpenCodeButton({ cardRef, cardId = null, target, resolve, hosts, size = "sm", label, compact = false }: {
  cardRef: string; cardId?: string | null; target?: Target | null; resolve?: () => Promise<Target | null>; hosts: string[]; size?: "sm" | "xs"; label?: string; compact?: boolean;
}) {
  const { pending } = useOpenCode();
  const [looking, setLooking] = useState(false);
  const [lookupError, setLookupError] = useState<string | null>(null);
  const busy = looking || pending.includes(cardRef);
  const unavailable = !target && !resolve;
  const pick = async (opener: HTMLElement) => {
    if (busy || unavailable) return;
    const token = openCode.begin();
    const scope = opener.closest("[data-open-scope]")?.getAttribute("data-open-scope") ?? null;
    setLookupError(null);
    let session = target ?? null;
    let failed: string | null = null;
    if (!session && resolve) {
      setLooking(true);
      try {
        session = await resolve();
        if (!session) failed = "Cardul nu are încă nicio sesiune.";
      } catch (failure) {
        failed = describeError(failure);
      } finally {
        setLooking(false);
      }
    }
    if (!openCode.current(token)) return;
    if (failed) {
      if (opener.isConnected) setLookupError(failed);
      else {
        openCode.report(cardRef, cardId, failed);
        focusBack(null, label ?? null, scope, () => document.getElementById("board-main"));
      }
      return;
    }
    const on = rememberedLaptop() ?? hosts.find((h) => h !== DEFAULT_HOST) ?? hosts[0];
    if (session && on) openCode.open(token, { cardRef, cardId, target: session, on, hosts, opener, label: label ?? null, scope });
  };
  return (
    <>
      <Button type="button" size={size} variant="hud" aria-label={label} aria-disabled={busy || unavailable || undefined} aria-busy={busy || undefined}
        onClick={(e) => void pick(e.currentTarget)} className={cn((busy || unavailable) && "opacity-50")}>
        {size !== "xs" && <CodeIcon aria-hidden />}Deschide în cod
      </Button>
      {lookupError && (compact
        ? <span role="alert" title={lookupError} className="max-w-full basis-full truncate text-right text-[11px] text-err">{lookupError}</span>
        : <p role="alert" className="basis-full text-sm text-err">{lookupError}</p>)}
    </>
  );
}

/** The card page's full result, keyed by the route's card (slug or id) so it shows while the card read loads or fails. */
export function OpenCodeResult({ cardKey }: { cardKey: string }) {
  const result = useOpenCode().results.find((r) => resultFor(r, cardKey));
  if (!result) return null;
  if (result.error) return <p role="alert" className="text-sm text-err">{result.error}</p>;
  if (result.command === null) return (
    <p role="status" className="flex items-center gap-2 text-sm">
      <span className="flex-1">Deschis în cod pe <span className="font-mono">@{result.on}</span>.</span>
      <Button type="button" variant="ghost" size="icon-xs" aria-label="Închide mesajul" onClick={() => openCode.dismiss(result.cardRef)}><XIcon aria-hidden /></Button>
    </p>
  );
  const command = result.command;
  return (
    <div className="flex flex-col gap-1.5 rounded-md border border-line-warn bg-bg0 px-[11px] py-[9px] text-sm">
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

/** Mounted once per page; fallback takes focus when neither the opener nor its replacement is on the page. */
export function OpenCodeDialog({ fallback }: { fallback: () => HTMLElement | null }) {
  const { dialog } = useOpenCode();
  const [last, setLast] = useState<OpenDialog | null>(null);
  if (dialog && dialog !== last) setLast(dialog);
  const shown = dialog ?? last;
  const remembered = rememberedLaptop();
  const laptops = [...new Set([...(shown?.hosts ?? []), ...(remembered ? [remembered] : []), ...(shown ? [shown.on] : [])])];
  return (
    <Dialog open={dialog !== null} onOpenChange={(next) => { if (!next) openCode.cancel(); }}>
      <DialogContent className="sm:max-w-sm" onCloseAutoFocus={(e) => {
        e.preventDefault();
        focusBack(shown?.opener ?? null, shown?.label ?? null, shown?.scope ?? null, fallback, true);
      }}>
        <DialogHeader>
          <DialogTitle>Deschide în cod <span className="font-mono">{shown?.cardRef}</span></DialogTitle>
          <DialogDescription>Laptopul pe care TUI-ul deschide sesiunea.</DialogDescription>
        </DialogHeader>
        <label className="flex flex-col gap-1.5 text-sm">
          <span className="font-medium">Laptop</span>
          <select value={shown?.on ?? ""} onChange={(e) => openCode.choose(e.target.value)} className={`${fieldCls} font-mono`}>
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

const plainClick = (e: MouseEvent) => e.button === 0 && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey;

/**
 * One line per result, mounted empty so it is announced. The board's line skips the cards shown elsewhere (exclude);
 * a peek sheet passes only, so its line sits inside the sheet where it can be reached.
 */
export function OpenCodeToast({ id, only, exclude = [], cardHref, onCardPage }: {
  id?: string; only?: string; exclude?: (string | null)[]; cardHref: (ref: string) => string; onCardPage: (ref: string) => void;
}) {
  const { results } = useOpenCode();
  const result = results.filter((r) => (only === undefined || resultFor(r, only)) && !exclude.some((key) => resultFor(r, key))).at(-1);
  const tone = !result ? "" : result.error ? "text-err" : result.command ? "text-warn" : "text-ok";
  return (
    <div id={id} {...(id ? { role: "status" } : { "aria-live": "polite" as const })} className="shrink-0">
      {result && (
        <div className={cn("flex items-center gap-2 border-b bg-bg1 px-3.5 py-1 text-xs", tone)}>
          <span className="min-w-0 flex-1 truncate">
            <span className="font-mono">{result.cardRef}</span>{" · "}
            {result.error ? result.error : result.command === null ? <>Deschis pe <span className="font-mono">@{result.on}</span></> : (
              <>@{result.on} nu răspunde: <a href={cardHref(result.cardRef)}
                onClick={(e) => { if (plainClick(e.nativeEvent)) { e.preventDefault(); onCardPage(result.cardRef); } }}
                className="underline underline-offset-2">comanda, pe pagina cardului</a></>
            )}
          </span>
          <Button type="button" variant="ghost" size="icon-xs" aria-label="Închide mesajul" onClick={() => openCode.dismiss(result.cardRef)}><XIcon aria-hidden /></Button>
        </div>
      )}
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
    <section aria-label="Acțiuni" data-open-scope="card" className="flex flex-col gap-3 rounded-md border bg-bg0 px-[11px] py-[9px]">
      <div className="flex flex-wrap items-center gap-2">
        <RunButton cardRef={show.ref} cardTitle={show.title} branch={branch} hosts={hosts} disabled={show.archived} />
        <OpenCodeButton cardRef={show.ref} cardId={show.id} target={target} hosts={hosts.map((h) => h.host)} />
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
