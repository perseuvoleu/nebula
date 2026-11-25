import fs from "node:fs";
import path from "node:path";
import { expect } from "@playwright/test";
import { test, login, noHorizontalOverflow, readAgents, writeAgents, STATE } from "./helpers";

test("phone loads older Claude conversation beyond the terminal screen and keeps live following", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const sid = "dddddddd-4444-4444-8444-dddddddddddd";
  const rows = readAgents();
  const agent = rows.find(row => row.id === "claude-1")!;
  agent.session_id = sid;
  agent.id = "claude-history";
  writeAgents(rows);
  const directory = path.join(STATE, "claude", "projects", "-srv-alpha-wt");
  fs.mkdirSync(directory, { recursive: true });
  const file = path.join(directory, `${sid}.jsonl`);
  const entry = (n: number) => JSON.stringify({ type: "assistant", uuid: `reply-${n}`,
    parentUuid: n ? `reply-${n - 1}` : null, sessionId: sid, cwd: "/srv/alpha-wt",
    timestamp: "2026-10-02T15:00:00Z", message: { role: "assistant", content: [
      { type: "text", text: `Răspunsul **${n}**: detalii păstrate în conversație.\n\n- Prima opțiune\n- A doua opțiune` },
      { type: "thinking", thinking: "PRIVATE REASONING" },
    ] } }) + "\n";
  fs.writeFileSync(file, Array.from({ length: 80 }, (_, n) => entry(n)).join(""));
  const firstResponse = page.waitForResponse(response => /\/api\/conversation\?/.test(response.url()));
  await login(page, "/p/alpha/s/claude-history");
  const initial = await (await firstResponse).json();
  expect(initial.capabilities.structured).toBe(true);
  expect(initial.snapshot?.messages.length).toBeLessThanOrEqual(30);
  expect(initial.snapshot?.hasMore).toBe(true);
  const log = page.getByRole("log");
  await expect(log.locator("strong", { hasText: /^79$/ })).toBeInViewport();
  await expect(log.locator("strong", { hasText: /^0$/ })).toHaveCount(0);
  await expect(log).not.toContainText("PRIVATE REASONING");
  const scroller = log.locator("> div").first();
  const before = await scroller.evaluate(el => {
    el.scrollTop = 0;
    const first = [...el.querySelectorAll("strong")].find(s => /^[0-9]+$/.test(s.textContent ?? ""))!;
    return { number: first.textContent, top: first.getBoundingClientRect().top };
  });
  await expect(log.locator("strong", { hasText: /^20$/ })).toBeAttached();
  const after = await log.locator("strong", { hasText: new RegExp(`^${before.number}$`) }).evaluate(el => el.getBoundingClientRect().top);
  expect(Math.abs(after - before.top), `message ${before.number} stays where it was when older messages load above it`).toBeLessThan(2);
  fs.appendFileSync(file, entry(80));
  await expect(log.locator("strong", { hasText: /^80$/ })).toBeAttached();
  await expect(log.locator("strong", { hasText: /^80$/ })).not.toBeInViewport();
  await scroller.evaluate(el => { el.scrollTop = 0; });
  await expect(log.locator("strong", { hasText: /^0$/ })).toBeAttached();
  await expect(page.getByRole("button", { name: "Încarcă mesaje mai vechi" })).toHaveCount(0);
  await scroller.evaluate(el => { el.scrollTop = el.scrollHeight; });
  fs.appendFileSync(file, entry(81));
  await expect(log.locator("strong", { hasText: /^81$/ })).toBeInViewport();
  await expect(log.locator("strong", { hasText: /^0$/ })).toHaveCount(1);
  await noHorizontalOverflow(page);
  await page.getByRole("button", { name: "Arată terminalul" }).click();
  await expect(page.getByLabel("Output de terminal", { exact: true })).toBeVisible();
});
