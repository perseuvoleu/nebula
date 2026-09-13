import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ArrowUpIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

export const MAX_MESSAGE = 16000;

// Desktop: Enter sends, Shift+Enter inserts a newline. Coarse pointers (phones/tablets)
// keep Enter as newline and rely on the explicit send button. IME composition never sends.
const coarse = () => window.matchMedia("(pointer: coarse)").matches;

export function Composer({ draft, onDraft, onSend, disabled, busy, placeholder, autoFocus }: {
  draft: string;
  onDraft: (value: string) => void;
  onSend: (text: string) => void;
  disabled?: boolean;
  busy?: boolean;
  placeholder?: string;
  autoFocus?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [touch] = useState(coarse);
  const canSend = !disabled && !busy && draft.trim().length > 0 && draft.length <= MAX_MESSAGE;

  useEffect(() => {
    if (autoFocus && !touch) ref.current?.focus();
  }, [autoFocus, touch]);

  const submit = () => {
    if (!canSend) return;
    onSend(draft);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || touch) return;
    event.preventDefault();
    submit();
  };

  return (
    <form
      className="flex items-end gap-2 border-t border-border bg-background px-3 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]"
      onSubmit={(event) => { event.preventDefault(); submit(); }}
      aria-label="Trimite un mesaj agentului"
    >
      <label className="sr-only" htmlFor="composer">Mesaj către agent</label>
      <textarea
        id="composer"
        ref={ref}
        value={draft}
        onChange={(event) => onDraft(event.target.value)}
        onKeyDown={onKeyDown}
        disabled={disabled}
        rows={1}
        maxLength={MAX_MESSAGE}
        placeholder={placeholder ?? (touch ? "Scrie un mesaj…" : "Scrie un mesaj… (Enter trimite, Shift+Enter rând nou)")}
        enterKeyHint={touch ? "enter" : "send"}
        autoCapitalize="sentences"
        className="field-sizing-content max-h-40 min-h-11 w-full flex-1 resize-none rounded-xl border border-input bg-white px-3.5 py-2.5 text-base leading-relaxed text-black outline-none placeholder:text-neutral-500 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60 disabled:opacity-60"
      />
      <Button type="submit" size="icon-lg" aria-label="Trimite" disabled={!canSend} className="size-11 shrink-0 rounded-full">
        <ArrowUpIcon aria-hidden className="size-5" />
      </Button>
    </form>
  );
}
