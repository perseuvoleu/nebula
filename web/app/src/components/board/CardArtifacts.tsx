import { useEffect, useState } from "react";
import type { Artifact } from "@/lib/board";
import { parseE2eRun } from "@/lib/e2e-run";
import { ArtifactFrame, type ArtifactContent, type ArtifactMeta } from "./ArtifactFrame";

const MARKDOWN = /\.(?:md|markdown)$/i;

function direct(artifact: Artifact): ArtifactContent | null {
  const content = artifact.content;
  if ("body" in content) return { view: "decision", why: content.body || null };
  if ("url" in content) return { view: "link", url: content.url };
  if (!artifact.src) return { view: "remote", host: content.host };
  if (artifact.kind === "e2e-run" || artifact.kind === "ticket" || MARKDOWN.test(content.entry)) return null;
  return { view: "html", src: artifact.src };
}

async function fetched(kind: string, entry: string, src: string, signal: AbortSignal): Promise<ArtifactContent> {
  const response = await fetch(src, { signal });
  if (!response.ok) throw new Error(response.status === 403 ? "Linkul a expirat; reîncarcă pagina." : "Artefactul nu poate fi citit acum.");
  if (kind !== "e2e-run") return { view: "ticket", markdown: await response.text(), source: entry };
  const root = src.split("/").slice(0, 4).join("/");
  return { view: "e2e-run", run: parseE2eRun(await response.json()),
    assetUrl: (path) => `${root}/${path.split("/").map(encodeURIComponent).join("/")}` };
}

function ArtifactItem({ artifact }: { artifact: Artifact }) {
  const immediate = direct(artifact);
  const src = immediate ? null : artifact.src ?? null;
  const entry = "entry" in artifact.content ? artifact.content.entry : "";
  const [loaded, setLoaded] = useState<{ src: string; content?: ArtifactContent; error?: string } | null>(null);
  useEffect(() => {
    if (!src) return;
    const abort = new AbortController();
    fetched(artifact.kind, entry, src, abort.signal)
      .then((content) => setLoaded({ src, content }))
      .catch((failure) => { if (!abort.signal.aborted) setLoaded({ src, error: failure instanceof Error ? failure.message : String(failure) }); });
    return () => abort.abort();
  }, [artifact.kind, entry, src]);
  const meta: ArtifactMeta = { id: artifact.id, kind: artifact.kind, title: artifact.title, session: artifact.session?.name ?? null,
    createdAt: new Date(artifact.created_at).toISOString() };
  const content = immediate ?? (loaded?.src === src ? loaded.content : undefined);
  if (content) return <ArtifactFrame artifact={meta} content={content} />;
  const error = loaded?.src === src ? loaded.error : undefined;
  return (
    <section aria-label={artifact.title} className="flex min-w-0 items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm">
      <span className="min-w-0 truncate font-medium">{artifact.title}</span>
      <span role={error ? "alert" : undefined} aria-busy={!error} className="text-muted-foreground">{error ?? "Se încarcă…"}</span>
    </section>
  );
}

export function CardArtifacts({ artifacts }: { artifacts: Artifact[] }) {
  return (
    <section aria-label="Artefacte" className="flex min-w-0 flex-col gap-2">
      <h3 className="text-xs font-medium text-muted-foreground">
        Artefacte<span className="ml-1.5 font-mono tabular-nums">{artifacts.length}</span>
      </h3>
      {artifacts.length
        ? artifacts.map((a) => <ArtifactItem key={a.id} artifact={a} />)
        : <p className="rounded-lg border bg-background px-3 py-2.5 text-sm text-muted-foreground">Niciun artefact încă.</p>}
    </section>
  );
}
