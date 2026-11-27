import { expect, type Locator, type Page } from "@playwright/test";
import { test, changeBoard, login, sends, setBehaviour, TOKEN } from "./helpers";

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

async function contrast(locator: Locator): Promise<number> {
  return locator.evaluate((el) => {
    const parse = (c: string) => (c.match(/[\d.]+/g) ?? []).map(Number);
    const mix = (top: number[], under: number[]) => { const a = top[3] ?? 1; return [0, 1, 2].map((i) => top[i] * a + under[i] * (1 - a)); };
    const layers: number[][] = [];
    let opacity = 1;
    for (let n: Element | null = el; n; n = n.parentElement) {
      const s = getComputedStyle(n);
      opacity *= Number(s.opacity);
      const bg = parse(s.backgroundColor);
      if ((bg[3] ?? 1) > 0) layers.push(bg);
      if ((bg[3] ?? 1) === 1) break;
    }
    const bg = layers.reverse().reduce((under, top) => mix(top, under), [0, 0, 0]);
    const fg = mix([...mix(parse(getComputedStyle(el).color), bg), opacity], bg);
    const lum = (c: number[]) => { const [r, g, b] = c.map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const [hi, lo] = [lum(fg), lum(bg)].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
  });
}


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
  const blocked = tile(page, "Wait for verification");
  await expect(blocked).toHaveCSS("border-top-style", "dashed");
  for (const text of [blocked.getByText("#13"), blocked.getByRole("button", { name: "Wait for verification" })]) expect(await contrast(text)).toBeGreaterThanOrEqual(4.5);
});

test("open in code sends the session that was waiting when its dialog opened, even after a poll replaces the wait", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });
  waiting(1764000600000);
  await login(page, "/board");
  const strip = page.getByRole("region", { name: /Te așteaptă/ });
  await strip.getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  await expect(dialog).toBeVisible();
  const polled = page.waitForResponse((r) => r.url().includes("/api/board?"));
  changeBoard((board) => {
    const sessions = board.details[CARD].sessions;
    Object.assign(sessions[0], { status: "running", status_changed_at: 1764000700000 });
    sessions.push({ ...sessions[0], id: REVIEWER, name: "reviewer", role: "review", writer: false, status: "needs_feedback", status_changed_at: 1764000700000 });
  });
  await polled;
  await expect(page.getByRole("button", { name: "Aprobă reviewer", includeHidden: true })).toHaveCount(1);
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  expect((await posted).postDataJSON()).toMatchObject({ session: "01JF0000000000000000000002", host: "leaseweb" });
});

test("a tile's Open in code finds the card's live session itself, and a tile reports the result in one line", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });
  await page.route("**/api/cards/*/open-code", (route) => route.fulfill({ status: 409, contentType: "application/json",
    body: JSON.stringify({ error: "Nebula not reachable on perseu-mbp", command: "nebula open --session=01JF0000000000000000000002 # long command from the gateway" }) }));
  await login(page, "/board");
  const card = tile(page, "Keep verification results");
  await card.getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  expect((await posted).postDataJSON()).toMatchObject({ session: "01JF0000000000000000000002", host: "leaseweb" });
  const recovery = page.getByRole("link", { name: /pagina cardului/ });
  await expect(recovery).toBeVisible();
  await expect(page.getByText(/nebula open --session/)).toHaveCount(0);
  const box = await card.boundingBox();
  const column = await page.getByRole("region", { name: "În lucru", exact: true }).boundingBox();
  expect(box!.x + box!.width).toBeLessThanOrEqual(column!.x + column!.width);
});

test("closing a tile's Run dialog puts focus back on its Pornește button", async ({ page }) => {
  await login(page, "/board");
  const run = page.getByRole("button", { name: "Pornește #12@leaseweb" });
  await run.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog", { name: "Pornește un agent pe card" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(run).toBeFocused();
});

const withWait = (session: number) => changeBoard((board) => {
  const sessions = board.details[CARD].sessions;
  sessions.push({ ...sessions[0], id: REVIEWER, host: "findl", name: "reviewer", role: "review", writer: false, status: "needs_feedback", status_changed_at: 1764000700000 + session });
});

