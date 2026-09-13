// Typed conversation controller for one selected Nebula session.
//
// Chosen over AI SDK UI's useChat: the conversation already exists on the host, messages
// arrive from the terminal without a web send, and reconnects must resume from a gateway
// cursor. Modelling that as request/response chat would add state instead of removing it.
import { useEffect, useMemo, useSyncExternalStore } from "react";
import { ApiError, api, attemptId } from "./api.ts";
import type {
  Capabilities, ConversationResponse, Cursor, Message, SendResponse, SessionRow, ToolResult, UpdatesResponse,
} from "./types.ts";

export type AttemptState = "sending" | "accepted" | "uncertain" | "failed";

export interface Attempt {
  id: string;
  text: string;
  createdAt: string;
  state: AttemptState;
  error?: string;
  knownIds: string[];
}

export type Phase = "loading" | "ready" | "unsupported" | "error";
export type Connection = "live" | "stale" | "offline";

export interface ConversationState {
  phase: Phase;
  session: SessionRow | null;
  capabilities: Capabilities | null;
  messages: Message[];
  cursor: Cursor | null;
  hasMore: boolean;
  loadingOlder: boolean;
  error: string | null;
  connection: Connection;
  lastSync: number | null;
  attempts: Attempt[];
  generation: number; // bumps on reset so views can re-anchor scrolling
  name: string | null;
}

const initial = (): ConversationState => ({
  phase: "loading", session: null, capabilities: null, messages: [], cursor: null, hasMore: false,
  loadingOlder: false, error: null, connection: "stale", lastSync: null, attempts: [], generation: 0, name: null,
});

export const POLL_IDLE_MS = 2500;
export const POLL_ACTIVE_MS = 1500;

export function messageText(message: Message): string {
  return message.parts.filter((p) => p.type === "text").map((p) => (p.type === "text" ? p.text : "")).join("\n");
}

export function mergeToolResults(messages: Message[], results: ToolResult[]): Message[] {
  if (!results.length) return messages;
  const next = messages.slice();
  for (const result of results) {
    for (let index = next.length - 1; index >= 0; index -= 1) {
      const message = next[index];
      const partIndex = message.parts.findIndex((p) => p.type === "tool" && p.callId === result.callId);
      if (partIndex === -1) continue;
      const parts = message.parts.slice();
      const part = parts[partIndex];
      if (part.type === "tool") {
        parts[partIndex] = { ...part, state: result.state, output: result.output,
          outputLength: result.outputLength, truncated: result.truncated };
      }
      next[index] = { ...message, parts };
      break;
    }
  }
  return next;
}

/** Drop attempts whose user message is now visible in the authoritative history. */
export function reconcileAttempts(attempts: Attempt[], messages: Message[]): Attempt[] {
  const claimed = new Set<string>();
  return attempts.filter((attempt) => {
    if (attempt.state === "sending" || attempt.state === "failed") return true;
    const known = new Set(attempt.knownIds);
    const match = messages.find((m) => m.role === "user" && !known.has(m.id) && !claimed.has(m.id)
      && messageText(m) === attempt.text && (!m.timestamp || m.timestamp >= attempt.createdAt.slice(0, 19)));
    if (match) claimed.add(match.id);
    return !match;
  });
}

export class ConversationController {
  private state = initial();
  private listeners = new Set<() => void>();
  private abort = new AbortController();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private polling = false;
  private disposed = false;

  readonly project: string;
  readonly sessionId: string;

  constructor(project: string, sessionId: string) {
    this.project = project;
    this.sessionId = sessionId;
    void this.load();
    document.addEventListener("visibilitychange", this.onVisible);
    window.addEventListener("online", this.onOnline);
    window.addEventListener("offline", this.onOffline);
  }

  dispose(): void {
    this.disposed = true;
    this.abort.abort();
    if (this.timer) clearTimeout(this.timer);
    document.removeEventListener("visibilitychange", this.onVisible);
    window.removeEventListener("online", this.onOnline);
    window.removeEventListener("offline", this.onOffline);
    this.listeners.clear();
  }

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  getSnapshot = (): ConversationState => this.state;

  private set(patch: Partial<ConversationState>): void {
    if (this.disposed) return;
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((fn) => fn());
  }

  private onVisible = () => { if (!document.hidden) this.pollSoon(0); };
  private onOnline = () => { this.set({ connection: "stale" }); this.pollSoon(0); };
  private onOffline = () => this.set({ connection: "offline" });

  private applyMessages(messages: Message[], extra: Partial<ConversationState> = {}): void {
    this.set({ messages, attempts: reconcileAttempts(this.state.attempts, messages), ...extra });
  }

  async load(): Promise<void> {
    try {
      const data = await api<ConversationResponse>("conversation", { project: this.project, id: this.sessionId },
        undefined, { signal: this.abort.signal });
      if (this.abort.signal.aborted) return;
      if (!("snapshot" in data)) {
        this.set({ phase: "unsupported", session: data.session, capabilities: data.capabilities,
          connection: "live", lastSync: Date.now(), error: null });
      } else {
        this.applyMessages(data.snapshot.messages, { phase: "ready", session: data.session,
          capabilities: data.capabilities, cursor: data.snapshot.cursor, hasMore: data.snapshot.hasMore,
          connection: "live", lastSync: Date.now(), error: null, name: data.snapshot.name });
      }
    } catch (error) {
      if (this.abort.signal.aborted) return;
      const message = error instanceof ApiError ? error.message : "Eroare neașteptată";
      if (error instanceof ApiError && error.status === 401) return;
      this.set({ phase: this.state.phase === "loading" ? "error" : this.state.phase, error: message,
        connection: navigator.onLine ? "stale" : "offline" });
    }
    this.pollSoon();
  }

