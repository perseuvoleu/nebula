import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api, describeError } from "@/lib/api";
import { Button } from "./ui/button";
import { Markdown } from "./Markdown";

const KEYS = [["Up", "↑"], ["Down", "↓"], ["Left", "←"], ["Right", "→"], ["Enter", "⏎"], ["Escape", "Esc"], ["Tab", "Tab"]] as const;

/** Display retained terminal output as readable Markdown or an exact raw snapshot.
 *  `children` (messages sent from this browser) scroll with the output, so phones get one view. */
export function TerminalView({ project, session, children, revealKey = 0 }: {
  project: string; session: string; children?: ReactNode;
  /** Changes when a message is sent, so the reader lands on it under the latest output. */
  revealKey?: number;
}) {
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
  const [reading, setReading] = useState(true);
  const scroller = useRef<HTMLDivElement>(null);
  const positions = useRef<{ reading: number | null; raw: number | null }>({ reading: null, raw: null });
  const changeView = (nextReading: boolean) => {
    const element = scroller.current;
    if (nextReading === reading || !element) return;
    const top = element.scrollTop;
    const follow = element.scrollHeight - top - element.clientHeight < 80;
    positions.current[reading ? "reading" : "raw"] = top;
    const target = positions.current[nextReading ? "reading" : "raw"] ?? top;
    setReading(nextReading);
    requestAnimationFrame(() => {
      if (scroller.current === element) element.scrollTop = follow ? element.scrollHeight : target;
    });
  };
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
        const element = scroller.current;
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

  useEffect(() => {
    const element = scroller.current;
    if (revealKey && element) requestAnimationFrame(() => { element.scrollTop = element.scrollHeight; });
  }, [revealKey]);

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col" aria-label="Output de terminal">
      <div className="flex shrink-0 flex-wrap items-center gap-1 border-b border-border px-2 py-1">
        <div className="flex shrink-0 gap-1" role="group" aria-label="Afișare output">
          <Button type="button" variant="ghost" size="sm" className="min-h-9" aria-pressed={reading} onClick={() => changeView(true)}>Lectură</Button>
          <Button type="button" variant="ghost" size="sm" className="min-h-9" aria-pressed={!reading} onClick={() => changeView(false)}>Terminal brut</Button>
        </div>
        <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Taste terminal">
          {KEYS.map(([key, glyph]) =>
            <Button key={key} type="button" variant="outline" size="sm" className="min-h-9 min-w-9 px-2" aria-label={key} title={key} disabled={sending} onClick={() => void sendKey(key)}>{glyph}</Button>)}
          {keyState && <p role="status" className="px-1 text-xs text-muted-foreground">{keyState}</p>}
          {keyError && <p role="alert" className="px-1 text-xs">{keyError}</p>}
        </div>
      </div>
      {error && <p role="alert" className="shrink-0 px-4 py-1 text-xs">{error}</p>}
      <div ref={scroller} aria-label="Conținut terminal" tabIndex={0} aria-live="off" onScroll={event => { if (event.currentTarget.scrollTop < 80) more(); }}
        className="min-h-0 min-w-0 flex-1 overflow-auto bg-black px-3 py-2 text-foreground">
        <div className="mb-2 flex justify-center text-xs text-muted-foreground">
          {hasMore ? <Button type="button" size="xs" variant="ghost" disabled={loadingOlder} onClick={more}>
            {loadingOlder ? "Se încarcă…" : "Output mai vechi"}
          </Button> : <span title="Doar scrollback-ul păstrat de daemon; nu reconstruim mesaje din text">Istoric păstrat de terminal</span>}
        </div>
        {reading ? <Markdown text={output ?? "Se încarcă outputul…"} /> :
          <pre className="font-mono text-[12.5px] leading-[1.45] whitespace-pre-wrap break-words">{output ?? "Se încarcă outputul…"}</pre>}
        {children}
      </div>
    </section>
  );
}
