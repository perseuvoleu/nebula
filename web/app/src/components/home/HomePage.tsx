import { useEffect, useState } from "react";
import { ArrowLeftIcon } from "lucide-react";
import { api } from "@/lib/api";
import { compactAge } from "@/lib/activity-age";
import { type Artifact, type Card, type CardSession, type CardShow, type NeedsYou, STAGES, STAGE_LABEL, type Stage, type Workspace, firstWait, openTarget, projectName, sessionProject } from "@/lib/board";
import type { Decision } from "@/lib/board-reads";
import { type HomeBoard, STAGE_LETTER, STAGE_TEXT, refHost } from "@/lib/home";
import { sessionDot, statusLabel } from "@/lib/status";
import type { ConversationResponse } from "@/lib/types";
import { cn } from "@/lib/utils";
import { StatusDot } from "@/components/ui/status-dot";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { ArtifactItem } from "@/components/board/CardArtifacts";

const ART_ICON: Record<string, [string, string]> = {
  recap: ["◆", "text-special"], "e2e-run": ["▣", "text-ok"], diagram: ["◇", "text-info"], ticket: ["▤", "text-warn"],
  decision: ["◆", "text-special"], prototype: ["◫", "text-primary"],
};

const rowCls = (selected: boolean) => cn(
  "flex h-[21px] w-full min-w-0 cursor-pointer items-center gap-[5px] border-l-2 px-1.5 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset max-md:h-11",
  selected ? "border-l-primary bg-sel2" : "border-l-transparent hover:bg-bg1",
);

function SecHead({ children }: { children: React.ReactNode }) {
  return <h2 className="flex items-center gap-[5px] px-1.5 pt-1 pb-0.5 text-[9.5px] tracking-[0.06em] whitespace-nowrap text-dim uppercase select-none">{children}</h2>;
}

