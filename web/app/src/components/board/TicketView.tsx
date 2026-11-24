import { Markdown } from "@/components/Markdown";

export function TicketView({ markdown, source }: { markdown: string; source?: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-2">
      {source && <span className="truncate font-mono text-xs text-muted-foreground">{source}</span>}
      <Markdown text={markdown} />
    </div>
  );
}
