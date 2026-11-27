import { expect, type Locator, type Page } from "@playwright/test";
import { test, changeBoard, login, sends, setBehaviour } from "./helpers";

const CARD = "01JD0000000000000000000012";
function addCard(number: number, title: string, extra: Record<string, unknown> = {}) {
  changeBoard((board) => board.cards.push({ ...board.cards[1], id: `01JD${String(number).padStart(22, "0")}`, number,
    ref: `#${number}@leaseweb`, title, stage: "backlog", checkout: null, blocked_by: [], ...extra }));
}

const column = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const tile = (scope: Page | Locator, title: string) => scope.getByRole("article").filter({ hasText: title });

async function polledAfterThis(page: Page) {
  addCard(90, "Poll marker");
  await expect(tile(page, "Poll marker")).toBeVisible();
}

const REVIEWER = "01JF0000000000000000000003";

function waiting(since: number, reviewer = false) {
  changeBoard((board) => {
    const sessions = board.details[CARD].sessions;
    Object.assign(sessions[0], { status: "needs_feedback", status_changed_at: since });
    if (reviewer) sessions.push({ ...sessions[0], id: REVIEWER, name: "reviewer", role: "review", writer: false });
  });
}

test("a dragged card moves to its column and stays there after the next poll", async ({ page }) => {
  addCard(13, "Drag me along");
  await login(page, "/board");
  await tile(column(page, "Backlog"), "Drag me along").dragTo(column(page, "Review"));
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
  await polledAfterThis(page);
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
  await page.reload();
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
});

test("a poll already in flight when the drag lands does not snap the card back", async ({ page }) => {
  addCard(13, "Drag me along");
  await login(page, "/board");
  await expect(tile(column(page, "Backlog"), "Drag me along")).toBeVisible();
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  let fetched = () => {};
  const inFlight = new Promise<void>((resolve) => { fetched = resolve; });
  let holding = true;
  await page.route("**/api/board?*", async (route) => {
    if (!holding) return route.continue();
    holding = false;
    const response = await route.fetch();
    fetched();
    await held;
    await route.fulfill({ response });
  });
  addCard(14, "Bystander");
  await inFlight;
  const moved = page.waitForResponse((r) => r.url().endsWith("/move"));
  await tile(column(page, "Backlog"), "Drag me along").dragTo(column(page, "Review"));
  expect((await moved).ok()).toBe(true);
  release();
  await expect(tile(page, "Bystander")).toBeVisible();
  expect(await tile(column(page, "Backlog"), "Drag me along").count()).toBe(0);
  await expect(tile(column(page, "Review"), "Drag me along")).toBeVisible();
});

test("a new card lands in the backlog of the current project", async ({ page }) => {
  await login(page, "/board/p/nebula");
  await page.getByRole("button", { name: "Card nou", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Card nou" });
  await dialog.getByLabel("Titlu").fill("Write the release notes");
  await dialog.getByRole("button", { name: "Creează cardul" }).click();
  await expect(dialog).toHaveCount(0);
  await page.reload();
  await expect(tile(column(page, "Backlog"), "Write the release notes")).toBeVisible();
});

test("Run starts the chosen agent and the card shows in doing", async ({ page }) => {
  addCard(13, "Run this one");
  await login(page, "/board/c/13%40leaseweb");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  await expect(dialog.getByRole("radio", { name: /@leaseweb/ })).toBeChecked();
  await expect(dialog.getByRole("radio", { name: "worker" })).toBeChecked();
  await dialog.getByText("Claude", { exact: true }).click();
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(sends().filter((s) => "run" in s)).toEqual([{ run: "#13@leaseweb", role: "worker", kind: "claude", host: "leaseweb", worktree: "new" }]);
  await page.getByRole("button", { name: "Board" }).click();
  await expect(tile(column(page, "În lucru"), "Run this one")).toBeVisible();
});

test("Run defaults to leaseweb when only a laptop reports slots; a refusal shows in the dialog, and another host runs under a new dispatch", async ({ page }) => {
  addCard(13, "Run this one");
  changeBoard((board) => { board.hosts = [{ host: "perseu-mbp", writers: 0, writer_slots: 2, checkouts: [], needs_you: 0, stale: false }]; });
  const bodies: { host: string; dispatch: string }[] = [];
  await page.route("**/api/cards/*/run", async (route) => {
    bodies.push(route.request().postDataJSON());
    if (bodies.length === 1) await route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: "No writer slot on leaseweb" }) });
    else await route.continue();
  });
  await login(page, "/board/c/13%40leaseweb");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  await expect(dialog.getByRole("radio", { name: /@leaseweb/ })).toBeChecked();
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog.getByRole("alert")).toHaveText("No writer slot on leaseweb");
  await dialog.getByText("@perseu-mbp").click();
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(bodies.map((b) => b.host)).toEqual(["leaseweb", "perseu-mbp"]);
  expect(bodies[0].dispatch).not.toBe(bodies[1].dispatch);
});

