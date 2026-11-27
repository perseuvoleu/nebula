import { Fragment } from "react";
import { cn } from "@/lib/utils";
import type { HostSlots } from "@/lib/board";

export function WriterSlots({ hosts, className }: { hosts: HostSlots[]; className?: string }) {
  return (
    <p title="Sesiuni care scriu acum, pe fiecare host"
      className={cn("inline-flex shrink-0 items-center gap-1.5 rounded-full border bg-bg1 px-2 py-0.5 font-mono text-[11px] whitespace-nowrap text-muted-foreground tabular-nums", className)}>
      <span>scriitori</span>
      {hosts.map((h, i) => {
        const full = h.slots !== null && h.writers >= h.slots;
        return (
          <Fragment key={h.host}>
            {i > 0 && <span aria-hidden className="text-dim">·</span>}
            <span className={cn(full && "font-semibold text-err")}>
              {h.writers}{h.slots !== null && `/${h.slots}`} {h.host}{full && <span className="sr-only"> (plin)</span>}
            </span>
          </Fragment>
        );
      })}
    </p>
  );
}