test("a tile with a known wait opens the waiting session, as the strip and the card page do", async ({ page }) => {
  withWait(0);
  await login(page, "/board");
  const opened: unknown[] = [];
  page.on("request", (r) => { if (r.url().endsWith("/open-code")) opened.push(r.postDataJSON()); });
  const confirm = async () => {
    const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
    await page.getByRole("dialog", { name: "Deschide în cod" }).getByRole("button", { name: "Deschide", exact: true }).click();
    await posted;
  };
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await confirm();
  await page.getByRole("region", { name: /Te așteaptă/ }).getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await confirm();
  await page.goto("/board/c/12%40leaseweb");
  await page.getByRole("region", { name: "Acțiuni" }).getByRole("button", { name: "Deschide în cod" }).click();
  await confirm();
  expect(opened).toEqual([0, 1, 2].map(() => expect.objectContaining({ session: REVIEWER, host: "findl" })));
});

test("focus comes back to Open in code after confirming it, and after a lookup that finds no session", async ({ page }) => {
  addCard(13, "Started without sessions", { stage: "doing" });
  changeBoard((board) => { board.details["01JD0000000000000000000013"] = { ...board.details[CARD], sessions: [] }; });
  await page.route("**/api/cards/*/open-code", async (route) => { await new Promise((r) => setTimeout(r, 600)); await route.fulfill({ status: 200, contentType: "application/json", body: "{}" }); });
  await login(page, "/board");
  const open = page.getByRole("button", { name: "Deschide în cod #12@leaseweb" }).last();
  await open.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  await dialog.getByRole("button", { name: "Deschide", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: /Deschis pe/ })).toBeVisible();
  await expect(open).toBeFocused();
  const empty = page.getByRole("button", { name: "Deschide în cod #13@leaseweb" });
  await empty.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("alert").filter({ hasText: /nicio sesiune/ })).toBeVisible();
  await expect(empty).toBeFocused();
});

test("the 409 recovery link shows the returned command on the card page without asking the gateway again", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });
  let posts = 0;
  await page.route("**/api/cards/*/open-code", (route) => { posts += 1; return route.fulfill({ status: 409, contentType: "application/json",
    body: JSON.stringify({ error: "Nebula not reachable on perseu-mbp", command: "nebula open --session=01JF0000000000000000000002 --on perseu-mbp" }) }); });
  await login(page, "/board");
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await page.getByRole("dialog", { name: "Deschide în cod" }).getByRole("button", { name: "Deschide", exact: true }).click();
  await page.getByRole("link", { name: /pagina cardului/ }).click();
  await expect(page).toHaveURL(/\/board\/c\/12%40leaseweb$/);
  await expect(page.getByText("nebula open --session=01JF0000000000000000000002 --on perseu-mbp")).toBeVisible();
  await expect(page.getByText("@perseu-mbp", { exact: false }).first()).toBeVisible();
  expect(posts).toBe(1);
});

test("a laptop chosen in the dialog stays selected and is the one sent, even after a poll drops it", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "findl" }); });
  await login(page, "/board");
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  const laptop = dialog.getByLabel("Laptop");
  await laptop.selectOption("findl");
  const polled = page.waitForResponse((r) => r.url().includes("/api/board?"));
  changeBoard((board) => { board.hosts = board.hosts.filter((h) => h.host !== "findl"); });
  await polled;
  addCard(90, "Poll marker");
  await expect(page.getByRole("article", { includeHidden: true }).filter({ hasText: "Poll marker" })).toHaveCount(1);
  await expect(laptop).toHaveValue("findl");
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  expect((await posted).postDataJSON()).toMatchObject({ on: "findl" });
});

