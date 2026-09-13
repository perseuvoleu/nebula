import { useCallback, useEffect, useRef, useState } from "react";
import { api, describeError } from "@/lib/api";
import { Button } from "./ui/button";

// Read the daemon's retained scrollback, not a guessed conversation transcript.
export function TerminalView({ project, session }: { project: string; session: string }) {
  const [output, setOutput] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [keyState, setKeyState] = useState<string | null>(null);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const keyBusy = useRef(false);
  const sendKey = async (key: string) => {
    if (keyBusy.current) return;
    keyBusy.current = true; setSending(true); setKeyError(null); setKeyState(`Se trimite ${key}…`);
    try {
      await api("key", {}, { project, id: session, key });
      setKeyState(`${key}: livrat către PTY`);
    } catch (failure) { setKeyState(null); setKeyError(describeError(failure)); }
    finally { keyBusy.current = false; setSending(false); }
  };
  const [limit, setLimit] = useState(250);
  const [hasMore, setHasMore] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const loadedLimit = useRef(250);
  const pre = useRef<HTMLPreElement>(null);
  const more = useCallback(() => {
    if (hasMore && !loadingOlder && limit < 10000) {
      setLoadingOlder(true);
      setLimit(value => Math.min(10000, value * 2));
    }
  }, [hasMore, loadingOlder, limit]);

  useEffect(() => {
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      if (document.hidden) { timer = setTimeout(tick, 2500); return; }
      try {
        const data = await api<{ output: string; hasMore: boolean }>("output", { project, id: session, lines: String(limit) }, undefined, { signal: abort.signal });
        if (abort.signal.aborted) return;
        const element = pre.current;
        const distance = element ? element.scrollHeight - element.scrollTop : 0;
        const follow = element ? distance - element.clientHeight < 80 : true;
        const expanding = limit > loadedLimit.current;
        loadedLimit.current = limit;
        setOutput(data.output); setHasMore(data.hasMore); setError(null); setLoadingOlder(false);
        if (element) requestAnimationFrame(() => {
          if (abort.signal.aborted) return;
          if (expanding) element.scrollTop = element.scrollHeight - distance;
          else if (follow) element.scrollTop = element.scrollHeight;
        });
      } catch (failure) {
        if (abort.signal.aborted) return;
        setError(describeError(failure)); setLoadingOlder(false);
      }
      timer = setTimeout(tick, 2500);
    };
    void tick();
    return () => { abort.abort(); if (timer) clearTimeout(timer); };
  }, [project, session, limit]);

  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label="Output de terminal">
      <div className="flex shrink-0 items-center justify-between gap-2 px-4 py-1 text-xs text-muted-foreground">
        {hasMore ? <Button type="button" size="xs" variant="ghost" disabled={loadingOlder} onClick={more}>
          {loadingOlder ? "Se încarcă…" : "Output mai vechi"}
        </Button> : <span title="Doar scrollback-ul păstrat de daemon; nu reconstruim mesaje din text">Istoric păstrat de terminal</span>}
        {error && <span role="alert" className="text-foreground">{error}</span>}
      </div>
      <div className="flex shrink-0 flex-wrap gap-1 px-3 py-2" role="group" aria-label="Taste terminal">
        {["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"].map(key =>
          <Button key={key} type="button" variant="outline" className="min-h-11 min-w-11" disabled={sending} onClick={() => void sendKey(key)}>{key}</Button>)}
        {keyState && <p role="status" className="w-full text-xs">{keyState}</p>}
        {keyError && <p role="alert" className="w-full text-xs">{keyError}</p>}
      </div>
      <pre ref={pre} tabIndex={0} aria-live="off" onScroll={event => { if (event.currentTarget.scrollTop < 80) more(); }}
        className="min-h-0 flex-1 overflow-auto bg-black px-4 py-3 font-mono text-[12.5px] leading-[1.45] whitespace-pre-wrap break-words text-foreground">
        {output ?? "Se încarcă outputul…"}
      </pre>
    </section>
  );
}
