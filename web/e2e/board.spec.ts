import fs from "node:fs";
import path from "node:path";
import { expect, type Locator, type Page } from "@playwright/test";
import { test, STATE, TOKEN, noHorizontalOverflow, setBehaviour, shot } from "./helpers";

type Board = { version: number; cards: Record<string, unknown>[]; hosts: { needs_you: number }[];
  away: { finished: { id: unknown; at: number }[] }; details: Record<string, { sessions: Record<string, unknown>[] }> };

const readBoard = (): Board => JSON.parse(fs.readFileSync(path.join(STATE, "board.json"), "utf8"));

function writeBoard(change: (board: Board) => void) {
  const board = readBoard();
  change(board);
  board.version += 1;
  const temporary = path.join(STATE, ".board.json.spec");
  fs.writeFileSync(temporary, JSON.stringify(board));
  fs.renameSync(temporary, path.join(STATE, "board.json"));
}

function addCard(number: number, title: string, stage: string, blockedBy: string[] = []) {
  writeBoard((board) => board.cards.push({ ...board.cards[1], id: `01JD${String(number).padStart(22, "0")}`, number,
    ref: `#${number}@leaseweb`, title, stage, checkout: null, blocked_by: blockedBy }));
}

async function open(page: Page, url: string) {
  await page.goto(url);
  await page.getByLabel("Token de acces").fill(TOKEN);
  await page.getByRole("button", { name: "Conectare" }).click();
  await expect(page.getByLabel("Token de acces")).toHaveCount(0);
}

async function awayFrom(page: Page, at = 1764000000000) {
  await page.addInitScript((seen) => localStorage.setItem("nebula-board-last-seen", String(seen)), at);
}

const column = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const tile = (scope: Page | Locator, title: string) => scope.getByRole("article").filter({ hasText: title });

test("columns and table", async ({ page }) => {
  await open(page, "/board");
  await expect(page.getByRole("heading", { name: "Board", level: 1 })).toBeVisible();
  for (const name of ["Backlog", "Urmează", "În lucru", "Review", "Gata"]) await expect(column(page, name)).toBeVisible();
  await expect(tile(column(page, "În lucru"), "Keep verification results")).toBeVisible();
  await expect(tile(column(page, "Gata"), "Pin the board contract")).toBeVisible();
  await page.getByRole("button", { name: "Tabel" }).click();
  await expect(page).toHaveURL(/\/board\?view=table$/);
  const table = page.getByRole("table");
  await expect(table.getByRole("row").filter({ hasText: "Keep verification results" })).toBeVisible();
  await expect(table.getByRole("row").filter({ hasText: "Pin the board contract" })).toBeVisible();
});

test("peek and card page", async ({ page }) => {
  writeBoard((board) => board.details["01JD0000000000000000000012"].sessions.push(
    { ...board.details["01JD0000000000000000000012"].sessions[0], id: "01JF0000000000000000000003", name: "reviewer", role: "review", status: "fresh", writer: false }));
  await open(page, "/board?peek=12%40leaseweb");
  const peek = page.getByRole("dialog", { name: "Keep verification results" });
  await expect(peek.getByText("Started on leaseweb")).toBeVisible();
  await peek.getByRole("link", { name: "Deschide pagina" }).click();
  await expect(page).toHaveURL(/\/board\/c\/12%40leaseweb$/);
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Sesiuni" }).getByRole("listitem")).toHaveText([/worker.*worker/, /reviewer.*review/]);
  await expect(page.getByText("agent-home-02", { exact: true })).toBeVisible();
  await expect(page.getByText("@leaseweb").first()).toBeVisible();
});

