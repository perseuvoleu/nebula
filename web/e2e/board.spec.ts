import { expect, type Locator, type Page } from "@playwright/test";
import { test, changeBoard, login, noHorizontalOverflow, setBehaviour, shot } from "./helpers";

function addCard(number: number, title: string, stage: string, blockedBy: string[] = []) {
  changeBoard((board) => board.cards.push({ ...board.cards[1], id: `01JD${String(number).padStart(22, "0")}`, number,
    ref: `#${number}@leaseweb`, title, stage, checkout: null, blocked_by: blockedBy }));
}

async function awayFrom(page: Page, at = 1764000000000) {
  await page.addInitScript((seen) => localStorage.setItem("nebula-board-last-seen", String(seen)), at);
}

const column = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const tile = (scope: Page | Locator, title: string) => scope.getByRole("article").filter({ hasText: title });

test("columns and table", async ({ page }) => {
  await login(page, "/board");
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
  changeBoard((board) => board.details["01JD0000000000000000000012"].sessions.push(
    { ...board.details["01JD0000000000000000000012"].sessions[0], id: "01JF0000000000000000000003", name: "reviewer", role: "review", status: "fresh", writer: false }));
  await login(page, "/board?peek=12%40leaseweb");
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
  changeBoard((board) => { board.hosts[0].needs_you = 1; });
  await login(page, "/board");
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
  await login(page, "/board/p/nebula");
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

test("the away digest reads the decisions it names once, not every page on every board change", async ({ page }) => {
  changeBoard((board) => board.decisions.push(...[1, 2, 3].map((n) => ({ ...board.decisions[0], id: `01JG${String(n).padStart(22, "0")}`,
    title: `Older decision ${n}`, created_at: 1763000000000 + n }))));
  const decisionReads: string[] = [];
  page.on("request", (r) => { if (new URL(r.url()).pathname === "/api/decisions") decisionReads.push(r.url()); });
  await awayFrom(page);
  await login(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest.getByText("Copy published files")).toBeVisible();
  for (const [i, stage] of ["next", "doing"].entries()) {
    const polled = page.waitForResponse((r) => new URL(r.url()).pathname === "/api/board" && r.ok() && r.request().url().includes("since"));
    addCard(30 + i, `Board change to ${stage}`, stage);
    await polled;
    await expect(tile(page, `Board change to ${stage}`)).toBeVisible();
  }
  expect(decisionReads).toHaveLength(1);
});

test("a failed digest read of its decisions is retried without a board change", async ({ page }) => {
  let failed = false;
  await page.route("**/api/decisions*", (route) => {
    if (failed) return route.continue();
    failed = true;
    return route.fulfill({ status: 503, json: { error: "Synthetic unavailable" } });
  });
  await awayFrom(page);
  await login(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest.getByRole("button", { name: "Keep verification results" })).toBeVisible();
  expect(failed).toBe(true);
  await expect(digest.getByText("Copy published files")).toBeVisible();
});

test("a hidden away digest reads no decisions", async ({ page }) => {
  const decisionReads: string[] = [];
  page.on("request", (r) => { if (new URL(r.url()).pathname === "/api/decisions") decisionReads.push(r.url()); });
  await awayFrom(page);
  await login(page, "/board/c/12%40leaseweb");
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
  await page.waitForResponse((r) => new URL(r.url()).pathname === "/api/board" && r.request().url().includes("since"));
  expect(decisionReads).toEqual([]);
});

test("at 390 px a long away digest is capped and leaves the board in view", async ({ page }) => {
  for (let number = 20; number < 50; number++) addCard(number, `Finished ${number}`, "done");
  changeBoard((board) => board.away.finished.push(...board.cards.filter((c) => c.stage === "done").map((c) => ({ id: c.id, at: 1764000500000 }))));
  await page.setViewportSize({ width: 390, height: 844 });
  await awayFrom(page);
  await login(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest.getByRole("button", { name: "Finished 20" })).toBeVisible();
  await expect(column(page, "Backlog")).toBeInViewport();
  await digest.getByRole("button", { name: /Arată toate/ }).click();
  await expect(digest.getByRole("button", { name: "Finished 49" })).toBeVisible();
});

test("a project's Decisions route lists only that project's decisions", async ({ page }) => {
  const listed = page.waitForResponse((r) => r.url().includes("/api/decisions"));
  await login(page, "/board/decisions/p/vela");
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
  await login(page, "/board?peek=12%40leaseweb");
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
  await login(page, "/board");
  const look = (title: string) => tile(page, title).evaluate((el) => { const s = getComputedStyle(el); return { bg: s.backgroundColor, border: s.borderTopStyle }; });
  await expect(tile(page, "Wait for verification")).toBeVisible();
  const [blocked, plain] = [await look("Wait for verification"), await look("Keep verification results")];
  expect(blocked.border).toBe("dashed");
  expect(blocked.bg).not.toBe(plain.bg);
});

test("sidebar counts and quiet projects", async ({ page }) => {
  await login(page, "/board");
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
  await login(page, "/board");
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

test("a board read that spans two pages shows the cards of both", async ({ page }) => {
  addCard(13, "Only on the second page", "backlog");
  let rest: unknown[] = [];
  await page.route("**/api/board?*", async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get("page") === "second") return route.fulfill({ json: { version: "v", board: { cards: rest, hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } });
    const response = await route.fetch();
    const reply = await response.json();
    if (reply.unchanged) return route.fulfill({ response, json: reply });
    rest = reply.board.cards.filter((c: { title: string }) => c.title === "Only on the second page");
    reply.board.cards = reply.board.cards.filter((c: { title: string }) => c.title !== "Only on the second page");
    await route.fulfill({ response, json: { ...reply, next: "second" } });
  });
  await login(page, "/board");
  await expect(tile(column(page, "Backlog"), "Only on the second page")).toBeVisible();
  await expect(tile(page, "Keep verification results")).toBeVisible();
});

test("board polls never overlap and stop while the tab is hidden", async ({ page }) => {
  await page.clock.install();
  await login(page, "/board");
  await expect(tile(page, "Keep verification results")).toBeVisible();
  let inFlight = 0;
  let most = 0;
  let sent = 0;
  let release = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/board?*", async (route) => {
    sent += 1;
    most = Math.max(most, ++inFlight);
    await held;
    const response = await route.fetch();
    inFlight -= 1;
    await route.fulfill({ response });
  });
  await page.clock.fastForward(4000);
  await expect.poll(() => sent).toBe(1);
  await page.clock.fastForward(30_000);
  release();
  await expect.poll(() => inFlight).toBe(0);
  expect(most, "board polls in flight at once").toBe(1);

  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await page.clock.fastForward(4000);
  const before = sent;
  await page.clock.fastForward(30_000);
  expect(sent, "board polls sent while the tab was hidden").toBe(before);
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(() => sent).toBeGreaterThan(before);
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test("board, peek and card page are usable at 390 px", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/board");
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

function waitingWorker() {
  changeBoard((board) => { Object.assign(board.details["01JD0000000000000000000012"].sessions[0], { status: "needs_feedback", status_changed_at: 1764000600000 }); });
}

test("a waiting card lights the sidebar: a Te așteaptă row with its count and a red dot on its project", async ({ page }) => {
  waitingWorker();
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  const needs = sidebar.getByRole("button", { name: /Te așteaptă\s*1$/ });
  await expect(needs).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /nebula/ }).getByRole("img", { name: "Așteaptă răspuns" })).toBeVisible();
  addCard(13, "Not waiting", "next");
  await expect(tile(page, "Not waiting")).toBeVisible();
  await needs.click();
  await expect(tile(page, "Keep verification results")).toBeVisible();
  await expect(tile(page, "Not waiting")).toHaveCount(0);
});

test("the header filter keeps the cards whose title, ref or project match", async ({ page }) => {
  addCard(13, "Another card", "next");
  await login(page, "/board");
  const filter = page.getByRole("searchbox", { name: "Filtrează cardurile" });
  await filter.fill("verification");
  await expect(tile(page, "Keep verification results")).toBeVisible();
  await expect(tile(page, "Another card")).toHaveCount(0);
  await filter.fill("#13");
  await expect(tile(page, "Another card")).toBeVisible();
  await expect(tile(page, "Keep verification results")).toHaveCount(0);
  await filter.fill("nebula");
  await expect(tile(page, "Another card")).toBeVisible();
  await expect(tile(page, "Keep verification results")).toBeVisible();
});

test("the Absență button reopens a closed away digest", async ({ page }) => {
  await awayFrom(page);
  await login(page, "/board");
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest).toBeVisible();
  await digest.getByRole("button", { name: "Închide rezumatul" }).click();
  await expect(digest).toHaveCount(0);
  await page.getByRole("button", { name: /^Absență \d+/ }).click();
  await expect(digest).toBeVisible();
});

