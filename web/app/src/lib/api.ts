// Authenticated fetch against the same-origin gateway. The bearer token lives only in
// sessionStorage (never in URLs); a 401 clears it and notifies the app.

const TOKEN_KEY = "nebula-token";
let token: string = safeRead();
const listeners = new Set<() => void>();

function safeRead(): string {
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function getToken(): string {
  return token;
}

export function setToken(value: string): void {
  token = value;
  try {
    if (value) sessionStorage.setItem(TOKEN_KEY, value);
    else sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    // Session storage unavailable: keep the token in memory for this page only.
  }
  listeners.forEach((fn) => fn());
}

export function onAuthChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export class ApiError extends Error {
  status: number;
  /** True when the request may have reached the gateway (timeout/network after send). */
  uncertain: boolean;
  constructor(message: string, status: number, uncertain = false) {
    super(message);
    this.status = status;
    this.uncertain = uncertain;
  }
}

export interface RequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

export async function api<T>(
  path: string,
  query: Record<string, string | undefined> = {},
  body?: unknown,
  options: RequestOptions = {},
): Promise<T> {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) if (value !== undefined) params.set(key, value);
  const url = `/api/${path}${params.size ? `?${params}` : ""}`;
  const timeout = AbortSignal.timeout(options.timeoutMs ?? 30000);
  const signal = options.signal ? AbortSignal.any([options.signal, timeout]) : timeout;
  let response: Response;
  try {
    response = await fetch(url, {
      method: body === undefined ? "GET" : "POST",
      cache: "no-store",
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      signal,
    });
  } catch (error) {
    if (options.signal?.aborted) throw new ApiError("Anulat", 0);
    const message = timeout.aborted ? "Serverul nu a răspuns la timp" : "Fără conexiune la server";
    throw new ApiError(message, 0, body !== undefined);
  }
  let data: { error?: string } & Record<string, unknown> = {};
  try {
    data = await response.json();
  } catch {
    if (response.ok) throw new ApiError("Răspuns invalid de la server", response.status);
  }
  if (response.status === 401) {
    setToken("");
    throw new ApiError("Token respins. Autentifică-te din nou.", 401);
  }
  if (!response.ok) throw new ApiError(typeof data.error === "string" ? data.error : "Cererea a eșuat", response.status);
  return data as T;
}

export function attemptId(): string {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
