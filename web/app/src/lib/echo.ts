import type { Attempt } from "./conversation.ts";

const WORD = "[\\p{L}\\p{N}_]";
const SYNTAX = /[\\^$.*+?()[\]{}|/]/g;

/** The terminal renders one line per pane row, so an echo may break at any column and be re-indented; it may not sit inside a longer word. */
function echoes(text: string, output: string): number {
  const words = text.trim().split(/\s+/).filter(Boolean).map((word) => [...word].map((c) => c.replace(SYNTAX, "\\$&")).join("\\s*"));
  if (!words.length) return 0;
  return output.match(new RegExp(`(?<!${WORD})${words.join("\\s+")}(?!${WORD})`, "gu"))?.length ?? 0;
}

/** Returns an update that, given the attempts in send order and the retained terminal output (omitted: unchanged), names
 *  the accepted attempts whose own echo has appeared since they were sent. `widened` marks a read that only reached further back. */
export function echoWatch() {
  const baselines = new Map<string, number>();
  let shown: string | null = null;
  return (attempts: Pick<Attempt, "id" | "text" | "state">[], output: string | null = shown, widened = false): string[] => {
    const previous = shown;
    shown = output;
    for (const id of baselines.keys()) if (!attempts.some((a) => a.id === id)) baselines.delete(id);
    if (output === null) return [];
    const older = widened && previous !== null ? output.split("\n").slice(0, -previous.split("\n").length).join("\n") : "";
    const claimed = new Map<string, number>();
    const delivered: string[] = [];
    for (const attempt of attempts) {
      const key = attempt.text.trim().replace(/\s+/g, " ");
      const now = echoes(attempt.text, output);
      let baseline = baselines.get(attempt.id);
      if (baseline === undefined) baseline = Math.max(now, (claimed.get(key) ?? -1) + 1);
      else if (previous !== null && widened) baseline += echoes(attempt.text, older);
      else if (previous !== null) baseline = Math.max(0, baseline - Math.max(0, echoes(attempt.text, previous) - now));
      baselines.set(attempt.id, baseline);
      claimed.set(key, baseline);
      if (attempt.state === "accepted" && now > baseline) delivered.push(attempt.id);
    }
    return delivered;
  };
}