test("with the side panel off a tile opens the full card page", async ({ page }) => {
  await login(page, "/board");
  const toggle = page.getByRole("button", { name: "Panou lateral" });
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "false");
  await tile(page, "Keep verification results").getByRole("button", { name: "Keep verification results" }).click();
  await expect(page).toHaveURL(/\/board\/c\/12%40leaseweb$/);
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("at 390 px the scope title, view switch and every host's writer slots stay visible, on the card and decisions routes too", async ({ page }) => {
  changeBoard((board) => { board.hosts = ["leaseweb", "findl", "perseu-mbp"].map((host) => ({ ...board.hosts[0], host, writers: 1, writer_slots: 4 })); });
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/board");
  const slots = page.getByTitle("Sesiuni care scriu acum, pe fiecare host");
  const title = page.getByRole("heading", { name: "Toate proiectele" });
  await expect(title).toBeInViewport();
  expect((await title.boundingBox())!.width).toBeGreaterThan(60);
  await expect(page.getByRole("button", { name: "Tabel" })).toBeInViewport();
  await expect(slots).toHaveCount(1);
  await expect(slots).toContainText("perseu-mbp");
  const filter = page.getByRole("searchbox", { name: "Filtrează cardurile" });
  expect(await filter.evaluate((el) => { const s = getComputedStyle(el); return el.clientWidth - parseFloat(s.paddingLeft) - parseFloat(s.paddingRight); })).toBeGreaterThanOrEqual(160);
  await expect(filter).toBeInViewport({ ratio: 1 });
  await noHorizontalOverflow(page);
  for (const route of ["/board/c/12%40leaseweb", "/board/decisions"]) {
    await page.goto(route);
    await expect(slots).toBeVisible();
    await noHorizontalOverflow(page);
  }
});

const proposal = (number: number, title: string) => changeBoard((board) => board.cards.push({ ...board.cards[1], id: `01JD${String(number).padStart(22, "0")}`, number,
  ref: `#${number}@leaseweb`, title, stage: "backlog", checkout: null, blocked_by: [], proposed: true }));

test("the header filter narrows tiles, the strip and proposals alike, and leaves the sidebar counts whole", async ({ page }) => {
  waitingWorker();
  proposal(13, "Proposed elsewhere");
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await expect(page.getByRole("button", { name: /Proposed elsewhere/ }).first()).toBeVisible();
  await page.getByRole("searchbox", { name: "Filtrează cardurile" }).fill("zzz");
  await expect(page.getByText("Proposed elsewhere")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Acceptă/ })).toHaveCount(0);
  await expect(page.getByRole("region", { name: /Te așteaptă/ })).toHaveCount(0);
  await expect(sidebar.getByRole("button", { name: /Te așteaptă\s*1$/ })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: /Toate proiectele/ })).toContainText(/\d+/);
});

