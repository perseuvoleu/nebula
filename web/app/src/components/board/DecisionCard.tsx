export interface DecisionRow {
  id: string;
  title: string;
  why: string | null;
  project: string | null;
  createdAt: string;
  session: { name: string; href: string } | null;
  card: { ref: string; title: string; href: string } | null;
}

export function formatWhen(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString("ro-RO", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

const linkCls = "min-w-0 truncate underline-offset-4 hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm";

export function DecisionCard({ decision, showProject = true }: { decision: DecisionRow; showProject?: boolean }) {
  return (
    <article className="flex flex-col gap-1.5 rounded-lg border border-border border-l-2 border-l-foreground/50 bg-card px-3 py-2.5">
      <div className="flex items-baseline gap-3">
        <h3 className="min-w-0 flex-1 text-sm font-medium break-words">{decision.title}</h3>
        <time dateTime={decision.createdAt} className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{formatWhen(decision.createdAt)}</time>
      </div>
      {decision.why && <p className="text-sm text-muted-foreground">{decision.why}</p>}
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {showProject && <span className="font-mono">{decision.project ? `@${decision.project}` : "fără proiect"}</span>}
        {decision.card
          ? <a href={decision.card.href} className={linkCls}><span className="font-mono">{decision.card.ref}</span> {decision.card.title}</a>
          : <span>la nivel de proiect</span>}
        {decision.session
          ? <a href={decision.session.href} className={linkCls}>sesiunea <span className="font-mono">{decision.session.name}</span></a>
          : <span>de tine</span>}
      </div>
    </article>
  );
}
