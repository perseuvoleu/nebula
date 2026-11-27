import { useRef, useState } from "react";
import { api, describeError } from "@/lib/api";
import { Button } from "./ui/button";

const KEYS = [["Up", "↑"], ["Down", "↓"], ["Left", "←"], ["Right", "→"], ["Enter", "⏎"], ["Escape", "Esc"], ["Tab", "Tab"]] as const;

/** Raw PTY keys, pinned above the composer in chat and terminal mode alike. */
export function TerminalKeys({ project, session }: { project: string; session: string }) {
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
  return (
    <div className="flex shrink-0 items-center gap-1 border-t bg-bg0 px-2 py-1" role="group" aria-label="Taste terminal">
      {KEYS.map(([key, glyph]) =>
        <Button key={key} type="button" variant="outline" size="sm" className="min-h-9 min-w-9 shrink-0 px-2 font-mono" aria-label={key} title={key} disabled={sending} onClick={() => void sendKey(key)}>{glyph}</Button>)}
      {keyState && <p role="status" className="min-w-0 truncate px-1 text-xs text-muted-foreground">{keyState}</p>}
      {keyError && <p role="alert" className="min-w-0 truncate px-1 text-xs" title={keyError}>{keyError}</p>}
    </div>
  );
}
