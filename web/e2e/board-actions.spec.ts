import fs from "node:fs";
import path from "node:path";
import { expect, type Locator, type Page } from "@playwright/test";
import { test, STATE, TOKEN, sends, setBehaviour } from "./helpers";

type Session = { id: string; status: string; status_changed_at: number };
type Board = {
  version: number;
  cards: Record<string, unknown>[];
  away: { needs_you: Record<string, unknown>[] };
  details: Record<string, { sessions: Session[] }>;
};

const CARD = "01JD0000000000000000000012";
const readBoard = (): Board => JSON.parse(fs.readFileSync(path.join(STATE, "board.json"), "utf8"));

function writeBoard(change: (board: Board) => void) {
  const board = readBoard();
  change(board);
  board.version += 1;
  const temporary = path.join(STATE, ".board.json.actions");
  fs.writeFileSync(temporary, JSON.stringify(board));
  fs.renameSync(temporary, path.join(STATE, "board.json"));
}

function addCard(number: number, title: string, extra: Record<string, unknown> = {}) {
  writeBoard((board) => board.cards.push({ ...board.cards[1], id: `01JD${String(number).padStart(22, "0")}`, number,
    ref: `#${number}@leaseweb`, title, stage: "backlog", checkout: null, blocked_by: [], ...extra }));
}

const cardNumbered = (number: number) => readBoard().cards.find((c) => c.number === number)!;

async function open(page: Page, url: string) {
  await page.goto(url);
  await page.getByLabel("Token de acces").fill(TOKEN);
  await page.getByRole("button", { name: "Conectare" }).click();
  await expect(page.getByLabel("Token de acces")).toHaveCount(0);
}

const column = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const tile = (scope: Page | Locator, title: string) => scope.getByRole("article").filter({ hasText: title });
const nextPoll = (page: Page) => page.waitForResponse((r) => r.url().includes("/api/board?") && r.ok());

test("a dragged card moves to its column and stays there after the next poll", async ({ page }) => {
  addCard(13, "Drag me along");
  await open(page, "/board");
  await tile(column(page, "Backlog"), "Drag me along").dragTo(column(page, "Review"));
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
  await nextPoll(page);
  await nextPoll(page);
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
  expect(cardNumbered(13).stage).toBe("review");
});

test("a new card lands in the backlog of the current project", async ({ page }) => {
  await open(page, "/board/p/nebula");
  await page.getByRole("button", { name: "Card nou", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Card nou" });
  await dialog.getByLabel("Titlu").fill("Write the release notes");
  await dialog.getByRole("button", { name: "Creează cardul" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(tile(column(page, "Backlog"), "Write the release notes")).toBeVisible();
  const created = readBoard().cards.find((c) => c.title === "Write the release notes")!;
  expect([created.project, created.stage]).toEqual(["nebula", "backlog"]);
});

test("Run starts the chosen agent and the card shows in doing", async ({ page }) => {
  addCard(13, "Run this one");
  await open(page, "/board/c/13%40leaseweb");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  await expect(dialog.getByRole("radio", { name: /@leaseweb/ })).toBeChecked();
  await dialog.getByText("review", { exact: true }).click();
  await dialog.getByText("Claude", { exact: true }).click();
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(sends().filter((s) => "run" in s)).toEqual([{ run: "#13@leaseweb", role: "review", kind: "claude", host: "leaseweb", worktree: "new" }]);
  await page.getByRole("button", { name: "Board" }).click();
  await expect(tile(column(page, "În lucru"), "Run this one")).toBeVisible();
});

test("accept puts a proposed card on the board and dismiss removes it", async ({ page }) => {
  addCard(13, "Proposed and kept", { proposed: true });
  addCard(14, "Proposed and dropped", { proposed: true });
  await open(page, "/board");
  const tray = page.getByRole("region", { name: "Carduri propuse" });
  await expect(tile(column(page, "Backlog"), "Proposed and kept")).toHaveCount(0);
  await tray.getByRole("listitem").filter({ hasText: "Proposed and kept" }).getByRole("button", { name: "Acceptă" }).click();
  await expect(tile(column(page, "Backlog"), "Proposed and kept")).toBeVisible();
  await tray.getByRole("listitem").filter({ hasText: "Proposed and dropped" }).getByRole("button", { name: "Respinge" }).click();
  await expect(tray).toHaveCount(0);
  await nextPoll(page);
  await expect(page.getByText("Proposed and dropped")).toHaveCount(0);
  expect([cardNumbered(13).proposed, cardNumbered(14).archived]).toEqual([false, true]);
});

test("approve in the needs-you strip sends the approval and the card leaves the strip", async ({ page }) => {
  const waitingSince = 1764000600000;
  writeBoard((board) => {
    Object.assign(board.details[CARD].sessions[0], { status: "needs_feedback", status_changed_at: waitingSince });
    board.away.needs_you = [{ host: "leaseweb", id: "01JF0000000000000000000002", card: CARD, status_changed_at: waitingSince, at: waitingSince }];
  });
  await page.addInitScript(() => localStorage.setItem("nebula-board-last-seen", "1764000000000"));
  await open(page, "/board");
  const strip = page.getByRole("region", { name: /Te așteaptă/ });
  await expect(strip.getByRole("button", { name: "Keep verification results" })).toBeVisible();
  const approved = page.waitForResponse((r) => r.url().endsWith("/approve"));
  await strip.getByRole("button", { name: "Aprobă" }).click();
  expect((await approved).ok()).toBe(true);
  await nextPoll(page);
  await expect(strip).toHaveCount(0);
  expect(readBoard().details[CARD].sessions[0].status).toBe("running");
});

test("open in code that cannot reach the laptop shows the command to copy", async ({ page }) => {
  setBehaviour("fail");
  await open(page, "/board/c/12%40leaseweb");
  await page.getByRole("button", { name: "Deschide în cod" }).click();
  await expect(page.getByText("nebula open --session=01JF0000000000000000000002")).toBeVisible();
  await expect(page.getByRole("button", { name: "Copiază comanda" })).toBeVisible();
});

test("blocking from the card page dims the card, and a note appears in its log", async ({ page }) => {
  addCard(13, "Wait for verification");
  await open(page, "/board/c/13%40leaseweb");
  await page.getByLabel("Blocat de cardul").selectOption({ label: "#12@leaseweb Keep verification results" });
  await page.getByRole("button", { name: "Blochează" }).click();
  await page.getByLabel("Notă nouă").fill("Waits on the contract");
  await page.getByRole("button", { name: "Adaugă nota" }).click();
  await expect(page.getByRole("listitem").filter({ hasText: "Waits on the contract" })).toBeVisible();
  await expect(page.getByRole("button", { name: /#12@leaseweb\s*Keep verification results/ })).toBeVisible();
  await page.getByRole("button", { name: "Board" }).click();
  await expect(tile(page, "Wait for verification")).toHaveCSS("opacity", "0.6");
});
