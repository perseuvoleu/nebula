import { Button } from "@/components/ui/button";

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
    <section aria-label="Carduri propuse" className="flex flex-col gap-2 border-b border-border px-4 py-3">
      <h2 className="text-sm font-medium">
        Propuse de agenți <span className="font-mono text-muted-foreground tabular-nums">{proposals.length}</span>
      </h2>
      <ul className="-mx-4 flex gap-2 overflow-x-auto px-4">
        {proposals.map((p) => (
          <li key={p.id} className="flex w-72 shrink-0 flex-col gap-2 rounded-lg border border-dashed border-input bg-card p-3">
            <button type="button" onClick={() => onOpen(p.id)}
              className="flex min-w-0 flex-col gap-1 rounded-md text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
              <span className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
                <span className="font-mono">{p.ref}</span>
                {p.project && <span className="truncate font-mono">@{p.project}</span>}
              </span>
              <span className="line-clamp-2 text-sm font-medium">{p.title}</span>
              <span className="truncate text-xs text-muted-foreground">propus de {p.proposedBy}</span>
            </button>
            <div className="flex gap-2">
              <Button type="button" variant="outline" size="sm" className="flex-1" onClick={() => onAccept(p.id)}>Acceptă</Button>
              <Button type="button" variant="ghost" size="sm" onClick={() => onDismiss(p.id)}>Respinge</Button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