test("a worker on a host whose writer slots are full starts now, as todo run does, instead of promising to wait", async ({ page }) => {
  addCard(13, "Run on a full host");
  changeBoard((board) => { board.hosts = [{ host: "leaseweb", writers: 2, writer_slots: 2, checkouts: [], needs_you: 0, stale: false }]; });
  await login(page, "/board/c/13%40leaseweb");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  await expect(dialog.getByRole("radio", { name: /@leaseweb/ })).toBeChecked();
  await expect(dialog.getByRole("status")).not.toContainText("așteaptă");
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(sends().filter((s) => "run" in s)).toHaveLength(1);
});

test("a worker on the card's own checkout is told todo run refuses it while that checkout has a writer, apart from the host's advisory count", async ({ page }) => {
  changeBoard((board) => { board.hosts = [{ host: "leaseweb", writers: 2, writer_slots: 2, checkouts: [], needs_you: 0, stale: false }]; });
  await login(page, "/board/c/12%40leaseweb");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  const notes = dialog.getByRole("status");
  await expect(notes).toContainText(["2/2"]);
  await expect(notes).not.toContainText([/refuz/]);
  await dialog.getByText("agent-home-02", { exact: true }).click();
  await expect(notes.filter({ hasText: /refuz/ })).toContainText("agent-home-02");
  await expect(notes.filter({ hasText: /oricum/ })).toHaveCount(0);
});

test("a refused new card shows why inside its dialog", async ({ page }) => {
  await page.route("**/api/cards", (route) => route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "Project not on this board" }) }));
  await login(page, "/board/p/nebula");
  await page.getByRole("button", { name: "Card nou", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Card nou" });
  await dialog.getByLabel("Titlu").fill("Write the release notes");
  await dialog.getByRole("button", { name: "Creează cardul" }).click();
  await expect(dialog.getByRole("alert")).toHaveText("Project not on this board");
});

test("accept puts a proposed card on the board and dismiss removes it", async ({ page }) => {
  addCard(13, "Proposed and kept", { proposed: true });
  addCard(14, "Proposed and dropped", { proposed: true });
  await login(page, "/board");
  const tray = page.getByRole("region", { name: "Carduri propuse" });
  await expect(tile(column(page, "Backlog"), "Proposed and kept")).toHaveCount(0);
  await tray.getByRole("listitem").filter({ hasText: "Proposed and kept" }).getByRole("button", { name: "Acceptă" }).click();
  await expect(tile(column(page, "Backlog"), "Proposed and kept")).toBeVisible();
  await tray.getByRole("listitem").filter({ hasText: "Proposed and dropped" }).getByRole("button", { name: "Respinge" }).click();
  await polledAfterThis(page);
  await expect(tray).toHaveCount(0);
  await expect(page.getByText("Proposed and dropped")).toHaveCount(0);
  await page.reload();
  await expect(tile(column(page, "Backlog"), "Proposed and kept")).toBeVisible();
  await expect(page.getByRole("region", { name: "Carduri propuse" })).toHaveCount(0);
  await expect(page.getByText("Proposed and dropped")).toHaveCount(0);
});

