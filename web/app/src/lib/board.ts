import type { Dot } from "./status.ts";

export const STAGES = ["backlog", "next", "doing", "review", "done"] as const;
export type Stage = (typeof STAGES)[number];

export const RUN_ROLES = ["worker", "review", "fix", "verify", "research"] as const;
export type RunRole = (typeof RUN_ROLES)[number];

export const STAGE_LABEL: Record<Stage, string> = {
  backlog: "Backlog",
  next: "Urmează",
  doing: "În lucru",
  review: "Review",
  done: "Gata",
};

export type BoardSession = {
  id: string;
  host: string;
  name: string;
  role: string;
  kind: string;
  status: string;
  writer: boolean;
};

export type BlockerRef = { ref: string; title: string };

export type BoardCard = {
  ref: string;
  title: string;
  stage: Stage;
  project: string | null;
  proposed: boolean;
  proposedBy: string | null;
  needsYou: boolean;
  blockers: BlockerRef[];
  checkout: Checkout | null;
  sessions: BoardSession[] | null;
  artifacts: number;
  age: string;
};

export type LogEntry = { id: string; author: string; text: string; age: string };


/** Null when the card's sessions are unknown (a list row, not a card read), so the tile makes no status claim. */
export function cardDot(card: BoardCard): Dot | null {
  if (card.sessions === null) return null;
  if (card.needsYou) return "needs";
  if (card.stage === "done") return "done";
  if (card.sessions.some((s) => s.status === "running")) return "running";
  if (card.sessions.some((s) => s.status === "fresh")) return "queued";
  return card.stage === "review" ? "done" : "off";
}

export const projectName = (project: string | null): string => project ?? "home";

export type Author = { host: string; agent: string | null; name: string };

export type Checkout = { host: string; branch: string; path: string; worktree: boolean };

export type Card = {
  id: string;
  number: number;
  ref: string;
  title: string;
  stage: Stage;
  project: string | null;
  proposed: boolean;
  archived: boolean;
  created_by: Author;
  checkout: Checkout | null;
  blocked_by: string[];
  cleared_blockers: string[];
  created_at: number;
  updated_at: number;
};

export type HostCheckout = { project: string; branch: string; path: string; worktree: boolean };

export type BoardHost = {
  host: string;
  writers: number;
  writer_slots: number | null;
  checkouts: HostCheckout[];
  needs_you: number;
  stale: boolean;
};

export type SessionAt = { host: string; id: string };

export type NeedsYou = SessionAt & { name: string; card: string; status_changed_at: number };

export type Away = { finished: string[]; needs_you: NeedsYou[]; decisions: string[] };

export type BoardData = { cards: Card[]; hosts: BoardHost[]; needs_you: NeedsYou[]; away: Away };

export type BoardReply =
  | { version: string; unchanged: true }
  | { version: string; board: BoardData; next?: string | null };

export type CardSession = {
  host: string;
  id: string;
  name: string;
  role: RunRole | null;
  kind: string;
  status: string;
  status_changed_at: number;
  writer: boolean;
  alive: boolean;
  stale: boolean;
};

export type CardNote = { id: string; card: string; text: string; author: Author; created_at: number };

export type SessionLink = { host: string; id: string; name: string };

export type ArtifactContent = { host: string; entry: string } | { url: string } | { body: string };

export type Artifact = {
  id: string;
  kind: string;
  title: string;
  card: string | null;
  project: string | null;
  session: SessionLink | null;
  content: ArtifactContent;
  created_at: number;
  /** Minted by the gateway when it stores the files: /a/<grant>/<id>/<entry>. */
  src?: string;
  /** Unix seconds after which `src`'s grant is refused. */
  expires?: number;
};

/** How long before a grant's expiry the card page reads a fresh one and moves its artifacts to it. */
export const GRANT_RENEW_MS = 5 * 60_000;

/** True once `now` (epoch ms) is within GRANT_RENEW_MS of `expires` (unix seconds); never for an unknown expiry. */
export const grantLapsing = (expires: number | undefined, now: number): boolean =>
  expires !== undefined && expires * 1000 - GRANT_RENEW_MS <= now;

