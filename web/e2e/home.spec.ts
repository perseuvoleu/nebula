import { expect, type Page } from "@playwright/test";
import { test, changeBoard, login, noHorizontalOverflow, sends, shot, TOKEN } from "./helpers";

const CARD12 = "01JD0000000000000000000012";
const WORKER = "01JF0000000000000000000002";

function waitingBoard() {
  changeBoard((board) => {
    Object.assign(board.details[CARD12].sessions[0], { status: "needs_feedback", status_changed_at: 1764000600000 });
    const base = board.cards.find((c) => c.id === CARD12)!;
    const extra = [
      [13, "Move RAG replicator from findl to leaseweb", "backlog"],
      [14, "Add backpressure metrics", "backlog"],
      [15, "Nightly eval latency spike", "next"],
      [16, "View tabs: DECISIONS + artifacts", "doing"],
      [17, "Stripe webhook retries", "review"],
    ] as const;
    for (const [number, title, stage] of extra) {
      board.cards.push({ ...base, id: `01JD00000000000000000000${number}`, number, ref: `#${number}@leaseweb`, title, stage, checkout: null, blocked_by: [] });
    }
    board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000099", title: "Keep verification results in the daemon", card: CARD12,
      content: { body: "Results outlive the checkout" }, created_at: 1764000500000 });
  });
}

const needs = (page: Page) => page.getByRole("region", { name: "Te așteaptă" });

test("HOME is /: NEEDS, the stage list, the selected card with its session chain, actions, artifacts and log, and the decisions rail", async ({ page }) => {
  waitingBoard();
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page, "/");
  await expect(page.getByRole("navigation", { name: "Taburi" }).getByRole("link", { name: /HOME/ })).toHaveAttribute("aria-current", "page");
  await expect(needs(page).getByRole("button", { name: /Keep verification results/ })).toBeVisible();
  const card = page.getByRole("region", { name: "Card #12@leaseweb" });
  await expect(card.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
  await expect(card.getByRole("list", { name: "Lanț de sesiuni" }).getByRole("button", { name: /worker/ })).toHaveAttribute("aria-current", "step");
  await expect(card.getByRole("button", { name: /aprobă worker/ })).toBeVisible();
  await expect(card.getByRole("button", { name: /deschide în cod/ })).toBeVisible();
  await expect(card.getByRole("button", { name: /artefacte \(1\)/ })).toBeVisible();
  await expect(card.getByRole("region", { name: "Artefacte" }).getByRole("button", { name: /board\.spec/ })).toBeVisible();
  await expect(card.getByRole("region", { name: "Jurnal" })).toContainText("Keep verification results in the daemon");
  await expect(card.getByRole("region", { name: "Jurnal" })).toContainText("Started on leaseweb");
  const rail = page.getByRole("complementary", { name: "Context și decizii" });
  await expect(rail).toContainText("Copy published files");
  await expect(page.getByRole("region", { name: "Backlog" }).getByRole("button")).toHaveCount(2);
  await expect(page.getByLabel("1 te așteaptă")).toBeVisible();
  await shot(page, "home-1440");
});

test("tab keys 1–4 switch HOME, CODE, RAG and DECISIONS; ? lists the keys; b opens the kanban, which still works at /board", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page, "/");
  await expect(page.getByRole("region", { name: /^Card / })).toBeVisible();
  await page.keyboard.press("2");
  await expect(page).toHaveURL(/\/p\/alpha$/);
  await expect(page.getByRole("combobox", { name: "Proiect", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /Refactor auth/ })).toBeVisible();
  await shot(page, "code-1440");
  await page.keyboard.press("3");
  await expect(page).toHaveURL(/\/rag$/);
  await expect(page.getByText(/În curând/)).toBeVisible();
  await shot(page, "rag-1440");
  await page.keyboard.press("4");
  await expect(page).toHaveURL(/\/decisions$/);
  await expect(page.getByRole("heading", { name: "Decizii" })).toBeVisible();
  await expect(page.getByRole("article").filter({ hasText: "Copy published files" })).toBeVisible();
  await shot(page, "decisions-1440");
  await page.keyboard.press("1");
  await expect(page).toHaveURL(/\/$/);
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "? Taste" })).toContainText("HOME / CODE / RAG / DECISIONS");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.keyboard.press("b");
  await expect(page).toHaveURL(/\/board$/);
  await expect(page.getByRole("heading", { name: "Nebula Board" })).toBeVisible();
  await page.getByRole("button", { name: "Keep verification results" }).click();
  await expect(page).toHaveURL(/peek=12%40leaseweb|\/board\/c\/12%40leaseweb/);
});