  pollSoon(delay?: number): void {
    if (this.disposed) return;
    if (this.timer) clearTimeout(this.timer);
    const active = this.state.session?.status === "running" || this.state.attempts.some((a) => a.state !== "failed");
    this.timer = setTimeout(() => void this.poll(), delay ?? (active ? POLL_ACTIVE_MS : POLL_IDLE_MS));
  }

  async poll(): Promise<void> {
    if (this.disposed || this.polling) return;
    if (document.hidden) return; // resumed by visibilitychange
    this.polling = true;
    try {
      if (this.state.phase === "error" || this.state.phase === "unsupported") {
        await this.load();
        return;
      }
      const cursor = this.state.cursor;
      const data = await api<UpdatesResponse>("conversation/updates", {
        project: this.project, id: this.sessionId, generation: cursor?.generation,
        offset: cursor ? String(cursor.offset) : undefined, leaf: cursor?.leaf ?? undefined,
      }, undefined, { signal: this.abort.signal, timeoutMs: 20000 });
      if (this.abort.signal.aborted) return;
      const synced = { connection: "live" as const, lastSync: Date.now(), error: null, session: data.session };
      if ("capabilities" in data) {
        this.set({ ...synced, phase: "unsupported", capabilities: data.capabilities });
      } else if (data.kind === "none") {
        this.set({ ...synced, attempts: reconcileAttempts(this.state.attempts, this.state.messages) });
      } else if (data.kind === "delta") {
        const merged = mergeToolResults([...this.state.messages, ...data.messages], data.toolResults);
        this.applyMessages(merged, { ...synced, cursor: data.cursor });
      } else {
        this.applyMessages(data.messages, { ...synced, cursor: data.cursor, hasMore: data.hasMore,
          generation: this.state.generation + 1, name: data.name });
      }
    } catch (error) {
      if (this.abort.signal.aborted) return;
      if (error instanceof ApiError && error.status === 401) return;
      const message = error instanceof ApiError ? error.message : "Eroare neașteptată";
      this.set({ error: message, connection: navigator.onLine ? "stale" : "offline" });
    } finally {
      this.polling = false;
      this.pollSoon();
    }
  }

  async loadOlder(): Promise<void> {
    const first = this.state.messages[0];
    if (!first || this.state.loadingOlder || !this.state.hasMore) return;
    this.set({ loadingOlder: true });
    try {
      const data = await api<ConversationResponse>("conversation",
        { project: this.project, id: this.sessionId, before: first.id }, undefined, { signal: this.abort.signal });
      if (this.abort.signal.aborted || !("snapshot" in data)) return;
      const known = new Set(this.state.messages.map((m) => m.id));
      const older = data.snapshot.messages.filter((m) => !known.has(m.id));
      this.set({ messages: [...older, ...this.state.messages], hasMore: data.snapshot.hasMore, loadingOlder: false });
    } catch (error) {
      if (this.abort.signal.aborted) return;
      this.set({ loadingOlder: false, error: error instanceof ApiError ? error.message : "Eroare neașteptată" });
    }
  }

  /** One explicit delivery attempt. Never retried automatically. */
  async send(text: string): Promise<Attempt> {
    const attempt: Attempt = { id: attemptId(), text, createdAt: new Date().toISOString(), state: "sending",
      knownIds: this.state.messages.map((m) => m.id) };
    this.set({ attempts: [...this.state.attempts, attempt] });
    const update = (patch: Partial<Attempt>) =>
      this.set({ attempts: this.state.attempts.map((a) => (a.id === attempt.id ? { ...a, ...patch } : a)) });
    try {
      await api<SendResponse>("send", {}, { project: this.project, id: this.sessionId, text, attempt: attempt.id },
        { signal: this.abort.signal });
      update({ state: "accepted" });
    } catch (error) {
      const failure = error instanceof ApiError ? error : new ApiError("Eroare neașteptată", 0);
      update({ state: failure.uncertain || failure.status === 504 ? "uncertain" : "failed", error: failure.message });
    }
    this.pollSoon(400);
    return this.state.attempts.find((a) => a.id === attempt.id) ?? attempt;
  }

  dismissAttempt(id: string): void {
    this.set({ attempts: this.state.attempts.filter((a) => a.id !== id) });
  }
}

export function useConversation(project: string | null, sessionId: string | null): {
  state: ConversationState | null; controller: ConversationController | null;
} {
  const controller = useMemo(
    () => (project && sessionId ? new ConversationController(project, sessionId) : null),
    [project, sessionId],
  );
  useEffect(() => () => controller?.dispose(), [controller]);
  const state = useSyncExternalStore(
    controller ? controller.subscribe : () => () => {},
    controller ? controller.getSnapshot : () => null,
  );
  return { state, controller };
}
