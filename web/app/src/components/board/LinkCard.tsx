import { ExternalLinkIcon, LinkIcon } from "lucide-react";

function hostOf(url: string): string {
  try { return new URL(url).host; } catch { return url; }
}

export function LinkCard({ url, title }: { url: string; title?: string }) {
  return (
    <a href={url} target="_blank" rel="noopener noreferrer nofollow"
      className="flex min-w-0 items-center gap-3 rounded-lg border border-border bg-card px-3 py-2 transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
      <LinkIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-sm font-medium">{title || hostOf(url)}</span>
        <span className="truncate font-mono text-xs text-muted-foreground">{url}</span>
      </span>
      <ExternalLinkIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
    </a>
  );
}
