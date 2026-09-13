// Mounted with key={project/session} by the shell, so draft/terminal state resets per session without effects.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeftIcon, RefreshCwIcon, TerminalIcon } from "lucide-react";
import { useStickToBottomContext } from "use-stick-to-bottom";
import { Conversation, ConversationContent, ConversationScrollButton } from "@/components/ai-elements/conversation";
import { Button } from "@/components/ui/button";
import type { Attempt, ConversationController, ConversationState } from "@/lib/conversation";
import { draftKey, getDraft, setDraft } from "@/lib/drafts";
import { formatTime, kindLabel, statusLabel } from "@/lib/status";
import type { SessionRow } from "@/lib/types";
import { Composer } from "./Composer";
import { MessageItem } from "./MessageItem";
import { TerminalView } from "./TerminalView";
import { SessionTabs } from "./SessionTabs";

const ATTEMPT_LABEL: Record<Attempt["state"], string> = {
  sending: "Se trimite…",
  accepted: "Acceptat de Nebula · așteaptă confirmarea în istoric",
  uncertain: "Livrare incertă",
  failed: "Trimiterea a eșuat",
};

function PendingAttempt({ attempt, onDismiss, onRetry, snapshotOnly = false }: { attempt: Attempt; onDismiss: () => void; onRetry: () => void; snapshotOnly?: boolean }) {
  return (
    <article className="flex flex-col items-end gap-1" data-attempt={attempt.state} aria-live="polite">
      <div className="max-w-[min(85%,72ch)] rounded-2xl rounded-br-md border border-dashed border-border bg-white px-4 py-2.5 text-[15px] leading-relaxed whitespace-pre-wrap break-words text-black">
        {attempt.text}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2 px-1 text-[11px] text-muted-foreground">
        <span>{snapshotOnly && attempt.state === "accepted" ? "Trimis către sesiune · din acest browser" : ATTEMPT_LABEL[attempt.state]}{attempt.error ? ` · ${attempt.error}` : ""}</span>
        {snapshotOnly && attempt.state === "accepted" && <Button type="button" size="xs" variant="ghost" onClick={onDismiss}>Ascunde</Button>}
        {(attempt.state === "uncertain" || attempt.state === "failed") && (
          <>
            {attempt.state === "uncertain" && <span>Verifică istoricul înainte de a retrimite.</span>}
            <Button type="button" size="xs" variant="outline" onClick={onRetry}>Retrimite</Button>
            <Button type="button" size="xs" variant="ghost" onClick={onDismiss}>Renunță</Button>
          </>
        )}
      </div>
    </article>
  );
}

/** Prepends older history while keeping the reader's position anchored to the same message. */
function LoadOlder({ controller, loading }: { controller: ConversationController; loading: boolean }) {
  const { scrollRef } = useStickToBottomContext();
  const sentinel = useRef<HTMLDivElement>(null);
  const inFlight = useRef(false);
  const load = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    const element = scrollRef.current;
    const before = element ? element.scrollHeight - element.scrollTop : 0;
    await controller.loadOlder();
    requestAnimationFrame(() => {
      if (element) element.scrollTop = element.scrollHeight - before;
      inFlight.current = false;
    });
  }, [controller, scrollRef]);
  useEffect(() => {
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) void load();
    }, { root: scrollRef.current, rootMargin: "80px 0px 0px 0px" });
    if (sentinel.current) observer.observe(sentinel.current);
    return () => observer.disconnect();
  }, [load, scrollRef]);
  return (
    <div ref={sentinel} className="flex justify-center">
      <Button type="button" variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
        {loading ? "Se încarcă…" : "Încarcă mesaje mai vechi"}
      </Button>
    </div>
  );
}

function JumpToLatest({ generation }: { generation: number }) {
  const { scrollToBottom } = useStickToBottomContext();
  useEffect(() => { void scrollToBottom({ animation: "instant" }); }, [generation, scrollToBottom]);
  return <ConversationScrollButton aria-label="Sari la ultimul mesaj" className="h-10 gap-1.5 px-4 text-sm shadow-none">Mesaje noi</ConversationScrollButton>;
}

function ConnectionLine({ state }: { state: ConversationState }) {
  const session = state.session;
  const working = session?.status === "running";
  let text: string;
  if (state.connection === "offline") text = "Fără conexiune · conținutul poate fi vechi; agentul continuă pe server";
  else if (state.connection === "stale") text = `Reconectare… ${state.error ?? ""}`.trim();
  else if (working) text = state.capabilities?.liveText ? "Agentul scrie…" : "Agentul lucrează · răspunsul apare când se încheie fiecare mesaj";
  else text = `Sincronizat ${formatTime(state.lastSync)}`;
  return (
    <p role="status" aria-live="polite" className="flex min-h-7 items-center gap-2 px-4 text-[12px] text-muted-foreground">
      {working && state.connection === "live" && <span aria-hidden className="inline-block size-1.5 animate-pulse rounded-full bg-foreground" />}
      <span className="truncate">{text}</span>
    </p>
  );
}