test("a NEEDS item selects its card; j/k move the selection", async ({ page }) => {
  waitingBoard();
  await login(page, "/");
  await page.getByRole("button", { name: "Stripe webhook retries" }).click();
  await expect(page).toHaveURL(/\/c\/17%40leaseweb$/);
  await expect(page.getByRole("region", { name: "Card #17@leaseweb" })).toBeVisible();
  await needs(page).getByRole("button", { name: /Keep verification results/ }).click();
  await expect(page).toHaveURL(/\/c\/12%40leaseweb$/);
  await expect(page.getByRole("region", { name: "Card #12@leaseweb" })).toBeVisible();
  await page.keyboard.press("j");
  await expect(page).toHaveURL(/\/c\/13%40leaseweb$/);
  await page.keyboard.press("k");
  await expect(page).toHaveURL(/\/c\/12%40leaseweb$/);
});

test("approve from HOME answers the waiting session and clears NEEDS", async ({ page }) => {
  waitingBoard();
  await login(page, "/c/12%40leaseweb");
  await page.getByRole("region", { name: "Card #12@leaseweb" }).getByRole("button", { name: /aprobă worker/ }).click();
  await expect(needs(page)).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: "aprobat worker" })).toBeVisible();
  await expect.poll(() => sends().filter((s) => s.approve === WORKER)).toHaveLength(1);
});

test("open in code from HOME: a session on another laptop asks where to open it (button and Enter)", async ({ page }) => {
  await login(page, "/");
  const card = page.getByRole("region", { name: "Card #12@leaseweb" });
  await card.getByRole("button", { name: /deschide în cod/ }).click();
  await expect(page.getByRole("dialog", { name: /Deschide în cod #12@leaseweb/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.locator("body").press("Enter");
  await expect(page.getByRole("dialog", { name: /Deschide în cod #12@leaseweb/ })).toBeVisible();
});

test("open in code from HOME: a session this gateway lists opens in CODE, shows its latest message on HOME, and CODE still sends", async ({ page }) => {
  const host = await (await page.request.get("/api/projects", { headers: { Authorization: `Bearer ${TOKEN}` } })).json().then((d) => d.host as string);
  const card = { id: "01JD0000000000000000000077", number: 77, ref: "#77", title: "Refactor auth card", stage: "doing", project: "alpha", proposed: false, archived: false,
    created_by: { host, agent: null, name: "you" }, checkout: { host, branch: "main", path: "/srv/alpha", worktree: false }, blocked_by: [], cleared_blockers: [], created_at: 1, updated_at: 1 };
  const session = { host, id: "pi-1", name: "Refactor auth", role: "worker", kind: "pi", status: "running", status_changed_at: 1, writer: true, alive: true, stale: false };
  await page.route("**/api/board?*", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/board", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/cards/77", (route) => route.fulfill({ json: { ...card, sessions: [session], artifacts: [], log: [] } }));
  await login(page, "/");
  const pane = page.getByRole("region", { name: "Card #77" });
  await expect(pane).toContainText("Da — am adăugat `test_expired_token` și trece.");
  await pane.getByRole("button", { name: /deschide în cod/ }).click();
  await expect(page).toHaveURL(/\/p\/alpha\/s\/pi-1$/);
  const composer = page.getByLabel("Mesaj către agent");
  await composer.fill("din HOME în CODE");
  await page.getByRole("button", { name: "Trimite" }).click();
  await expect.poll(() => sends().some((s) => s.text === "din HOME în CODE")).toBe(true);
});

test("an artifact opens rendered from HOME, by click and by v", async ({ page }) => {
  await login(page, "/");
  const card = page.getByRole("region", { name: "Card #12@leaseweb" });
  await card.getByRole("region", { name: "Artefacte" }).getByRole("button", { name: /board\.spec/ }).click();
  const viewer = page.getByRole("dialog");
  await expect(viewer.getByRole("region", { name: "board.spec" })).toBeVisible();
  await expect(viewer).toContainText("drag a card to doing");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.keyboard.press("v");
  await expect(page.getByRole("dialog").getByRole("region", { name: "board.spec" })).toBeVisible();
});

test("390 px: tabs on top, HOME is the list, a card opens full screen and Back returns", async ({ page }) => {
  waitingBoard();
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/");
  await expect(page.getByRole("navigation", { name: "Taburi" })).toBeVisible();
  await expect(needs(page).getByRole("button", { name: /Keep verification results/ })).toBeVisible();
  await expect(page.getByRole("region", { name: /^Card / })).toHaveCount(0);
  await noHorizontalOverflow(page);
  await shot(page, "home-390");
  await needs(page).getByRole("button", { name: /Keep verification results/ }).click();
  const card = page.getByRole("region", { name: "Card #12@leaseweb" });
  await expect(card.getByRole("region", { name: "Artefacte" })).toBeVisible();
  await expect(needs(page)).toHaveCount(0);
  await noHorizontalOverflow(page);
  await shot(page, "home-390-card");
  await card.getByRole("button", { name: "Înapoi" }).click();
  await expect(needs(page)).toBeVisible();
  await expect(page).toHaveURL(/\/$/);
});