test("a dismissed proposal leaves Toate and its project's count together", async ({ page }) => {
  addCard(14, "Proposed and dropped", { proposed: true });
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/cards/*/dismiss", async (route) => { await held; await route.continue(); });
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await expect(sidebar.getByRole("button", { name: /^Toate proiectele\s*3$/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /nebula\s*3$/ })).toBeVisible();
  await page.getByRole("region", { name: "Carduri propuse" }).getByRole("listitem").filter({ hasText: "Proposed and dropped" })
    .getByRole("button", { name: "Respinge" }).click();
  await expect(sidebar.getByRole("button", { name: /^Toate proiectele\s*2$/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /nebula\s*2$/ })).toBeVisible();
  release();
});

test("on a first visit each waiting session has its own approve button, which answers that session and leaves the strip", async ({ page }) => {
  const waitingSince = 1764000600000;
  waiting(waitingSince, true);
  await login(page, "/board");
  const strip = page.getByRole("region", { name: /Te așteaptă/ });
  await expect(strip.getByRole("button", { name: "Keep verification results" })).toBeVisible();
  const approved = page.waitForResponse((r) => r.url().endsWith("/approve"));
  await strip.getByRole("button", { name: "Aprobă reviewer" }).click();
  const response = await approved;
  expect([response.ok(), response.request().postDataJSON()]).toEqual([true, { session: REVIEWER, host: "leaseweb", expectedStatusAt: waitingSince }]);
  await polledAfterThis(page);
  await expect(strip.getByRole("button", { name: /^Aprobă/ })).toHaveText(["Aprobă worker"]);
  expect(sends().filter((s) => "approve" in s)).toEqual([{ approve: REVIEWER, card: "#12@leaseweb" }]);
});

test("an approved session offers no second approval until its host reports it moved on, and does again once it waits anew", async ({ page }) => {
  const waitingSince = 1764000600000;
  waiting(waitingSince);
  await login(page, "/board");
  const approvals = page.getByRole("button", { name: /^Aprobă/ });
  const approved = page.waitForResponse((r) => r.url().endsWith("/approve"));
  await approvals.click();
  expect((await approved).ok()).toBe(true);
  await polledAfterThis(page);
  await expect(approvals).toHaveCount(0);
  changeBoard((board) => Object.assign(board.details[CARD].sessions[0], { status: "running", status_changed_at: waitingSince + 1000 }));
  addCard(91, "Second poll marker");
  await expect(tile(page, "Second poll marker")).toBeVisible();
  await expect(approvals).toHaveCount(0);
  waiting(waitingSince + 2000);
  await expect(approvals).toHaveText(["Aprobă worker"]);
});

test("the away digest names the waiting card, and after caught up the strip still offers its approval", async ({ page }) => {
  waiting(1764000600000);
  await page.addInitScript(() => localStorage.setItem("nebula-board-last-seen", "1764000000000"));
  await login(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  const waits = digest.locator("section").filter({ has: page.getByRole("heading", { name: /Te așteaptă/ }) });
  await expect(waits.getByRole("button", { name: "Keep verification results" })).toBeVisible();
  await digest.getByRole("button", { name: "Am văzut tot" }).click();
  await polledAfterThis(page);
  await expect(page.getByRole("region", { name: /Te așteaptă/ }).getByRole("button", { name: "Aprobă worker" })).toBeVisible();
});

test("open in code opens on the laptop picked in its dialog and remembers it", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });
  await login(page, "/board/c/12%40leaseweb");
  await page.getByRole("button", { name: "Deschide în cod" }).click();
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  await dialog.getByLabel("Laptop").selectOption("perseu-mbp");
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  expect((await posted).postDataJSON()).toEqual({ session: "01JF0000000000000000000002", host: "leaseweb", on: "perseu-mbp" });
  await expect(page.getByText("Deschis în cod pe @perseu-mbp.")).toBeVisible();
  await page.reload();
  await page.getByRole("button", { name: "Deschide în cod" }).click();
  await expect(dialog.getByLabel("Laptop")).toHaveValue("perseu-mbp");
});

test("open in code shows the gateway's command only when the laptop is unreachable, and any other error as its message", async ({ page }) => {
  const answers = [
    { status: 409, body: { error: "Nebula not reachable on leaseweb", command: "nebula open --session=01JF0000000000000000000002 # from the gateway" } },
    { status: 409, body: { error: "Session is not on this card; refresh the card" } },
  ];
  await page.route("**/api/cards/*/open-code", (route) => {
    const answer = answers.shift()!;
    return route.fulfill({ status: answer.status, contentType: "application/json", body: JSON.stringify(answer.body) });
  });
  await login(page, "/board/c/12%40leaseweb");
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  await page.getByRole("button", { name: "Deschide în cod" }).click();
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  await expect(page.getByText("nebula open --session=01JF0000000000000000000002 # from the gateway")).toBeVisible();
  await expect(page.getByRole("button", { name: "Copiază comanda" })).toBeVisible();
  await page.getByRole("button", { name: "Deschide în cod" }).click();
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  await expect(page.getByRole("alert")).toHaveText("Session is not on this card; refresh the card");
  await expect(page.getByRole("button", { name: "Copiază comanda" })).toHaveCount(0);
});

test("a note typed while the previous one saves stays in the box and saves under a new id", async ({ page }) => {
  await login(page, "/board/c/12%40leaseweb");
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  const bodies: { id: string }[] = [];
  await page.route("**/api/cards/*/note", async (route) => {
    bodies.push(route.request().postDataJSON());
    if (bodies.length === 1) await held;
    await route.continue();
  });
  const box = page.getByLabel("Notă nouă");
  await box.fill("First note");
  const first = page.waitForResponse((r) => r.url().endsWith("/note"));
  await page.getByRole("button", { name: "Adaugă nota" }).click();
  await box.fill("Second draft");
  release();
  expect((await first).ok()).toBe(true);
  await expect(page.getByRole("listitem").filter({ hasText: "First note" })).toBeVisible();
  await expect(box).toHaveValue("Second draft");
  await page.getByRole("button", { name: "Adaugă nota" }).click();
  await expect(page.getByRole("listitem").filter({ hasText: "Second draft" })).toBeVisible();
  expect(new Set(bodies.map((b) => b.id)).size).toBe(2);
});

test("blocking from the card page dims the card, and a note appears in its log", async ({ page }) => {
  addCard(13, "Wait for verification");
  await login(page, "/board/c/13%40leaseweb");
  await page.getByLabel("Blocat de cardul").selectOption({ label: "#12@leaseweb Keep verification results" });
  await page.getByRole("button", { name: "Blochează" }).click();
  await page.getByLabel("Notă nouă").fill("Waits on the contract");
  await page.getByRole("button", { name: "Adaugă nota" }).click();
  await expect(page.getByRole("listitem").filter({ hasText: "Waits on the contract" })).toBeVisible();
  await expect(page.getByRole("button", { name: /#12@leaseweb\s*Keep verification results/ })).toBeVisible();
  await page.getByRole("button", { name: "Board" }).click();
  await expect(tile(page, "Wait for verification")).toHaveCSS("opacity", "0.66");
});
