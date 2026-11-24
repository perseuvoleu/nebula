export type Stage = "backlog" | "next" | "doing" | "review" | "done";

export const STAGES: Stage[] = ["backlog", "next", "doing", "review", "done"];

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

export type CardCheckout = { host: string; branch: string };

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
  checkout: CardCheckout | null;
  sessions: BoardSession[];
  artifacts: number;
  age: string;
};

export type LogEntry = { id: string; author: string; text: string; age: string };

export type Dot = "needs" | "running" | "done" | "queued" | "off";

export const DOT_LABEL: Record<Dot, string> = {
  needs: "Așteaptă răspuns",
  running: "Lucrează",
  done: "Gata",
  queued: "Nou",
  off: "Inactiv",
};

export function sessionDot(status: string): Dot {
  if (status === "needs_feedback") return "needs";
  if (status === "running") return "running";
  if (status === "finished") return "done";
  if (status === "fresh") return "queued";
  return "off";
}

export function cardDot(card: BoardCard): Dot {
  if (card.needsYou) return "needs";
  if (card.stage === "done") return "done";
  if (card.sessions.some((s) => s.status === "running")) return "running";
  if (card.sessions.some((s) => s.status === "fresh")) return "queued";
  return card.stage === "review" ? "done" : "off";
}

export const projectName = (project: string | null): string => project ?? "home";
