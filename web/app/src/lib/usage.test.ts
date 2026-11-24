import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeUsage, quotaLabel } from './usage.ts';

test('weekly labels preserve zero and mark expired readings stale without inventing a reset', () => {
  const data = normalizeUsage({ codex: { remainingPercent: 0, resetAt: 2000, updatedAt: 1000, status: 'ok', reason: null }, claude: { remainingPercent: '99', status: 'ok' } }, 2100);
  assert.equal(quotaLabel('Codex', data.codex), 'Codex 0% · vechi');
  assert.equal(quotaLabel('Claude', data.claude), 'Claude —');
});

test('polling pauses hidden/offline, revalidates on wake, and discards results after disposal', async (t) => {
  const { startUsagePolling } = await import('./usage.ts');
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const document = Object.assign(new EventTarget(), { hidden: true });
  const window = new EventTarget();
  const navigator = { onLine: true };
  let reads = 0;
  let resolve: (value: unknown) => void = () => {};
  const results: unknown[] = [];
  const stop = startUsagePolling(() => { reads++; return new Promise(r => { resolve = r; }); }, value => results.push(value), { document, window, navigator });
  assert.equal(reads, 0);
  document.hidden = false;
  document.dispatchEvent(new Event('visibilitychange'));
  window.dispatchEvent(new Event('online'));
  assert.equal(reads, 1);
  resolve({});
  await Promise.resolve(); await Promise.resolve();
  navigator.onLine = false;
  window.dispatchEvent(new Event('offline'));
  t.mock.timers.tick(300000);
  assert.equal(reads, 1);
  navigator.onLine = true;
  window.dispatchEvent(new Event('online'));
  assert.equal(reads, 2);
  stop(); resolve({});
  await Promise.resolve(); await Promise.resolve();
  window.dispatchEvent(new Event('online'));
  assert.equal(reads, 2);
  assert.equal(results.filter(value => !(value instanceof Error)).length, 1);
});

test('losing connectivity immediately marks the last reading unconfirmed', async () => {
  const { startUsagePolling } = await import('./usage.ts');
  const document = Object.assign(new EventTarget(), { hidden: false });
  const window = new EventTarget();
  const navigator = { onLine: true };
  const results: unknown[] = [];
  const stop = startUsagePolling(async () => ({}), value => results.push(value), { document, window, navigator });
  await Promise.resolve();
  navigator.onLine = false;
  window.dispatchEvent(new Event('offline'));
  stop();
  assert.ok(results.at(-1) instanceof Error);
});
