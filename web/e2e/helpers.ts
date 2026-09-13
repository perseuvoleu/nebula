import fs from "node:fs";
import path from "node:path";
import { expect, type Page } from "@playwright/test";

export const TOKEN = "e2e-synthetic-token-not-a-secret-0123456789";
export const STATE = path.join(__dirname, ".state");
export const SHOTS = path.join(__dirname, "screenshots");

export async function login(page: Page, url = "/") {
  await page.goto(url);
  await page.getByLabel("Token de acces").fill(TOKEN);
  await page.getByRole("button", { name: "Conectare" }).click();
  await expect(page.getByLabel("Proiect").first()).toBeVisible();
}

export function readAgents(): Array<Record<string, unknown> & { id: string; name: string; status: string; path: string; session_id: string | null }> {
  return JSON.parse(fs.readFileSync(path.join(STATE, "agents.json"), "utf8"));
}

export function writeAgents(rows: unknown): void {
  fs.writeFileSync(path.join(STATE, "agents.json"), JSON.stringify(rows, null, 1));
}

export function sends(): Array<Record<string, string>> {
  return fs.readFileSync(path.join(STATE, "sends.log"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

export function setBehaviour(value: "ok" | "fail" | "hang"): void {
  fs.writeFileSync(path.join(STATE, "behaviour"), value);
}

function sessionFile(agentId: string): string {
  const agent = readAgents().find((a) => a.id === agentId)!;
  const dir = path.join(STATE, "pi", "sessions", `--${agent.path.replace(/^\//, "").replace(/[/\\:]/g, "-")}--`);
  return fs.readdirSync(dir).filter((f) => f.endsWith(`_${agent.session_id}.jsonl`)).map((f) => path.join(dir, f))[0];
}

/** Simulate the host agent persisting an entry (what Pi does when a message completes). */
export function appendEntry(agentId: string, message: Record<string, unknown>, id = Math.random().toString(16).slice(2, 10)): void {
  const file = sessionFile(agentId);
  const lines = fs.readFileSync(file, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  const parent = lines.length > 1 ? lines[lines.length - 1].id : null;
  fs.appendFileSync(file, JSON.stringify({ type: "message", id, parentId: parent, timestamp: new Date().toISOString(), message }) + "\n");
}

export const assistantText = (text: string) => ({ role: "assistant", content: [{ type: "text", text }], api: "x", provider: "p", model: "m", usage: {}, stopReason: "stop", timestamp: Date.now() });
export const userText = (text: string) => ({ role: "user", content: text, timestamp: Date.now() });

export function setStatus(agentId: string, status: string): void {
  const rows = readAgents();
  rows.find((a) => a.id === agentId)!.status = status;
  writeAgents(rows);
}

export async function noHorizontalOverflow(page: Page) {
  const overflow = await page.evaluate(() => ({
    doc: document.documentElement.scrollWidth - window.innerWidth,
    body: document.body.scrollWidth - window.innerWidth,
  }));
  expect(overflow.doc, "document must not scroll horizontally").toBeLessThanOrEqual(0);
  expect(overflow.body, "body must not scroll horizontally").toBeLessThanOrEqual(0);
}

export async function shot(page: Page, name: string) {
  fs.mkdirSync(SHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SHOTS, `${name}.png`), fullPage: false });
}
