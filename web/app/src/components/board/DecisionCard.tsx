import { formatWhen } from "@/lib/status";
import { Pill } from "@/components/ui/pill";

export interface DecisionRow {
  id: string;
  title: string;
  why: string | null;
  project: string | null;
  createdAt: string;
  /** href is null when this gateway has no page for the session's host. */
  session: { name: string; host: string; href: string | null } | null;
  card: { ref: string; title: string; href: string } | null;
}

const linkCls = "min-w-0 truncate underline-offset-4 hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm";

export function DecisionCard({ decision, showProject = true }: { decision: DecisionRow; showProject?: boolean }) {
  return (
    <article className="flex flex-col gap-[5px] rounded-md border border-l-[3px] border-l-special bg-bg1 px-3 py-2.5">
      <div className="flex items-baseline gap-3">
        <h3 className="flex min-w-0 flex-1 items-baseline gap-1.5 text-sm font-semibold break-words"><span aria-hidden className="size-2 shrink-0 translate-y-[-1px] rotate-45 rounded-[1px] bg-special" />{decision.title}</h3>
        <time dateTime={decision.createdAt} className="shrink-0 font-mono text-[11px] text-muted-foreground tabular-nums">{formatWhen(decision.createdAt)}</time>
      </div>
      {decision.why && <p className="text-sm text-muted-foreground">{decision.why}</p>}
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {showProject && <Pill tone="project">{decision.project ?? "fără proiect"}</Pill>}
        {decision.card
          ? <a href={decision.card.href} className={linkCls}><span className="font-mono text-muted-foreground">{decision.card.ref}</span> {decision.card.title}</a>
          : <span>la nivel de proiect</span>}
        {!decision.session ? <span>de tine</span>
          : decision.session.href ? <a href={decision.session.href} className={linkCls}>sesiunea <span className="font-mono">{decision.session.name}</span></a>
          : <span>sesiunea <span className="font-mono">{decision.session.name} @{decision.session.host}</span></span>}
      </div>
    </article>
  );
}
