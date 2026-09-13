import { useEffect, useRef, useState } from "react";
import { api, describeError } from "@/lib/api";

// Explicitly labelled terminal snapshot (last 250 rendered lines). Not chat history.
export function TerminalView({ project, session }: { project: string; session: string }) {
  const [output, setOutput] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pre = useRef<HTMLPreElement>(null);

  useEffect(() => {
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      if (document.hidden) { timer = setTimeout(tick, 2500); return; }
      try {
        const data = await api<{ output: string }>("output", { project, id: session }, undefined, { signal: abort.signal });
        if (abort.signal.aborted) return;
        const element = pre.current;
        const follow = element ? element.scrollHeight - element.scrollTop - element.clientHeight < 80 : true;
        setOutput(data.output);
        setError(null);
        if (follow && element) requestAnimationFrame(() => { element.scrollTop = element.scrollHeight; });
      } catch (failure) {
        if (abort.signal.aborted) return;
        setError(describeError(failure));
      }
      timer = setTimeout(tick, 2500);
    };
    void tick();
    return () => { abort.abort(); if (timer) clearTimeout(timer); };
  }, [project, session]);

  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label="Output de terminal">
      <p className="border-b border-border px-4 py-2 text-xs text-muted-foreground">
        Output de terminal, actualizat periodic. Nu este istoric de conversație.
        {error && <span role="alert" className="ml-2 text-foreground">{error}</span>}
      </p>
      <pre ref={pre} tabIndex={0} aria-live="off"
        className="min-h-0 flex-1 overflow-auto bg-black px-4 py-3 font-mono text-[12.5px] leading-[1.45] whitespace-pre-wrap break-words text-foreground">
        {output ?? "Se încarcă outputul…"}
      </pre>
    </section>
  );
}
