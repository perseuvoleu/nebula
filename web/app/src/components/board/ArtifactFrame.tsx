import { ExternalLinkIcon, ServerIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/Markdown";
import { E2eRunView, type E2eRun } from "./E2eRunView";
import { TicketView } from "./TicketView";
import { LinkCard } from "./LinkCard";
import { formatWhen } from "./DecisionCard";

export interface ArtifactMeta {
  id: string;
  kind: string;
  title: string;
  session: string | null;
  createdAt: string;
}

export type ArtifactContent =
  | { view: "html"; src: string }
  | { view: "e2e-run"; run: E2eRun; assetUrl: (path: string) => string }
  | { view: "ticket"; markdown: string; source?: string }
  | { view: "link"; url: string }
  | { view: "decision"; why: string | null }
  | { view: "remote"; host: string };

function Body({ artifact, content }: { artifact: ArtifactMeta; content: ArtifactContent }) {
  switch (content.view) {
    case "html":
      return (
        <iframe src={content.src} title={artifact.title} sandbox="allow-scripts" referrerPolicy="no-referrer" loading="lazy"
          className="block h-[70dvh] min-h-80 w-full bg-background" />
      );
    case "e2e-run":
      return <div className="p-3"><E2eRunView run={content.run} assetUrl={content.assetUrl} /></div>;
    case "ticket":
      return <div className="p-3"><TicketView markdown={content.markdown} source={content.source} /></div>;
    case "link":
      return <div className="p-3"><LinkCard url={content.url} title={artifact.title} /></div>;
    case "decision":
      return <p className="p-3 text-sm text-muted-foreground">{content.why ?? "Fără motiv notat."}</p>;
    case "remote": {
      const command = `nebula artifact open ${artifact.id}`;
      return (
        <div className="flex flex-col gap-2 p-3 text-sm">
          <p className="flex items-center gap-2">
            <ServerIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <span>Stocat pe <span className="font-mono">@{content.host}</span>; se deschide acolo.</span>
          </p>
          <div className="flex min-w-0 items-center gap-1 rounded-lg border border-border bg-background py-1 pr-1 pl-3">
            <code className="min-w-0 flex-1 truncate font-mono text-xs">{command}</code>
            <CopyButton text={command} label="Copiază comanda" />
          </div>
        </div>
      );
    }
  }
}

export function ArtifactFrame({ artifact, content, onClose }: {
  artifact: ArtifactMeta;
  content: ArtifactContent;
  onClose?: () => void;
}) {
  return (
    <section aria-label={artifact.title} className="overflow-hidden rounded-lg border border-border bg-card">
      <header className="flex min-w-0 items-center gap-2 border-b border-border bg-muted/50 py-1.5 pr-1.5 pl-3">
        <span className="min-w-0 truncate text-sm font-medium">{artifact.title}</span>
        <span className="hidden min-w-0 truncate font-mono text-xs text-muted-foreground sm:inline">
          {[artifact.kind, artifact.session, formatWhen(artifact.createdAt)].filter(Boolean).join(" · ")}
        </span>
        <span className="flex-1" />
        {content.view === "html" && (
          <Button asChild variant="ghost" size="icon-sm">
            <a href={content.src} target="_blank" rel="noopener noreferrer" aria-label="Deschide separat"><ExternalLinkIcon aria-hidden /></a>
          </Button>
        )}
        {onClose && (
          <Button type="button" variant="ghost" size="icon-sm" aria-label="Închide" onClick={onClose}><XIcon aria-hidden /></Button>
        )}
      </header>
      <Body artifact={artifact} content={content} />
    </section>
  );
}