test("the attention scope names itself, stays selected, and says so when nothing waits any more", async ({ page }) => {
  waitingWorker();
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await sidebar.getByRole("button", { name: /Te așteaptă/ }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Te așteaptă", exact: true })).toBeVisible();
  const row = sidebar.getByRole("button", { name: /Te așteaptă/ });
  await expect(row).toHaveAttribute("aria-current", "page");
  changeBoard((board) => { Object.assign(board.details["01JD0000000000000000000012"].sessions[0], { status: "running", status_changed_at: 1764000700000 }); });
  await expect(page.getByText("Nimic nu te așteaptă acum.")).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("aria-current", "page");
  await expect(row).toContainText("0");
});

test("a wait on a proposed card lights nothing: no project dot, no count, no strip", async ({ page }) => {
  proposal(13, "Proposed and waiting");
  changeBoard((board) => { board.details["01JD0000000000000000000013"] = { sessions: [{ ...board.details["01JD0000000000000000000012"].sessions[0], id: "01JF0000000000000000000013", status: "needs_feedback", status_changed_at: 1764000600000 }], artifacts: [], log: [] }; });
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await expect(page.getByText("Proposed and waiting").first()).toBeVisible();
  await expect(sidebar.getByRole("img", { name: "Așteaptă răspuns" })).toHaveCount(0);
  await expect(sidebar.getByRole("button", { name: /Te așteaptă/ })).toHaveCount(0);
  await expect(page.getByRole("region", { name: /Te așteaptă/ })).toHaveCount(0);
});

test("Absență is in the header wherever the digest can show, and opens an empty digest that says so", async ({ page }) => {
  await login(page, "/board");
  const away = page.getByRole("button", { name: /^Absență/ });
  const digest = page.getByRole("region", { name: "De când ai lipsit" });
  await expect(digest).toHaveCount(0);
  await away.click();
  await expect(digest).toBeVisible();
  await expect(digest.getByText("Nimic nou de la ultima vizită.")).toBeVisible();
  await page.goto("/board/decisions");
  await expect(away).toBeVisible();
});

test("at 1280 px every tile's age stays inside its tile", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await login(page, "/board");
  const tiles = page.getByRole("article");
  await expect(tiles.first()).toBeVisible();
  for (const t of await tiles.all()) {
    const edges = await t.evaluate((el) => {
      const age = [...el.querySelectorAll("span")].find((s) => /^\d+[a-zăîșț]+$/.test(s.textContent ?? ""))!;
      return { age: age.getBoundingClientRect().right, tile: el.getBoundingClientRect().right };
    });
    expect(edges.age).toBeLessThanOrEqual(edges.tile - 4);
  }
});

test("the filter's placeholder reads at 4.5:1", async ({ page }) => {
  await login(page, "/board");
  const ratio = await page.getByRole("searchbox", { name: "Filtrează cardurile" }).evaluate((el) => {
    const parse = (c: string) => (c.match(/[\d.]+/g) ?? []).map(Number);
    const lum = (c: number[]) => { const [r, g, b] = c.slice(0, 3).map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const [hi, lo] = [lum(parse(getComputedStyle(el, "::placeholder").color)), lum(parse(getComputedStyle(el).backgroundColor))].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
  });
  expect(ratio).toBeGreaterThanOrEqual(4.5);
});

test("in the attention scope a filter that matches nothing says so, apart from nothing waiting", async ({ page }) => {
  waitingWorker();
  await login(page, "/board");
  await page.getByRole("navigation", { name: "Proiecte" }).getByRole("button", { name: /Te așteaptă/ }).click();
  await page.getByRole("searchbox", { name: "Filtrează cardurile" }).fill("zzz");
  await expect(page.getByText("Niciun card care te așteaptă nu se potrivește filtrului.")).toBeVisible();
  await expect(page.getByText("Nimic nu te așteaptă acum.")).toHaveCount(0);
});