test("a dialog opened from the strip survives its row leaving and sends the captured session", async ({ page }) => {
  changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });
  waiting(1764000600000);
  await login(page, "/board");
  const strip = page.getByRole("region", { name: /Te așteaptă/ });
  await strip.getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  const dialog = page.getByRole("dialog", { name: "Deschide în cod" });
  changeBoard((board) => { Object.assign(board.details[CARD].sessions[0], { status: "running", status_changed_at: 1764000700000 }); });
  await expect(page.getByRole("button", { name: /^Aprobă/, includeHidden: true })).toHaveCount(0, { timeout: 15_000 });
  await expect(dialog).toBeVisible();
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await dialog.getByRole("button", { name: "Deschide", exact: true }).click();
  expect((await posted).postDataJSON()).toMatchObject({ session: "01JF0000000000000000000002", host: "leaseweb" });
  await expect(page.getByRole("status").filter({ hasText: /Deschis pe/ })).toBeVisible();
});

test("Open in code sits only on tiles with a wait, a known session or a Doing/Review stage", async ({ page }) => {
  addCard(13, "Never started");
  addCard(14, "Under review", { stage: "review" });
  await login(page, "/board");
  await expect(tile(page, "Never started").getByRole("button", { name: /Pornește/ })).toBeVisible();
  await expect(tile(page, "Never started").getByRole("button", { name: /Deschide în cod/ })).toHaveCount(0);
  await expect(tile(page, "Under review").getByRole("button", { name: /Deschide în cod/ })).toBeVisible();
  await expect(tile(page, "Keep verification results").getByRole("button", { name: /Deschide în cod/ })).toBeVisible();
});

test("two waiting sessions with the same name keep one footer dot each, and a resumed one drops its dot", async ({ page }) => {
  changeBoard((board) => {
    const sessions = board.details[CARD].sessions;
    Object.assign(sessions[0], { status: "needs_feedback", status_changed_at: 1764000600000 });
    sessions.push({ ...sessions[0], id: "01JF0000000000000000000004", host: "findl" }, { ...sessions[0], id: REVIEWER, name: "reviewer", writer: false });
  });
  await login(page, "/board");
  const footer = tile(page, "Keep verification results").getByRole("img", { name: /: Așteaptă răspuns$/ });
  await expect(footer).toHaveCount(3);
  changeBoard((board) => { Object.assign(board.details[CARD].sessions[0], { status: "running", status_changed_at: 1764000700000 }); });
  await expect(page.getByRole("button", { name: /^Aprobă/ })).toHaveCount(2, { timeout: 15_000 });
  await expect(footer).toHaveCount(2);
  await expect(tile(page, "Keep verification results").getByRole("img", { name: /^worker:/ })).toHaveCount(1);
});


const DIALOG = (ref: string) => `Deschide în cod ${ref}`;
const confirmOpen = (page: Page) => page.getByRole("dialog", { name: "Deschide în cod" }).getByRole("button", { name: "Deschide", exact: true }).click();
const startedCard = (number: number, title: string, session: string, delay = 0) => {
  addCard(number, title, { stage: "doing" });
  changeBoard((board) => { board.details[`01JD${String(number).padStart(22, "0")}`] = { sessions: [{ ...board.details[CARD].sessions[0], id: session }], artifacts: [], log: [] }; });
  return delay;
};
const gone409 = (page: Page) => page.route("**/api/cards/*/open-code", (route) => route.fulfill({ status: 409, contentType: "application/json",
  body: JSON.stringify({ error: "Nebula not reachable on perseu-mbp", command: "nebula open --session=01JF0000000000000000000002 --on perseu-mbp" }) }));
const withLaptop = () => changeBoard((board) => { board.hosts.push({ ...board.hosts[0], host: "perseu-mbp" }); });

test("a slow lookup that finishes after another dialog opened is dropped, and the dialog names its card", async ({ page }) => {
  startedCard(13, "Slow lookup", "01JF0000000000000000000099");
  waiting(1764000600000);
  withLaptop();
  await page.route("**/api/cards/13%40leaseweb", async (route) => { await new Promise((r) => setTimeout(r, 2000)); await route.continue(); });
  await login(page, "/board");
  await tile(page, "Slow lookup").getByRole("button", { name: "Deschide în cod #13@leaseweb" }).click();
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  const dialog = page.getByRole("dialog", { name: DIALOG("#12@leaseweb") });
  await expect(dialog).toBeVisible();
  await page.waitForTimeout(2500);
  await expect(dialog).toBeVisible();
  await expect(page.getByRole("dialog", { name: DIALOG("#13@leaseweb") })).toHaveCount(0);
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await confirmOpen(page);
  const request = await posted;
  expect(request.url()).toContain("/cards/12%40leaseweb/open-code");
  expect(request.postDataJSON()).toMatchObject({ session: "01JF0000000000000000000002" });
});

