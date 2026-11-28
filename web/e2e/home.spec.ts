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
  await expect(page).toHaveURL(/\/code\?view=all$/);
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
  await expect(page.getByRole("dialog", { name: "? Taste" })).toBeVisible();
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
  await expect(page.getByRole("region", { name: /^Card #/ })).toHaveCount(0);
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

test("Enter on a focused button or link activates it instead of opening CODE", async ({ page }) => {
  waitingBoard();
  await login(page, "/");
  const reviewBtn = page.getByRole("group", { name: "Etape" }).getByRole("button", { name: /Review/ });
  await reviewBtn.focus();
  await page.keyboard.press("Enter");
  await expect(reviewBtn).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const decisionsLink = page.getByRole("complementary", { name: "Context și decizii" }).getByRole("link");
  await decisionsLink.focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/decisions$/);
});

test("picking a session in Lanț de sesiuni opens that picked session in CODE", async ({ page }) => {
  const host = await (await page.request.get("/api/projects", { headers: { Authorization: `Bearer ${TOKEN}` } })).json().then((d) => d.host as string);
  const card = { id: "01JD0000000000000000000078", number: 78, ref: "#78", title: "Two session card", stage: "doing", project: "alpha", proposed: false, archived: false,
    created_by: { host, agent: null, name: "you" }, checkout: { host, branch: "main", path: "/srv/alpha", worktree: false }, blocked_by: [], cleared_blockers: [], created_at: 1, updated_at: 1 };
  const s1 = { host, id: "pi-1", name: "First worker", role: "worker", kind: "pi", status: "finished", status_changed_at: 1, writer: false, alive: true, stale: false };
  const s2 = { host, id: "pi-2", name: "Second reviewer", role: "review", kind: "pi", status: "running", status_changed_at: 2, writer: true, alive: true, stale: false };
  await page.route("**/api/board?*", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/board", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/cards/78", (route) => route.fulfill({ json: { ...card, sessions: [s1, s2], artifacts: [], log: [] } }));
  await login(page, "/");
  const pane = page.getByRole("region", { name: "Card #78" });
  const chain = pane.getByRole("list", { name: "Lanț de sesiuni" });
  await expect(chain.getByRole("button", { name: /Second reviewer/ })).toHaveAttribute("aria-current", "step");
  await chain.getByRole("button", { name: /First worker/ }).click();
  await expect(chain.getByRole("button", { name: /First worker/ })).toHaveAttribute("aria-current", "step");
  await pane.getByRole("button", { name: /deschide în cod/ }).click();
  await expect(page).toHaveURL(/\/p\/alpha\/s\/pi-1$/);
});

test("stale /c/999 shows Card negăsit and does not bind a or Enter to another card", async ({ page }) => {
  waitingBoard();
  await login(page, "/c/999");
  await expect(page.getByRole("alert").filter({ hasText: "Card negăsit" })).toBeVisible();
  await expect(page.getByRole("region", { name: /^Card #/ })).toHaveCount(0);
  await page.locator("body").press("a");
  await page.locator("body").press("Enter");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(needs(page).getByRole("button", { name: /Keep verification results/ })).toBeVisible();
  expect(sends().filter((s) => s.approve)).toHaveLength(0);
});

test("held a (repeat keydown) is ignored and does not approve", async ({ page }) => {
  waitingBoard();
  await login(page, "/c/12%40leaseweb");
  await expect(needs(page).getByRole("button", { name: /Keep verification results/ })).toBeVisible();
  await page.evaluate(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "a", repeat: true, bubbles: true, cancelable: true }));
  });
  await expect(needs(page).getByRole("button", { name: /Keep verification results/ })).toBeVisible();
  expect(sends().filter((s) => s.approve)).toHaveLength(0);
});

test("inline reply on HOME focuses with i, sends to gateway-listed session, clears input and refreshes latest", async ({ page }) => {
  const host = await (await page.request.get("/api/projects", { headers: { Authorization: `Bearer ${TOKEN}` } })).json().then((d) => d.host as string);
  const card = { id: "01JD0000000000000000000079", number: 79, ref: "#79", title: "Inline reply card", stage: "doing", project: "alpha", proposed: false, archived: false,
    created_by: { host, agent: null, name: "you" }, checkout: { host, branch: "main", path: "/srv/alpha", worktree: false }, blocked_by: [], cleared_blockers: [], created_at: 1, updated_at: 1 };
  const session = { host, id: "pi-1", name: "Refactor auth", role: "worker", kind: "pi", status: "running", status_changed_at: 1, writer: true, alive: true, stale: false };
  await page.route("**/api/board?*", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/board", (route) => route.fulfill({ json: { version: "B.1", board: { cards: [card], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } }, next: null } }));
  await page.route("**/api/cards/79", (route) => route.fulfill({ json: { ...card, sessions: [session], artifacts: [], log: [] } }));
  await login(page, "/");
  const pane = page.getByRole("region", { name: "Card #79" });
  const input = pane.getByPlaceholder("Răspunde lui Refactor auth… (i)");
  await expect(input).toBeVisible();
  await page.locator("body").press("i");
  await expect(input).toBeFocused();
  await expect(input).toHaveValue("");
  const before = sends().length;
  await input.fill("Salut direct din HOME");
  await pane.getByRole("button", { name: "Trimite" }).click();
  await expect.poll(() => sends().slice(before).some((s) => s.text === "Salut direct din HOME")).toBe(true);
  await expect(input).toHaveValue("");
  const chat = pane.getByRole("region", { name: "Chat" });
  await expect(chat).toContainText("Salut direct din HOME");
  await expect(chat).toContainText("Da — am adăugat");
});

test("project scope via PROIECTE and topbar picker filters HOME, persists across DECISIONS/RAG/CODE, and resets via × or 0", async ({ page }) => {
  changeBoard((board) => {
    const base = board.cards.find((c) => c.id === CARD12)!;
    board.cards.push({ ...base, id: "01JD0000000000000000000081", number: 81, ref: "#81@leaseweb", title: "Vela billing polish", stage: "doing", project: "vela", checkout: null, blocked_by: [] });
    board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000081", title: "Vela invoice format", card: "01JD0000000000000000000081", project: "vela",
      content: { body: "Keep invoice PDF deterministic" }, created_at: 1764000700000 });
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page, "/");

  await page.getByRole("tab", { name: "PROIECTE" }).click();
  await page.getByRole("region", { name: "Proiecte" }).getByRole("button", { name: /vela/ }).click();
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("@vela");
  await expect(page.getByRole("region", { name: "Card #81@leaseweb" })).toBeVisible();
  const rail = page.getByRole("complementary", { name: "Context și decizii" });
  await expect(rail).toContainText("Vela invoice format");
  await expect(rail).not.toContainText("Copy published files");

  await page.keyboard.press("4");
  await expect(page).toHaveURL(/\/decisions$/);
  await expect(page.getByRole("article").filter({ hasText: "Vela invoice format" })).toBeVisible();
  await expect(page.getByRole("article").filter({ hasText: "Copy published files" })).toHaveCount(0);

  await page.keyboard.press("3");
  await expect(page).toHaveURL(/\/rag$/);
  await expect(page.getByRole("region", { name: "RAG" })).toContainText("@vela");

  await page.getByRole("button", { name: "Filtru proiect" }).click();
  await page.getByRole("menu", { name: "Proiecte" }).getByRole("menuitemradio", { name: /alpha/ }).click();
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("@alpha");
  await page.keyboard.press("2");
  await expect(page).toHaveURL(/\/p\/alpha$/);
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("@alpha");

  await page.getByRole("button", { name: "Resetează filtrul de proiect" }).click();
  await expect(page).toHaveURL(/\/code\?view=all$/);
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("all");
  await expect(page.getByRole("button", { name: "Resetează filtrul de proiect" })).toHaveCount(0);

  await page.keyboard.press("1");
  await expect(page).toHaveURL(/\/$/);
  await page.getByRole("button", { name: "Filtru proiect" }).click();
  await page.getByRole("menu", { name: "Proiecte" }).getByRole("menuitemradio", { name: /vela/ }).click();
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("@vela");
  await page.keyboard.press("0");
  await expect(page.getByRole("button", { name: "Filtru proiect" })).toContainText("all");
  await expect(rail).toContainText("Copy published files");
});

test("a card with no session starts one from HOME's chat: i focuses, Enter sends the first prompt on the card, and the session joins the chain", async ({ page }) => {
  changeBoard((board) => {
    const base = board.cards.find((c) => c.id === CARD12)!;
    board.cards.push({ ...base, id: "01JD0000000000000000000090", number: 90, ref: "#90", title: "Rollout check", stage: "backlog", project: "alpha", checkout: null, blocked_by: [] });
  });
  await login(page, "/c/90");
  const pane = page.getByRole("region", { name: "Card #90" });
  const input = pane.getByRole("textbox", { name: "Pornește o sesiune pe #90" });
  await expect(input).toBeEnabled();
  await expect(pane.getByRole("list", { name: "Lanț de sesiuni" })).toHaveCount(0);
  await page.locator("body").press("i");
  await expect(input).toBeFocused();
  await input.fill("Verifică rollout-ul");
  await input.press("Enter");
  await expect.poll(() => sends().find((s) => s.new && s.text === "Verifică rollout-ul")).toMatchObject({ kind: "claude", card: "01JD0000000000000000000090" });
  await expect(pane.getByRole("list", { name: "Lanț de sesiuni" }).getByRole("button")).toHaveCount(1);
  await expect(pane.getByRole("textbox", { name: /^Răspunde lui Mobile / })).toBeVisible();
  expect(page.url()).not.toMatch(/\/p\//);
});

test("a card whose session this gateway cannot reach still chats on HOME: the message lands in the card log", async ({ page }) => {
  await login(page, "/");
  const pane = page.getByRole("region", { name: "Card #12@leaseweb" });
  const input = pane.getByRole("textbox", { name: "Mesaj pe #12@leaseweb" });
  await expect(input).toBeEnabled();
  await page.locator("body").press("i");
  await expect(input).toBeFocused();
  await input.fill("Notă din HOME");
  await pane.getByRole("button", { name: "Trimite" }).click();
  await expect(input).toHaveValue("");
  await expect(pane.getByRole("region", { name: "Jurnal" })).toContainText("Notă din HOME");
});

test("HOME chat shows the user message instantly on the right and an agent loader on the left while in flight", async ({ page }) => {
  changeBoard((board) => {
    const base = board.cards.find((c) => c.id === CARD12)!;
    board.cards.push({ ...base, id: "01JD0000000000000000000091", number: 91, ref: "#91", title: "Instant chat check", stage: "backlog", project: "alpha", checkout: null, blocked_by: [] });
  });
  let release!: () => void;
  const hold = new Promise<void>((r) => { release = r; });
  await page.route("**/api/new", async (route) => {
    await hold;
    await route.continue();
  });
  await login(page, "/c/91");
  const pane = page.getByRole("region", { name: "Card #91" });
  const input = pane.getByRole("textbox", { name: "Pornește o sesiune pe #91" });
  await input.fill("Salut instant");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  const userBubble = pane.locator('li[data-role="user"][data-pending]');
  const loader = pane.locator('li[data-role="assistant-loader"]');
  await expect(userBubble).toContainText("Salut instant");
  await expect(loader).toContainText("Se pornește agentul…");
  const userBox = await userBubble.boundingBox();
  const loaderBox = await loader.boundingBox();
  expect(userBox!.x).toBeGreaterThan(loaderBox!.x);
  release();
  await expect(pane.getByRole("list", { name: "Lanț de sesiuni" }).getByRole("button")).toHaveCount(1);
});