export type CardShow = Card & { sessions: CardSession[]; artifacts: Artifact[]; log: CardNote[] };

export type HostSlots = { host: string; writers: number; slots: number | null };

export function byStage<T extends { stage: Stage }>(cards: T[]): Record<Stage, T[]> {
  const columns = Object.fromEntries(STAGES.map((stage) => [stage, [] as T[]])) as Record<Stage, T[]>;
  for (const card of cards) columns[card.stage].push(card);
  return columns;
}

/** Blockers still open: not done and not cleared. A blocker outside `cards` stays open with an empty title. */
export function openBlockers(card: Card, cards: Card[]): BlockerRef[] {
  return card.blocked_by.flatMap((ref) => {
    const blocker = cards.find((c) => c.ref === ref);
    return blocker?.stage === "done" || card.cleared_blockers.includes(ref) ? [] : [{ ref, title: blocker?.title ?? "" }];
  });
}

export const isBlocked = (card: Card, cards: Card[]): boolean => openBlockers(card, cards).length > 0;

/** Every host this board knows of: the ones reporting, the ones in card refs and authors, and the card's own sessions' hosts. */
export function knownHosts(board: BoardData | null, sessionHosts: string[] = []): { host: string; writers: number; writerSlots: number | null }[] {
  const known = [...new Set([
    ...(board?.hosts ?? []).map((h) => h.host),
    ...(board?.cards ?? []).flatMap((c) => [c.created_by.host, ...c.ref.split("@").slice(1)]),
    ...sessionHosts,
  ])];
  return known.map((host) => {
    const reported = board?.hosts.find((h) => h.host === host);
    return { host, writers: reported?.writers ?? 0, writerSlots: reported?.writer_slots ?? null };
  });
}

export const writerSlots = (hosts: BoardHost[]): HostSlots[] =>
  hosts.map((h) => ({ host: h.host, writers: h.writers, slots: h.writer_slots }));

const inProject = (project: string | null, scope: SidebarScope): boolean =>
  scope.kind === "all" || scope.kind === "needs" || scope.kind === "all-decisions" || project === scope.project;

/** What changed since the last visit within `scope`; decisions come from `known`, so one not read yet is left out. */
export function awayDigest<D extends { id: string; project: string | null }>(board: BoardData, scope: SidebarScope, known: D[]):
  { finished: Card[]; waiting: Card[]; decisions: D[]; empty: boolean } {
  const scoped = (id: string) => board.cards.filter((c) => c.id === id && inProject(c.project, scope));
  const finished = board.away.finished.flatMap(scoped);
  const waiting = [...new Set(board.away.needs_you.map((w) => w.card))].flatMap(scoped);
  const decisions = board.away.decisions.flatMap((id) => known.filter((d) => d.id === id && inProject(d.project, scope)));
  return { finished, waiting, decisions, empty: !finished.length && !waiting.length && !decisions.length };
}