test("with two waits, the tile, the strip and the card page all open the one that has waited longest", async ({ page }) => {
  withLaptop();
  changeBoard((board) => {
    const sessions = board.details[CARD].sessions;
    Object.assign(sessions[0], { status: "needs_feedback", status_changed_at: 1764000900000 });
    sessions.push({ ...sessions[0], id: REVIEWER, host: "findl", name: "reviewer", writer: false, status_changed_at: 1764000800000 });
  });
  const opened: unknown[] = [];
  page.on("request", (r) => { if (r.url().endsWith("/open-code")) opened.push(r.postDataJSON()); });
  await login(page, "/board");
  for (const button of [tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }),
    page.getByRole("region", { name: /Te așteaptă/ }).getByRole("button", { name: "Deschide în cod #12@leaseweb" })]) {
    await button.click();
    const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
    await confirmOpen(page);
    await posted;
  }
  await page.goto("/board/c/12%40leaseweb");
  await page.getByRole("region", { name: "Acțiuni" }).getByRole("button", { name: "Deschide în cod" }).click();
  const posted = page.waitForRequest((r) => r.url().endsWith("/open-code"));
  await confirmOpen(page);
  await posted;
  expect(opened).toEqual([0, 1, 2].map(() => expect.objectContaining({ session: REVIEWER, host: "findl" })));
});

test("leaving the route closes an open dialog and drops a lookup still in flight", async ({ page }) => {
  startedCard(13, "Slow lookup", "01JF0000000000000000000099");
  withLaptop();
  await page.route("**/api/cards/13%40leaseweb", async (route) => { await new Promise((r) => setTimeout(r, 2000)); await route.continue(); });
  await login(page, "/board/c/11%40leaseweb");
  await page.getByRole("button", { name: "Board", exact: true }).click();
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await expect(page.getByRole("dialog", { name: "Deschide în cod" })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/\/board\/c\/11%40leaseweb$/);
  await expect(page.getByRole("dialog", { name: /Deschide în cod/ })).toHaveCount(0);
  await page.goForward();
  await tile(page, "Slow lookup").getByRole("button", { name: "Deschide în cod #13@leaseweb" }).click();
  await page.getByRole("button", { name: "Sesiuni", exact: true }).click();
  await page.waitForTimeout(2500);
  await page.goBack();
  await expect(tile(page, "Slow lookup")).toBeVisible();
  await page.waitForTimeout(500);
  await expect(page.getByRole("dialog", { name: /Deschide în cod/ })).toHaveCount(0);
});

test("a lookup that fails after a poll replaced its tile reports in the status line and focuses the new tile's button", async ({ page }) => {
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/cards/12%40leaseweb", async (route) => {
    await held;
    await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "Gateway busy" }) });
  });
  await login(page, "/board");
  const doing = column(page, "În lucru").getByRole("button", { name: "Deschide în cod #12@leaseweb" });
  await doing.focus();
  await page.keyboard.press("Enter");
  changeBoard((board) => { board.cards.find((c: { id: string }) => c.id === CARD)!.stage = "review"; });
  const moved = column(page, "Review").getByRole("button", { name: "Deschide în cod #12@leaseweb" });
  await expect(moved).toBeVisible({ timeout: 15_000 });
  release();
  await expect(page.getByRole("status").filter({ hasText: "Gateway busy" })).toBeVisible();
  await expect(moved).toBeFocused();
});

test("the stored 409 command shows on the card page even when the card read fails", async ({ page }) => {
  withLaptop();
  await gone409(page);
  await login(page, "/board");
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await confirmOpen(page);
  await page.route("**/api/cards/12%40leaseweb", (route) => route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "Gateway busy" }) }));
  await page.getByRole("link", { name: /pagina cardului/ }).click();
  await expect(page.getByText("nebula open --session=01JF0000000000000000000002 --on perseu-mbp")).toBeVisible();
  await expect(page.getByRole("button", { name: "Copiază comanda" })).toBeVisible();
});

