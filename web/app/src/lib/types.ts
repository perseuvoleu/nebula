// Wire types of the Nebula web gateway (web/server.py). Identity is always the Nebula session id.

export type AgentKind = "pi" | "claude" | "codex" | "cursor";
export const AGENT_STATUSES = ["fresh", "queued", "running", "finished", "needs_feedback", "terminated", "disconnected"] as const;
export type AgentStatus = (typeof AGENT_STATUSES)[number];

export interface SessionRow {
  id: string;
  name: string;
  kind: AgentKind | string;
  status: AgentStatus;
  project: string;
  worktree: string;
  path: string;
  archived: boolean;
}

export interface Worktree {
  path: string;
  branch: string;
  is_main: boolean;
  project: string;
  sessions: { name: string; kind: string; status: AgentStatus; alive: boolean }[];
}

export interface Capabilities {
  structured: boolean;
  reason: string | null;
  liveText: boolean;
  liveTextReason: string;
  tools: boolean;
  send: boolean;
  terminal: boolean;
  kind: string;
}

export interface TextPart {
  type: "text";
  text: string;
  truncated?: boolean;
}

export interface ImagePart {
  type: "image";
  mimeType: string;
}

export interface ToolPart {
  type: "tool";
  callId: string;
  name: string;
  args: string;
  argsTruncated: boolean;
  state: "pending" | "done" | "error";
  output: string;
  outputLength: number;
  truncated: boolean;
}

export type Part = TextPart | ImagePart | ToolPart;
export type Role = "user" | "assistant" | "tool" | "notice";

export interface Message {
  id: string;
  role: Role;
  timestamp: string | null;
  parts: Part[];
  model?: string;
  stopReason?: string;
  error?: string;
  label?: string;
}

export interface ToolResult {
  callId: string;
  name: string;
  state: "done" | "error";
  output: string;
  outputLength: number;
  truncated: boolean;
}

export interface Cursor {
  generation: string;
  offset: number;
  leaf: string | null;
}

export interface Snapshot {
  kind: "snapshot" | "reset";
  messages: Message[];
  hasMore: boolean;
  unknownEntries: number;
  name: string | null;
  cursor: Cursor;
  partial: boolean;
}

export type ConversationResponse =
  | { session: SessionRow; capabilities: Capabilities & { structured: true }; snapshot: Snapshot }
  | { session: SessionRow; capabilities: Capabilities & { structured: false } };

export type UpdatesResponse =
  | { session: SessionRow; kind: "none"; cursor: Cursor }
  | { session: SessionRow; kind: "delta"; messages: Message[]; toolResults: ToolResult[]; cursor: Cursor }
  | ({ session: SessionRow } & Snapshot & { kind: "reset" })
  | { session: SessionRow; capabilities: Capabilities & { structured: false } };

export interface SendResponse {
  ok: true;
  attempt: string;
  acceptedAt: string;
}

export interface NewResponse {
  ok: true;
  name: string;
  id: string | null;
}