test("a card without session data makes no status claim, and needs-you stays hidden", async ({ page }) => {
  writeBoard((board) => { board.hosts[0].needs_you = 1; });
  await open(page, "/board");
  await expect(tile(page, "Keep verification results")).toBeVisible();
  await expect(tile(page, "Keep verification results").getByRole("img")).toHaveCount(0);
  await expect(page.getByRole("navigation", { name: "Proiecte" }).getByRole("button", { name: /Te așteaptă/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Tabel" }).click();
  const row = page.getByRole("row").filter({ hasText: "Keep verification results" });
  await expect(row).toBeVisible();
  await expect(row.getByRole("img")).toHaveCount(0);
});

test("the away digest lists only the scope's project and shows only what it lists", async ({ page }) => {
  await awayFrom(page);
  await open(page, "/board/p/nebula");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest.getByRole("button", { name: "Keep verification results" })).toBeVisible();
  await expect(digest.getByText("Copy published files")).toBeVisible();
  await expect(digest.getByText("Te așteaptă")).toHaveCount(0);
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await sidebar.getByRole("button", { name: /Liniștite/ }).click();
  await sidebar.getByRole("button", { name: /vela\s*0$/ }).click();
  await expect(page).toHaveURL(/\/board\/p\/vela$/);
  await expect(tile(page, "Keep verification results")).toHaveCount(0);
  await expect(digest).toHaveCount(0);
});

test("at 390 px a long away digest is capped and leaves the board in view", async ({ page }) => {
  for (let number = 20; number < 50; number++) addCard(number, `Finished ${number}`, "done");
  writeBoard((board) => board.away.finished.push(...board.cards.filter((c) => c.stage === "done").map((c) => ({ id: c.id, at: 1764000500000 }))));
  await page.setViewportSize({ width: 390, height: 844 });
  await awayFrom(page);
  await open(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest.getByRole("button", { name: "Finished 20" })).toBeVisible();
  await expect(column(page, "Backlog")).toBeInViewport();
  await digest.getByRole("button", { name: /Arată toate/ }).click();
  await expect(digest.getByRole("button", { name: "Finished 49" })).toBeVisible();
});

test("a project's Decisions route lists only that project's decisions", async ({ page }) => {
  const listed = page.waitForResponse((r) => r.url().includes("/api/decisions"));
  await open(page, "/board/decisions/p/vela");
  await listed;
  await expect(page.getByRole("heading", { name: "Decizii" })).toBeVisible();
  await expect(page.getByText("Copy published files")).toHaveCount(0);
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await sidebar.getByRole("button", { name: "Checkouturi nebula" }).click();
  await sidebar.getByRole("button", { name: "Decizii", exact: true }).click();
  await expect(page).toHaveURL(/\/board\/decisions\/p\/nebula$/);
  await expect(page.getByText("Copy published files")).toBeVisible();
});

test("a failed card read is retried on the next poll, and a stale detail is marked stale", async ({ page }) => {
  let fail = true;
  await page.route("**/api/cards/**", (route) => fail ? route.fulfill({ status: 503, body: "{}" }) : route.continue());
  await open(page, "/board?peek=12%40leaseweb");
  const peek = page.getByRole("dialog", { name: "Keep verification results" });
  await expect(peek.getByRole("alert")).toBeVisible();
  fail = false;
  await expect(peek.getByText("Started on leaseweb")).toBeVisible();
  await expect(peek.getByRole("status")).toHaveCount(0);
  fail = true;
  addCard(14, "Bumps the version", "backlog");
  await expect(peek.getByRole("status").filter({ hasText: "Date neactualizate" })).toBeVisible();
  await expect(peek.getByText("Started on leaseweb")).toBeVisible();
});

test("blocked card dimmed", async ({ page }) => {
  addCard(13, "Wait for verification", "next", ["#12@leaseweb"]);
  await open(page, "/board");
  await expect(tile(page, "Wait for verification")).toHaveCSS("opacity", "0.6");
  await expect(tile(page, "Keep verification results")).toHaveCSS("opacity", "1");
});

test("sidebar counts and quiet projects", async ({ page }) => {
  await open(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await expect(sidebar.getByRole("button", { name: /nebula\s*2$/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /^Toate proiectele\s*2$/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /vela/ })).toHaveCount(0);
  await sidebar.getByRole("button", { name: /Liniștite/ }).click();
  await expect(sidebar.getByRole("button", { name: /vela\s*0$/ })).toBeVisible();
  await sidebar.getByRole("button", { name: "Checkouturi nebula" }).click();
  await expect(sidebar.getByRole("button", { name: /^main/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /^agent-home-02/ })).toBeVisible();
  await sidebar.getByRole("button", { name: /nebula\s*2$/ }).click();
  await expect(page).toHaveURL(/\/board\/p\/nebula$/);
});

test("poll picks up a new version", async ({ page }) => {
  await open(page, "/board");
  await expect(tile(page, "Keep verification results")).toBeVisible();
  const unchanged = await page.waitForResponse(async (r) => r.url().includes("/api/board?since=") && (await r.json()).unchanged === true);
  expect(unchanged.ok()).toBe(true);
  await expect(tile(page, "Keep verification results")).toBeVisible();
  addCard(14, "Arrived by poll", "backlog");
  await expect(tile(column(page, "Backlog"), "Arrived by poll")).toBeVisible();
  setBehaviour("fail");
  await expect(page.getByRole("status").filter({ hasText: "Date neactualizate" })).toBeVisible();
  await expect(tile(page, "Arrived by poll")).toBeVisible();
  setBehaviour("ok");
  await expect(page.getByRole("status").filter({ hasText: "Date neactualizate" })).toHaveCount(0);
});

test("board, peek and card page are usable at 390 px", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await open(page, "/board");
  await expect(column(page, "Backlog")).toBeInViewport();
  await tile(page, "Keep verification results").scrollIntoViewIfNeeded();
  await expect(tile(page, "Keep verification results")).toBeInViewport();
  await noHorizontalOverflow(page);
  await shot(page, "board-390");
  await tile(page, "Keep verification results").getByRole("button", { name: "Keep verification results" }).click();
  await expect(page).toHaveURL(/peek=12%40leaseweb/);
  const peek = page.getByRole("dialog", { name: "Keep verification results" });
  await expect(peek.getByText("Started on leaseweb")).toBeInViewport();
  await noHorizontalOverflow(page);
  await shot(page, "board-peek-390");
  await peek.getByRole("link", { name: "Deschide pagina" }).click();
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeInViewport();
  await noHorizontalOverflow(page);
  await shot(page, "board-card-390");
  await page.getByRole("button", { name: "Meniu" }).click();
  await expect(page.getByRole("navigation", { name: "Proiecte" })).toBeVisible();
});