test("the card page offers every laptop its sessions know, even when the board read fails", async ({ page }) => {
  changeBoard((board) => {
    const sessions = board.details[CARD].sessions;
    sessions.push({ ...sessions[0], id: "01JF0000000000000000000005", host: "findl", writer: false }, { ...sessions[0], id: "01JF0000000000000000000006", host: "perseu-mbp", writer: false });
  });
  await page.route("**/api/board?*", (route) => route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "Gateway busy" }) }));
  await login(page, "/board/c/12%40leaseweb");
  await page.getByRole("region", { name: "Acțiuni" }).getByRole("button", { name: "Deschide în cod" }).click();
  const options = page.getByRole("dialog", { name: "Deschide în cod" }).getByLabel("Laptop").locator("option");
  await expect(options).toHaveCount(3);
  expect((await options.allInnerTexts()).sort()).toEqual(["@findl", "@leaseweb", "@perseu-mbp"]);
});

test("a peek shows the compact result, not the command block", async ({ page }) => {
  withLaptop();
  await gone409(page);
  await login(page, "/board?peek=12%40leaseweb");
  const peek = page.getByRole("dialog", { name: "Keep verification results" });
  await peek.getByRole("button", { name: "Deschide în cod" }).click();
  await confirmOpen(page);
  await expect(page.locator("#open-code-status")).toContainText("pagina cardului");
  await expect(page.getByText(/nebula open --session/)).toHaveCount(0);
});

test("logging out forgets stored Open in code results", async ({ page }) => {
  withLaptop();
  await gone409(page);
  await login(page, "/board");
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await confirmOpen(page);
  await expect(page.getByRole("link", { name: /pagina cardului/ })).toBeVisible();
  await page.getByRole("button", { name: "Sesiuni", exact: true }).click();
  await page.getByRole("button", { name: "Deconectare" }).filter({ visible: true }).click();
  await page.getByLabel("Token de acces").fill(TOKEN);
  await page.getByRole("button", { name: "Conectare" }).click();
  await page.getByRole("link", { name: "Board" }).or(page.getByRole("button", { name: "Board", exact: true })).filter({ visible: true }).first().click();
  await expect(tile(page, "Keep verification results")).toBeVisible();
  await expect(page.getByRole("link", { name: /pagina cardului/ })).toHaveCount(0);
});

test("the recovery link is a real link: a modified click opens a new tab, and the command survives a reload", async ({ page, context }) => {
  withLaptop();
  await gone409(page);
  await login(page, "/board");
  await tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" }).click();
  await confirmOpen(page);
  const link = page.getByRole("link", { name: /pagina cardului/ });
  await expect(link).toHaveAttribute("href", "/board/c/12%40leaseweb");
  const opened = context.waitForEvent("page");
  await link.click({ modifiers: ["ControlOrMeta"] });
  expect((await opened).url()).toMatch(/\/board\/c\/12%40leaseweb$/);
  await expect(page).toHaveURL(/\/board$/);
  await link.click();
  await expect(page).toHaveURL(/\/board\/c\/12%40leaseweb$/);
  await page.reload();
  await expect(page.getByText("nebula open --session=01JF0000000000000000000002 --on perseu-mbp")).toBeVisible();
});

test("a success line sits in a status region mounted beforehand, can be closed, and leaves on its own", async ({ page }) => {
  test.setTimeout(60_000);
  withLaptop();
  await login(page, "/board");
  const region = page.locator("#open-code-status");
  await expect(region).toHaveAttribute("role", "status");
  await expect(region).toHaveText("");
  const open = tile(page, "Keep verification results").getByRole("button", { name: "Deschide în cod #12@leaseweb" });
  await open.click();
  await confirmOpen(page);
  await expect(region).toContainText("Deschis pe");
  await region.getByRole("button", { name: "Închide mesajul" }).click();
  await expect(region).toHaveText("");
  await open.click();
  await confirmOpen(page);
  await expect(region).toContainText("Deschis pe");
  await expect(region).toHaveText("", { timeout: 20_000 });
});