function CardRow({ card, selected, needs, onSelect }: { card: Card; selected: boolean; needs: boolean; onSelect: () => void }) {
  const host = refHost(card.ref);
  return (
    <li>
      <button type="button" onClick={onSelect} aria-current={selected ? "true" : undefined} className={rowCls(selected)}>
        {needs ? <StatusDot dot="needs" className="size-1.5" /> : <span className="w-5 shrink-0 text-[9.5px] text-dim tabular-nums">#{card.number}</span>}
        <span className="min-w-0 flex-1 truncate text-foreground">{card.title}</span>
        {host ? <span className="max-w-[74px] shrink-0 truncate text-[9px] text-remote">@{host}</span>
          : <span className={cn("max-w-[74px] shrink-0 truncate text-[10px]", selected ? "text-muted-foreground" : "text-dim")}>{projectName(card.project)}</span>}
      </button>
    </li>
  );
}

function HomeLeft({ home, selected, onSelect }: { home: HomeBoard; selected: string | null; onSelect: (card: Card) => void }) {
  const [stage, setStage] = useState<Stage | null>(null);
  const shown = stage ? [stage] : STAGES;
  return (
    <div className="flex min-h-0 w-full flex-col md:w-[245px] md:shrink-0 md:border-r">
      {home.needs.length > 0 && (
        <section aria-label="Te așteaptă" className="shrink-0 border-b bg-bg1">
          <SecHead><StatusDot dot="needs" label={null} className="size-1.5" />needs<span className="font-semibold text-muted-foreground tabular-nums">{home.needs.length}</span></SecHead>
          <ul>
            {home.needs.map((c) => (
              <li key={c.id}>
                <button type="button" onClick={() => onSelect(c)} aria-current={selected === c.id ? "true" : undefined} className={rowCls(selected === c.id)}>
                  <StatusDot dot="needs" className="size-1.5" />
                  <span className="min-w-0 flex-1 truncate text-foreground">{c.title}</span>
                  <span className="max-w-[74px] shrink-0 truncate text-[10px] text-dim">{projectName(c.project)}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <div className="flex h-[22px] shrink-0 items-center gap-0.5 border-b bg-bg1 px-1">
        <span className="border-b-2 border-primary px-1.5 py-0.5 text-[10px] font-semibold text-foreground">BOARD</span>
      </div>
      <div role="group" aria-label="Etape" className="grid shrink-0 grid-cols-5 border-b bg-bg1">
        {STAGES.map((s) => (
          <button key={s} type="button" aria-pressed={stage === s} aria-label={`${STAGE_LABEL[s]} ${home.byStage[s].length}`} onClick={() => setStage(stage === s ? null : s)}
            className={cn("flex h-[22px] items-center justify-center gap-[3px] border-b-2 text-[9.5px] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset max-md:h-9",
              stage === s ? "border-primary bg-sel2 font-bold text-foreground" : "border-transparent text-dim hover:bg-bg2 hover:text-foreground")}>
            <span className={STAGE_TEXT[s]}>{STAGE_LETTER[s]}</span><span className="tabular-nums">{home.byStage[s].length}</span>
          </button>
        ))}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {shown.map((s) => (
          <section key={s} aria-label={STAGE_LABEL[s]}>
            <SecHead><span className={STAGE_TEXT[s]}>■ {STAGE_LABEL[s]}</span><span className="font-semibold text-muted-foreground tabular-nums">{home.byStage[s].length}</span></SecHead>
            <ul>
              {home.byStage[s].map((c) => (
                <CardRow key={c.id} card={c} selected={selected === c.id} needs={home.waiting.some((w) => w.card === c.id)} onSelect={() => onSelect(c)} />
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}

type Latest = { key: string; text: string | null; at: string | null };

/** The latest assistant text of a session this gateway lists; null when it lists none or shows only a terminal. */
function useLatest(project: string | null, session: CardSession | null, version: unknown): Latest | null {
  const key = project && session ? `${project}/${session.id}` : null;
  const [latest, setLatest] = useState<Latest | null>(null);
  useEffect(() => {
    if (!key || !project || !session) return;
    const abort = new AbortController();
    api<ConversationResponse>("conversation", { project, id: session.id, limit: "6" }, undefined, { signal: abort.signal })
      .then((data) => {
        const message = "snapshot" in data ? data.snapshot.messages.filter((m) => m.role === "assistant" && m.parts.some((p) => p.type === "text")).at(-1) : undefined;
        const text = message?.parts.flatMap((p) => p.type === "text" ? [p.text] : []).join("\n") ?? null;
        setLatest({ key, text, at: message?.timestamp ?? null });
      })
      .catch(() => { if (!abort.signal.aborted) setLatest({ key, text: null, at: null }); });
    return () => abort.abort();
  }, [key, project, session, version]);
  return latest?.key === key ? latest : null;
}

const actCls = "inline-flex h-5 items-center gap-1 rounded-xs border px-1.5 text-[10px] whitespace-nowrap text-foreground outline-none hover:border-primary hover:bg-bg1 focus-visible:ring-2 focus-visible:ring-ring max-md:h-9 max-md:px-2.5";

function ArtifactDialog({ artifact, onClose }: { artifact: Artifact | null; onClose: () => void }) {
  return (
    <Dialog open={artifact !== null} onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="max-h-[90dvh] gap-3 overflow-y-auto border-primary font-mono sm:max-w-[min(1140px,94vw)]" aria-describedby={undefined}>
        <DialogTitle className="flex items-center gap-2 text-[11px]">
          <span className="font-bold text-primary">⌘X</span>
          {artifact && <span className="rounded-xs bg-bg1 px-1 text-[10px] text-muted-foreground">{artifact.kind}</span>}
          <span className="truncate">{artifact?.title}</span>
        </DialogTitle>
        {artifact && <ArtifactItem artifact={artifact} now={Date.now()} />}
      </DialogContent>
    </Dialog>
  );
}

export function CardPane({ card, show, home, workspace, decisions, onApprove, onOpenCode, onBack, version }: {
  card: Card;
  show: CardShow | null;
  home: HomeBoard;
  workspace: Workspace;
  decisions: Decision[];
  onApprove: (card: Card, wait: NeedsYou) => void;
  onOpenCode: (card: Card, sessions: CardSession[] | null) => void;
  onBack?: () => void;
  version: unknown;
}) {
  const sessions = show?.sessions ?? [];
  const wait = firstWait(home.waiting.filter((w) => w.card === card.id));
  const target = wait ?? openTarget(sessions);
  const defaultIdx = Math.max(0, sessions.findIndex((s) => target && s.id === target.id && s.host === target.host));
  const [picked, setPicked] = useState<{ card: string; idx: number } | null>(null);
  const idx = picked?.card === card.id && picked.idx < sessions.length ? picked.idx : defaultIdx;
  const session = sessions[idx] ?? null;
  const project = session && card.project ? sessionProject(card.project, session.host, workspace) : null;
  const latest = useLatest(project, session, version);
  const [openArt, setOpenArt] = useState<Artifact | null>(null);
  const artifacts = (show?.artifacts ?? []).filter((a) => a.kind !== "decision");
  const now = Date.now();
  const cardDecisions = decisions.filter((d) => d.card === card.id);
  const dot = wait ? "needs" : card.stage === "done" ? "done" : sessions.some((s) => s.status === "running") ? "running" : card.stage === "review" ? "done" : "off";
  const branch = card.checkout?.branch ?? "main";
  const openCode = () => onOpenCode(card, show ? sessions : null);
  return (
    <section aria-label={`Card ${card.ref}`} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex min-h-[27px] shrink-0 items-center gap-1.5 border-b px-2 py-1 max-md:flex-wrap">
        {onBack && (
          <button type="button" onClick={onBack} className="-ml-1 inline-flex size-9 items-center justify-center rounded-xs outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label="Înapoi">
            <ArrowLeftIcon aria-hidden className="size-4" />
          </button>
        )}
        <StatusDot dot={dot} className="size-1.5" />
        <span className="shrink-0 text-dim tabular-nums">{card.ref.split("@")[0]}{refHost(card.ref) && <span className="text-remote">@{refHost(card.ref)}</span>}</span>
        <h1 className="min-w-0 truncate font-semibold text-foreground max-md:basis-[calc(100%-6rem)]">{card.title}</h1>
        <span className={cn("shrink-0 rounded-xs bg-bg1 px-1 text-[10px] leading-[1.4]", STAGE_TEXT[card.stage])}>{STAGE_LABEL[card.stage]}</span>
        <button type="button" onClick={openCode} title={`Deschide ${projectName(card.project)} / ${branch} în CODE (Enter)`}
          className="inline-flex shrink-0 items-center gap-1 rounded-xs border border-line-info bg-tint-branch px-1.5 py-px text-[10.5px] whitespace-nowrap text-info outline-none hover:border-primary hover:bg-sel2 hover:text-foreground focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-ring">
          <span className="font-semibold text-primary">{projectName(card.project)}</span><span className="text-dim">/</span><span>{branch}</span>
          {card.checkout && card.checkout.host !== workspace.host && <span className="text-remote">@{card.checkout.host}</span>}
          <kbd className="ml-0.5 font-mono text-[9px] text-dim">⏎</kbd>
        </button>
      </div>
      {sessions.length > 0 && (
        <ol aria-label="Lanț de sesiuni" className="flex shrink-0 flex-wrap items-center gap-x-1 gap-y-0.5 border-b px-2 py-[3px] text-[10px] text-dim">
          {sessions.map((s, k) => (
            <li key={`${s.host}/${s.id}`} className="contents">
              {k > 0 && <span aria-hidden>→</span>}
              <button type="button" onClick={() => setPicked({ card: card.id, idx: k })} aria-current={k === idx ? "step" : undefined}
                className={cn("inline-flex h-[18px] items-center gap-1 rounded-xs px-[5px] whitespace-nowrap outline-none focus-visible:ring-2 focus-visible:ring-ring max-md:h-8",
                  k === idx ? "bg-sel2 text-foreground" : "hover:bg-bg1 hover:text-foreground")}>
                <StatusDot dot={wait && wait.id === s.id ? "needs" : sessionDot(s.status)} className="size-1.5" />{s.name}
                {s.writer && <span className="text-warn" aria-label="scrie în checkout">✎</span>}
              </button>
            </li>
          ))}
        </ol>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {session && (
          <div className="max-w-[820px] px-2.5 py-[5px]">
            <div className="mb-px flex items-center gap-1.5 text-[10px] text-dim">
              <span>{session.name}</span><span>{compactAge(latest?.at ? Date.parse(latest.at) : session.status_changed_at, now)}</span>
              <span>· {statusLabel(wait && wait.id === session.id ? "needs_feedback" : session.status)}</span>
            </div>
            <p className="leading-[1.45] break-words whitespace-pre-wrap text-foreground">
              {latest?.text ?? (project ? "" : "Mesajele sesiunii se văd în CODE, pe laptopul ei.")}
            </p>
          </div>
        )}
        {!show && <p aria-busy="true" className="px-2.5 py-1.5 text-dim">Se încarcă…</p>}
        <div className="flex flex-wrap items-center gap-[5px] px-2.5 pt-1 pb-1.5">
          {wait && (
            <button type="button" onClick={() => onApprove(card, wait)} aria-keyshortcuts="a"
              className={cn(actCls, "border-line-danger text-err hover:border-err hover:bg-btn-danger")}>
              ✓ aprobă {wait.name} <kbd className="font-mono text-[9.5px] text-dim">a</kbd>
            </button>
          )}
          <button type="button" onClick={openCode} aria-keyshortcuts="Enter" className={actCls}>▸ deschide în cod <kbd className="font-mono text-[9.5px] text-dim">⏎</kbd></button>
          {artifacts.length > 0 && (
            <button type="button" onClick={() => setOpenArt(artifacts[0])} aria-keyshortcuts="v" className={actCls}>⌘X artefacte ({artifacts.length}) <kbd className="font-mono text-[9.5px] text-dim">v</kbd></button>
          )}
        </div>
        {artifacts.length > 0 && (
          <section aria-label="Artefacte" className="border-t border-hair px-2.5 pt-1 pb-1.5">
            <h2 className="flex items-center gap-[5px] pt-0.5 pb-[3px] text-[9.5px] tracking-[0.06em] text-dim uppercase">
              <span className="text-primary">⌘X</span>artefacte<span className="font-semibold text-muted-foreground">{artifacts.length}</span>
              <span className="flex-1" /><span className="tracking-normal normal-case">v sau click deschide</span>
            </h2>
            <ul>
              {artifacts.map((a) => {
                const [icon, tone] = ART_ICON[a.kind] ?? ["◆", "text-primary"];
                return (
                  <li key={a.id}>
                    <button type="button" onClick={() => setOpenArt(a)} data-artifact={a.id}
                      className="flex h-[21px] w-full min-w-0 items-center gap-1.5 rounded-r-xs border-l-2 border-l-transparent px-1.5 text-left outline-none hover:border-l-primary hover:bg-bg1 focus-visible:ring-2 focus-visible:ring-ring max-md:h-10">
                      <span aria-hidden className={cn("w-3 flex-none text-center", tone)}>{icon}</span>
                      <span className="min-w-0 flex-1 truncate text-foreground">{a.title}</span>
                      <span className="flex-none rounded-xs bg-bg1 px-1 text-[9.5px] text-dim">{a.kind}</span>
                      <span className="flex-none text-[10px] whitespace-nowrap text-dim">{[a.session?.name, compactAge(a.created_at, now)].filter(Boolean).join(" · ")}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        )}
        {(cardDecisions.length > 0 || (show?.log.length ?? 0) > 0) && (
          <section aria-label="Jurnal" className="border-t border-hair px-2.5 pt-1 pb-1.5">
            {cardDecisions.map((d) => (
              <p key={d.id} className="my-0.5 border-l-2 border-l-special px-2 py-[3px] text-[11px]">
                <span className="text-special">◆</span> {d.title} <span className="text-dim">{compactAge(d.created_at, now)}</span>
              </p>
            ))}
            {show?.log.map((n) => (
              <p key={n.id} className="my-0.5 border-l-2 border-l-edge px-2 py-[3px] text-[11px]">
                <span className="text-muted-foreground">{n.author.name}:</span> {n.text} <span className="text-dim">{compactAge(n.created_at, now)}</span>
              </p>
            ))}
          </section>
        )}
      </div>
      <button type="button" onClick={openCode}
        className="flex h-6 shrink-0 items-center gap-1.5 border-t bg-bg1 px-2 text-left text-dim outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset max-md:h-11">
        <span className="text-primary">›</span>{session ? `Răspunde lui ${session.name} în CODE… (Enter)` : "Deschide în CODE… (Enter)"}
      </button>
      <ArtifactDialog artifact={openArt} onClose={() => setOpenArt(null)} />
    </section>
  );
}

function DecisionsRail({ card, cards, decisions, workspace, onDecisions, onSelect }: {
  card: Card | null; cards: Card[]; decisions: Decision[]; workspace: Workspace; onDecisions: () => void; onSelect: (card: Card) => void;
}) {
  const shown = [...decisions].sort((a, b) => b.created_at - a.created_at).slice(0, 12);
  return (
    <aside aria-label="Context și decizii" className="hidden w-[215px] shrink-0 flex-col border-l lg:flex">
      {card && (
        <div className="border-b bg-bg1 px-1.5 py-[5px] text-[10px]">
          <div className="flex min-w-0 items-center gap-1">
            <span className="font-semibold text-primary">{projectName(card.project)}</span><span className="text-dim">/</span>
            <span className="truncate text-info">{card.checkout?.branch ?? "main"}</span>
          </div>
          {card.checkout && (
            <div className="mt-[3px] flex min-w-0 items-center gap-1">
              <StatusDot dot={card.checkout.host === workspace.host ? "done" : "off"} label={null} className="size-1.5" />
              <span className="shrink-0 text-remote">@{card.checkout.host}</span>
              <span className="truncate text-dim" title={card.checkout.path}>{card.checkout.path}</span>
            </div>
          )}
        </div>
      )}
      <a href="/decisions" onClick={(e) => { e.preventDefault(); onDecisions(); }}
        className="flex items-center gap-[5px] px-1.5 pt-1 pb-0.5 text-[9.5px] tracking-[0.06em] text-dim uppercase outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
        <span className="text-special">◆</span>decisions<span className="font-semibold text-muted-foreground tabular-nums">{decisions.length}</span><span className="flex-1" /><span>4</span>
      </a>
      <ul className="min-h-0 flex-1 overflow-y-auto">
        {shown.map((d) => {
          const dc = cards.find((c) => c.id === d.card);
          return (
            <li key={d.id}>
              <button type="button" onClick={() => dc && onSelect(dc)} disabled={!dc}
                className="block w-full border-b border-l-2 border-hair border-l-transparent px-1.5 py-[3px] text-left outline-none enabled:hover:bg-bg1 focus-visible:ring-2 focus-visible:ring-ring">
                <span className="block truncate text-[10.5px] text-foreground">{d.title}</span>
                <span className="block truncate text-[9.5px] text-dim">{projectName(d.project)}{dc ? ` · ${dc.ref}` : ""}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </aside>
  );
}

export function HomePage({ home, card, show, workspace, decisions, phone, stale, loaded, onSelect, onBack, onApprove, onOpenCode, onDecisions, version }: {
  home: HomeBoard;
  card: Card | null;
  show: CardShow | null;
  workspace: Workspace;
  decisions: Decision[];
  phone: boolean;
  stale: boolean;
  loaded: boolean;
  onSelect: (card: Card) => void;
  onBack: () => void;
  onApprove: (card: Card, wait: NeedsYou) => void;
  onOpenCode: (card: Card, sessions: CardSession[] | null) => void;
  onDecisions: () => void;
  version: unknown;
}) {
  if (!loaded) return (
    <p role={stale ? "alert" : undefined} aria-busy={!stale} className="p-4 font-mono text-dim">{stale ? "Board-ul nu poate fi citit acum." : "Se încarcă…"}</p>
  );
  const pane = card && (
    <CardPane key={card.id} card={card} show={show?.id === card.id ? show : null} home={home} workspace={workspace} decisions={decisions}
      onApprove={onApprove} onOpenCode={onOpenCode} onBack={phone ? onBack : undefined} version={version} />
  );
  if (phone) return <div className="flex min-h-0 flex-1 flex-col font-mono text-[12.5px] leading-[1.35]">{pane ?? <HomeLeft home={home} selected={null} onSelect={onSelect} />}</div>;
  return (
    <div className="flex min-h-0 flex-1 font-mono text-[11.5px] leading-[1.35]">
      <HomeLeft home={home} selected={card?.id ?? null} onSelect={onSelect} />
      {pane ?? <p className="flex-1 p-4 text-dim">Niciun card încă. Agenții le adaugă cu <code>nebula todo add</code>.</p>}
      <DecisionsRail card={card} cards={home.cards} decisions={decisions} workspace={workspace} onDecisions={onDecisions} onSelect={onSelect} />
    </div>
  );
}

