import { cn } from "@/lib/utils";
import { type Dot, DOT_LABEL } from "@/lib/status";

const DOT_CLASS: Record<Dot, string> = {
  needs: "bg-err",
  running: "bg-warn",
  done: "bg-ok",
  queued: "border-[1.5px] border-warn",
  off: "bg-dim",
};

/** The label carries the status as text too, so colour is never the only signal; null hides a decorative dot. */
export function StatusDot({ dot, label = DOT_LABEL[dot], className }: { dot: Dot; label?: string | null; className?: string }) {
  const classes = cn("inline-block size-[7px] shrink-0 rounded-full", DOT_CLASS[dot], className);
  if (label === null) return <span aria-hidden className={classes} />;
  return <span role="img" aria-label={label} title={label} className={classes} />;
}
