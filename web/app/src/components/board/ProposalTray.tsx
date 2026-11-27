import { CheckIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Pill } from "@/components/ui/pill";
import { ProjectTag } from "@/components/ui/tags";
import { CardRef } from "./CardTile";

export interface Proposal {
  id: string;
  ref: string;
  title: string;
  project: string | null;
  proposedBy: string;
}

export function ProposalTray({ proposals, onOpen, onAccept, onDismiss }: {
  proposals: Proposal[];
  onOpen: (id: string) => void;
  onAccept: (id: string) => void;
  onDismiss: (id: string) => void;
}) {
  if (proposals.length === 0) return null;
  return (
    <section aria-label="Carduri propuse" className="flex flex-col gap-2 border-b px-3.5 py-2.5">
      <h2 className="label-caps flex items-center gap-1.5 text-warn">
        Propuse de agenți <span className="font-mono tabular-nums">{proposals.length}</span>
      </h2>
      <ul className="-mx-3.5 flex gap-2 overflow-x-auto px-3.5">
        {proposals.map((p) => (
          <li key={p.id} className="flex w-72 shrink-0 flex-col gap-[5px] rounded-md border border-dashed border-line-warn bg-bg2 px-[9px] py-2">
            <button type="button" onClick={() => onOpen(p.id)}
              className="flex min-w-0 flex-col gap-[5px] rounded-sm text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
              <span className="flex min-w-0 items-center gap-1">
                <CardRef cardRef={p.ref} />
                {p.project && <ProjectTag project={p.project} />}
              </span>
              <span className="line-clamp-2 text-ui leading-[1.32] font-semibold">{p.title}</span>
              <Pill tone="proposed" className="self-start truncate">propus de {p.proposedBy}</Pill>
            </button>
            <div className="flex gap-1.5 border-t border-hair pt-1.5">
              <Button type="button" variant="ok" size="sm" className="flex-1" onClick={() => onAccept(p.id)}><CheckIcon aria-hidden />Acceptă</Button>
              <Button type="button" variant="ghost" size="sm" onClick={() => onDismiss(p.id)}>Respinge</Button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