export function ConversationView({ project, row, state, controller, onBack, showBack, sessions, onSelect }: {
  project: string;
  row: SessionRow | null;
  state: ConversationState;
  controller: ConversationController;
  onBack: () => void;
  showBack: boolean;
  sessions: SessionRow[];
  onSelect: (row: SessionRow) => void;
}) {
  const session = state.session ?? row;
  const sessionId = controller.sessionId;
  const key = draftKey(project, sessionId);
  const [draft, setDraftState] = useState(() => getDraft(key));
  const [terminal, setTerminal] = useState(false);
  const structured = state.phase === "ready";
  const showTerminal = terminal || state.phase === "unsupported";
  const sending = state.attempts.some((a) => a.state === "sending");

  const onDraft = (value: string) => { setDraftState(value); setDraft(key, value); };
  const send = async (text: string) => {
    onDraft("");
    const attempt = await controller.send(text);
    if (attempt.state === "failed" && !getDraft(key)) onDraft(text);
  };
  const retry = (attempt: Attempt) => { controller.dismissAttempt(attempt.id); void send(attempt.text); };

  const title = state.name || session?.name || "Sesiune";
  const list = useMemo(() => state.messages.map((m) => (
    <MessageItem key={m.id} message={m} project={project} session={sessionId} />
  )), [state.messages, project, sessionId]);

  return (
    <section id="conversation-panel" role="tabpanel" className="flex min-h-0 min-w-0 flex-1 flex-col" aria-label={`Conversație: ${title}`}>
      <header className="flex min-h-14 items-center gap-2 border-b border-border px-2 sm:px-4">
        {showBack && (
          <Button type="button" variant="ghost" size="icon-lg" aria-label="Înapoi la sesiuni" onClick={onBack} className="size-11">
            <ArrowLeftIcon aria-hidden className="size-5" />
          </Button>
        )}
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-[15px] font-semibold leading-tight">{title}</h1>
          {session && (
            <p className="truncate text-xs text-muted-foreground">
              {kindLabel(session.kind)} · <span className="font-mono">{session.worktree}</span> · {statusLabel(session.status)}
              {showTerminal && <span title={state.capabilities?.reason ?? "Output brut al sesiunii, nu istoric de chat"}> · Terminal</span>}
            </p>
          )}
        </div>
        {state.capabilities?.structured && (
          <Button type="button" variant={terminal ? "secondary" : "ghost"} size="icon-lg" aria-pressed={terminal}
            aria-label={terminal ? "Arată conversația" : "Arată terminalul"} onClick={() => setTerminal((v) => !v)} className="size-11">
            <TerminalIcon aria-hidden className="size-5" />
          </Button>
        )}
      </header>
      <SessionTabs sessions={sessions} selected={sessionId} onSelect={onSelect} />

      {state.phase === "loading" && (
        <div className="flex-1 space-y-4 px-4 py-6" aria-busy="true" aria-label="Se încarcă conversația">
          <div className="ml-auto h-10 w-2/3 animate-pulse rounded-2xl bg-muted/60" />
          <div className="h-24 w-full max-w-[72ch] animate-pulse rounded-lg bg-muted/40" />
          <div className="ml-auto h-10 w-1/2 animate-pulse rounded-2xl bg-muted/60" />
        </div>
      )}

      {state.phase === "error" && (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
          <p role="alert" className="text-sm">{state.error}</p>
          <Button type="button" variant="outline" onClick={() => void controller.load()}><RefreshCwIcon aria-hidden /> Reîncearcă</Button>
        </div>
      )}

      {state.phase === "unsupported" && session?.kind === "pi" && (
        <p role="status" className="border-b border-border px-4 py-2 text-xs text-muted-foreground">
          Conversație structurată indisponibilă: {state.capabilities?.reason}
        </p>
      )}

      {showTerminal && state.phase !== "loading" && state.phase !== "error" && <TerminalView project={project} session={sessionId} />}

      {structured && !showTerminal && (
        <Conversation className="min-h-0 flex-1" initial="instant" resize="instant">
          <ConversationContent className="mx-auto w-full max-w-[86ch] gap-5 px-4 py-4 sm:px-6">
            {state.hasMore && <LoadOlder controller={controller} loading={state.loadingOlder} />}
            {state.messages.length === 0 && state.attempts.length === 0 && (
              <p className="py-10 text-center text-sm text-muted-foreground">Sesiunea nu are încă mesaje persistate. Pi scrie istoricul după primul răspuns.</p>
            )}
            {list}
            {state.attempts.map((attempt) => (
              <PendingAttempt key={attempt.id} attempt={attempt} onDismiss={() => controller.dismissAttempt(attempt.id)} onRetry={() => retry(attempt)} />
            ))}
          </ConversationContent>
          <JumpToLatest generation={state.generation} />
        </Conversation>
      )}

      {showTerminal && state.attempts.length > 0 && (
        <div className="max-h-40 shrink-0 space-y-3 overflow-y-auto border-t border-border px-4 py-3" aria-label="Mesaje trimise din acest browser">
          {state.attempts.map(attempt => <PendingAttempt key={attempt.id} attempt={attempt} snapshotOnly
            onDismiss={() => controller.dismissAttempt(attempt.id)} onRetry={() => retry(attempt)} />)}
        </div>
      )}
      {state.phase !== "loading" && state.phase !== "error" && (
        <>
          <ConnectionLine state={state} />
          <Composer draft={draft} onDraft={onDraft} onSend={(text) => void send(text)} busy={sending}
            disabled={!state.capabilities?.send || state.connection === "offline"} autoFocus={!showBack} />
        </>
      )}
    </section>
  );
}
