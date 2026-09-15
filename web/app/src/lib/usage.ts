/** One account-wide weekly quota; timestamps are Unix seconds, never session usage. */
export interface Quota {
  /** Remaining share, or null when no valid reading exists. */
  remainingPercent: number | null;
  /** Provider's reset time; elapsed time never implies restored quota. */
  resetAt: number | null;
  /** Time of the last successful provider read. */
  updatedAt: number | null;
  /** Whether the reading is fresh, stale, missing, or being loaded. */
  status: 'ok' | 'stale' | 'unavailable' | 'loading';
  /** Safe server reason code, translated locally. */
  reason: string | null;
}
/** Independent account readings, present regardless of selected project or session. */
export type Usage = Record<'codex' | 'claude', Quota>;

function empty(): Quota {
  return { remainingPercent: null, resetAt: null, updatedAt: null, status: 'unavailable', reason: null };
}
const timestamp = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value > 0 && value < 253402300800;

/** Validate each provider separately and age readings even when a client is offline. */
export function normalizeUsage(value: unknown, now: number): Usage {
  const input = value && typeof value === 'object' ? value as Record<string, unknown> : {};
  const normalize = (raw: unknown): Quota => {
    if (!raw || typeof raw !== 'object') return empty();
    const row = raw as Record<string, unknown>;
    const reason = typeof row.reason === 'string' ? row.reason : null;
    if (row.remainingPercent === null && (row.status === 'unavailable' || row.status === 'loading')) return { ...empty(), status: row.status, reason };
    if (typeof row.remainingPercent !== 'number' || !Number.isFinite(row.remainingPercent) || row.remainingPercent < 0 || row.remainingPercent > 100 || !timestamp(row.resetAt) || !timestamp(row.updatedAt) || !['ok', 'stale'].includes(String(row.status))) return empty();
    const expired = row.resetAt <= now;
    const aged = now - row.updatedAt >= 300;
    return { remainingPercent: row.remainingPercent, resetAt: row.resetAt, updatedAt: row.updatedAt,
      status: row.status === 'stale' || expired || aged ? 'stale' : 'ok',
      reason: expired ? 'expired_window' : reason };
  };
  return { codex: normalize(input.codex), claude: normalize(input.claude) };
}

/** Compact visible label, including a non-color stale indicator. */
export function quotaLabel(name: string, quota: Quota): string {
  return `${name} ${quota.remainingPercent === null ? '—' : `${Math.round(quota.remainingPercent)}%`}${quota.status === 'stale' ? ' · vechi' : ''}`;
}

/** Browser lifecycle boundaries, supplied explicitly so polling can be tested without a DOM. */
export interface UsageEnvironment {
  /** Visibility events and current page state. */
  document: Pick<Document, 'hidden' | 'addEventListener' | 'removeEventListener'>;
  /** Connectivity events. */
  window: Pick<Window, 'addEventListener' | 'removeEventListener'>;
  /** Current connectivity state. */
  navigator: Pick<Navigator, 'onLine'>;
}

/** Poll globally in minutes; cancel on sleep/disposal and revalidate on wake without overlap. */
export function startUsagePolling(read: (signal: AbortSignal) => Promise<unknown>, publish: (value: unknown | Error) => void, environment: UsageEnvironment): () => void {
  let disposed = false;
  let active: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const awake = () => !environment.document.hidden && environment.navigator.onLine;
  const refresh = async () => {
    clearTimeout(timer);
    if (disposed || active || !awake()) return;
    const abort = new AbortController();
    active = abort;
    let delay = 300000;
    try {
      const value = await read(abort.signal);
      if (!disposed && !abort.signal.aborted) {
        publish(value);
        const data = normalizeUsage(value, Date.now() / 1000);
        if (data.codex.status === 'loading' || data.claude.status === 'loading') delay = 15000;
      }
    } catch {
      if (!disposed && !abort.signal.aborted) publish(new Error('connection'));
    } finally {
      if (active === abort) {
        active = null;
        if (!disposed && awake()) timer = setTimeout(() => void refresh(), delay);
      }
    }
  };
  const wake = () => {
    clearTimeout(timer);
    if (!awake()) {
      active?.abort(); active = null;
      if (!environment.navigator.onLine) publish(new Error('connection'));
      return;
    }
    void refresh();
  };
  environment.document.addEventListener('visibilitychange', wake);
  environment.window.addEventListener('online', wake);
  environment.window.addEventListener('offline', wake);
  void refresh();
  return () => {
    disposed = true; clearTimeout(timer); active?.abort();
    environment.document.removeEventListener('visibilitychange', wake);
    environment.window.removeEventListener('online', wake);
    environment.window.removeEventListener('offline', wake);
  };
}