export const cardSlug = (ref: string): string => ref.replace(/^#/, "");

/** Without `show` (a card read) sessions are null and needsYou false: a list row does not carry them. */
export function toBoardCard(card: Card, cards: Card[], show?: CardShow): BoardCard {
  const sessions = show?.sessions;
  return {
    ref: card.ref, title: card.title, stage: card.stage, project: card.project, proposed: card.proposed,
    proposedBy: card.proposed ? card.created_by.name : null,
    needsYou: !!sessions?.some((s) => s.status === "needs_feedback"),
    blockers: openBlockers(card, cards),
    checkout: card.checkout,
    sessions: sessions?.map((s) => ({ id: s.id, host: s.host, name: s.name, role: s.role ?? "—", kind: s.kind, status: s.status, writer: s.writer })) ?? null,
    artifacts: show?.artifacts.length ?? 0,
    age: "",
  };
}

export type SidebarCheckout = { branch: string; host: string | null; primary: boolean };

export type SidebarProject = {
  project: string | null;
  name: string;
  host: string | null;
  cards: number;
  needsYou: boolean;
  running: boolean;
  checkouts: SidebarCheckout[];
};

export type SidebarScope =
  | { kind: "all" }
  | { kind: "needs" }
  | { kind: "project"; project: string | null }
  | { kind: "checkout"; project: string | null; host: string | null; branch: string }
  | { kind: "decisions"; project: string | null }
  | { kind: "all-decisions" };

/** One entry per project with cards or checkouts, counting every unarchived card (Done included), sorted by name. */
/** waiting: ids of the cards a session waits on, the same set the needs-you strip shows. */
export function sidebarProjects(board: BoardData, waiting: ReadonlySet<string> = new Set()): SidebarProject[] {
  const projects = new Map<string | null, SidebarProject>();
  const project = (key: string | null) => {
    if (!projects.has(key)) projects.set(key, { project: key, name: projectName(key), host: null, cards: 0, needsYou: false, running: false, checkouts: [] });
    return projects.get(key)!;
  };
  const checkout = (p: SidebarProject, co: SidebarCheckout) => {
    if (!p.checkouts.some((c) => c.host === co.host && c.branch === co.branch)) p.checkouts.push(co);
  };
  for (const host of board.hosts) {
    for (const co of host.checkouts) checkout(project(co.project), { branch: co.branch, host: host.host, primary: !co.worktree });
  }
  for (const card of board.cards.filter((c) => !c.archived)) {
    const p = project(card.project);
    p.cards += 1;
    if (card.stage === "doing") p.running = true;
    if (waiting.has(card.id)) p.needsYou = true;
    if (card.checkout) checkout(p, { branch: card.checkout.branch, host: card.checkout.host, primary: !card.checkout.worktree });
  }
  return [...projects.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export function inScope(card: BoardCard, scope: SidebarScope): boolean {
  switch (scope.kind) {
    case "all":
    case "all-decisions": return true;
    case "needs": return card.needsYou;
    case "checkout": return card.project === scope.project && card.checkout?.branch === scope.branch
      && (scope.host === null || card.checkout.host === scope.host);
    default: return card.project === scope.project;
  }
}

export function filterCards<T extends { ref: string; title: string; project: string | null }>(cards: T[], query: string): T[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return cards;
  return cards.filter((c) => [c.ref, c.title, projectName(c.project)].some((field) => field.toLowerCase().includes(needle)));
}

/** The session Open in code opens: the live writer, else the latest session. */
export const openTarget = (sessions: CardSession[]): { id: string; host: string } | null => {
  const s = sessions.find((x) => x.writer && x.alive) ?? sessions.at(-1);
  return s ? { id: s.id, host: s.host } : null;
};

export type SortKey = "ref" | "title" | "stage" | "project";
export type TableSort = { key: SortKey; asc: boolean };

const SORT_VALUE: Record<SortKey, (card: BoardCard) => string | number> = {
  ref: (c) => Number(c.ref.replace(/^#/, "").split("@")[0]) || 0,
  title: (c) => c.title.toLowerCase(),
  stage: (c) => STAGES.indexOf(c.stage),
  project: (c) => projectName(c.project),
};

export function sortCards(cards: BoardCard[], sort: TableSort): BoardCard[] {
  const value = SORT_VALUE[sort.key];
  return [...cards].sort((a, b) => (value(a) < value(b) ? -1 : value(a) > value(b) ? 1 : 0) * (sort.asc ? 1 : -1));
}

export type Workspace = { projects: string[]; host: string };

/** The gateway project that lists a session: its bare name on this host, name@host for a remote the gateway anchors. */
export function sessionProject(project: string, host: string, workspace: Workspace): string | null {
  const key = host === workspace.host ? project : `${project}@${host}`;
  return workspace.projects.includes(key) ? key : null;
}

/** The gateway projects that list sessions on `host`: bare names for this host, name@host for a remote. */
export const hostProjects = (host: string, workspace: Workspace): string[] =>
  workspace.projects.filter((p) => host === workspace.host ? !p.includes("@") : p.endsWith(`@${host}`));
