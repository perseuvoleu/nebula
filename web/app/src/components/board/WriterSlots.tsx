import { Fragment } from "react";
import { cn } from "@/lib/utils";

export type HostSlots = { host: string; writers: number; slots: number | null };

export function WriterSlots({ hosts }: { hosts: HostSlots[] }) {
  return (
    <p title="Sesiuni care scriu acum, pe fiecare host"
      className="inline-flex shrink-0 items-center gap-1.5 rounded-full border bg-card px-2.5 py-0.5 font-mono text-xs whitespace-nowrap text-muted-foreground tabular-nums">
      <span>scriitori</span>
      {hosts.map((h, i) => {
        const full = h.slots !== null && h.writers >= h.slots;
        return (
          <Fragment key={h.host}>
            {i > 0 && <span aria-hidden>·</span>}
            <span className={cn("text-foreground", full && "font-semibold underline underline-offset-2")}>
              {h.writers}{h.slots !== null && `/${h.slots}`} {h.host}{full && <span className="sr-only"> (plin)</span>}
            </span>
          </Fragment>
        );
      })}
    </p>
  );
}
