import os from "node:os";
import { expect, type Page } from "@playwright/test";
import { test, changeBoard, openBoard, readBoardState, storeArtifact } from "./helpers";

const CARD = "01JD0000000000000000000012";
const RUN = "01JG0000000000000000000001";
const SESSION = { host: "leaseweb", id: "01JF0000000000000000000002", name: "worker" };
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=", "base64");

function artifact(id: string, kind: string, title: string, content: Record<string, string>) {
  return { id, kind, title, card: CARD, project: "nebula", session: SESSION, checkout: null, content, meta: {}, created_at: 1764000600000 };
}

function addArtifacts(...rows: ReturnType<typeof artifact>[]) {
  changeBoard((board) => board.details[CARD].artifacts.push(...rows));
}

const section = (page: Page, title: string) => page.getByRole("region", { name: title, exact: true });

test("e2e-run with 12 screenshots under poll", async ({ page }) => {
  const steps = Array.from({ length: 12 }, (_, i) => ({ name: `step ${i + 1}`, status: i === 11 ? "failed" : "passed", duration_ms: 100 * (i + 1),
    screenshot: `shots/${i + 1}.png`, ...(i === 11 ? { error: "row never appeared" } : {}) }));
  storeArtifact(RUN, { "result.json": JSON.stringify({ title: "board.spec", status: "failed", started_at: 1764000000000, duration_ms: 7800, steps }),
    ...Object.fromEntries(steps.map((s) => [s.screenshot, PNG])) });
  let polled: Promise<unknown> | undefined;
  let incompleteAtPoll: Promise<number> | undefined;
  await page.route("**/a/**/shots/*.png", async (route) => {
    const response = await route.fetch();
    polled ??= page.waitForResponse((r) => new URL(r.url()).pathname === "/api/board" && r.ok());
    incompleteAtPoll ??= polled.then(() => section(page, "board.spec").getByRole("img")
      .evaluateAll((imgs: HTMLImageElement[]) => imgs.filter((img) => !(img.complete && img.naturalWidth)).length));
    await incompleteAtPoll;
    await route.fulfill({ response });
  });
  await openBoard(page, "/board/c/12%40leaseweb");
  const run = section(page, "board.spec");
  await expect(run.getByRole("img")).toHaveCount(12);
  for (const [i, image] of (await run.getByRole("img").all()).entries()) {
    await image.scrollIntoViewIfNeeded();
    await expect(image).toHaveAccessibleName(`Captură: step ${i + 1}`);
    await expect.poll(() => image.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBeGreaterThan(0);
  }
  expect(await incompleteAtPoll, "a board poll succeeded while screenshots were still loading").toBeGreaterThan(0);
  await expect(run.getByText("row never appeared")).toBeVisible();
});

test("each kind renders", async ({ page }) => {
  storeArtifact("01JG0000000000000000000010", { "index.html": "<!doctype html><title>r</title><p id=out>static</p><script>out.textContent = 'recap script ran'</script>" });
  storeArtifact("01JG0000000000000000000011", { "tickets.md": "# Tichete slice 1\n\n- [ ] A1 imagini sub poll\n" });
  addArtifacts(
    artifact("01JG0000000000000000000010", "recap", "Recap slice 1", { host: "leaseweb", entry: "index.html" }),
    artifact("01JG0000000000000000000011", "ticket", "Tichete", { host: "leaseweb", entry: "tickets.md" }),
    artifact("01JG0000000000000000000012", "link", "Documentația", { url: "https://example.com/docs" }),
    artifact("01JG0000000000000000000013", "diagram", "Diagrama de pe laptop", { host: "perseu-mbp", entry: "index.html" }),
  );
  changeBoard((board) => board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000014", title: "Poll every three seconds",
    card: CARD, content: { body: "SSE can come later" } }));
  await openBoard(page, "/board/c/12%40leaseweb");

  const recap = section(page, "Recap slice 1").locator("iframe");
  await expect(recap).toHaveAttribute("sandbox", "allow-scripts");
  await expect(page.frameLocator('iframe[title="Recap slice 1"]').getByText("recap script ran")).toBeVisible();
  await expect(section(page, "Tichete").getByRole("heading", { name: "Tichete slice 1" })).toBeVisible();
  await expect(section(page, "Documentația").getByRole("link")).toHaveAttribute("href", "https://example.com/docs");
  await expect(section(page, "Poll every three seconds").getByText("SSE can come later")).toBeVisible();
  const remote = section(page, "Diagrama de pe laptop");
  await expect(remote.getByText("@perseu-mbp")).toBeVisible();
  await expect(remote.getByText("nebula artifact open 01JG0000000000000000000013")).toBeVisible();
  await expect(remote.locator("iframe")).toHaveCount(0);
});

test("decisions search", async ({ page }) => {
  changeBoard((board) => board.decisions.push(
    { ...board.decisions[0], id: "01JG0000000000000000000020", title: "Poll every three seconds", card: CARD, content: { body: "SSE can come later" } },
    { ...board.decisions[0], id: "01JG0000000000000000000021", title: "Vela keeps its schema", project: "vela", card: null, content: { body: "" } }));
  await openBoard(page, "/board/decisions");
  await expect(page.getByRole("article")).toHaveCount(3);
  await page.getByRole("searchbox", { name: "Caută decizii" }).fill("three");
  await expect(page.getByRole("article")).toHaveCount(1);
  const row = page.getByRole("article").filter({ hasText: "Poll every three seconds" });
  await expect(row.getByRole("link", { name: /sesiunea/ })).toHaveCount(0);
  await expect(row.getByText("sesiunea worker @leaseweb")).toBeVisible();
  await row.getByRole("link", { name: /#12@leaseweb/ }).click();
  await expect(page).toHaveURL(/\/board\/p\/nebula\/c\/12%40leaseweb$/);
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
});

test("record decision", async ({ page }) => {
  changeBoard((board) => board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000021", title: "Vela keeps its schema",
    project: "vela", card: null, content: { body: "" } }));
  await openBoard(page, "/board/p/nebula/c/12%40leaseweb");
  const form = page.getByRole("form", { name: "Notează o decizie" });
  await form.getByLabel("Decizia").fill("Keep runs for a year");
  await form.getByLabel("De ce").fill("Reviews look back that far");
  await form.getByRole("button", { name: "Notează" }).click();
  await expect(section(page, "Keep runs for a year").getByText("Reviews look back that far")).toBeVisible();
  await expect(form.getByLabel("Decizia")).toHaveValue("");
  expect(readBoardState().decisions.filter((d) => d.title === "Keep runs for a year")).toHaveLength(1);
  await page.goto("/board/decisions/p/nebula");
  const row = page.getByRole("article").filter({ hasText: "Keep runs for a year" });
  await expect(row.getByRole("link", { name: /#12@leaseweb/ })).toBeVisible();
  await expect(page.getByRole("article").filter({ hasText: "Vela keeps its schema" })).toHaveCount(0);
});

test("a decision's session link lands on that session on its host, a home decision's too", async ({ page }) => {
  changeBoard((board) => board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000030", title: "Home chores run nightly",
    project: null, card: null, session: { host: os.hostname(), id: "pi-1", name: "Refactor auth" }, content: { body: "" } }));
  await openBoard(page, "/board/decisions");
  const row = page.getByRole("article").filter({ hasText: "Home chores run nightly" });
  const link = row.getByRole("link", { name: /sesiunea Refactor auth/ });
  await expect(link).toHaveAttribute("href", "/p/alpha/s/pi-1");
  await link.click();
  await expect(page).toHaveURL(/\/p\/alpha\/s\/pi-1$/);
  await expect(page.getByLabel("Mesaj către agent")).toBeVisible();
});

test("the sidebar opens the decisions of every project", async ({ page }) => {
  changeBoard((board) => board.decisions.push({ ...board.decisions[0], id: "01JG0000000000000000000021", title: "Vela keeps its schema",
    project: "vela", card: null, content: { body: "" } }));
  await openBoard(page, "/board/p/nebula");
  const entry = page.getByRole("navigation", { name: "Proiecte" }).getByRole("button", { name: "Toate deciziile" });
  await entry.click();
  await expect(page).toHaveURL(/\/board\/decisions$/);
  await expect(entry).toHaveAttribute("aria-current", "page");
  await expect(page.getByRole("article")).toHaveCount(2);
});

test("a project's decisions show although the first page holds only other projects'", async ({ page }) => {
  changeBoard((board) => board.decisions.unshift(...[1, 2, 3].map((n) => ({ ...board.decisions[0], id: `01JG000000000000000000004${n}`,
    title: `Vela decision ${n}`, project: "vela", card: null, content: { body: "" } }))));
  await openBoard(page, "/board/decisions/p/nebula");
  await expect(page.getByRole("article").filter({ hasText: "Copy published files" })).toBeVisible();
  await expect(page.getByRole("article")).toHaveCount(1);
});

test("a failed search shows its error and no earlier results", async ({ page }) => {
  await openBoard(page, "/board/decisions");
  await expect(page.getByRole("article")).toHaveCount(1);
  const search = page.getByRole("searchbox", { name: "Caută decizii" });
  await expect(search).toHaveAttribute("maxlength", "200");
  await page.route(/\/api\/decisions\?.*query=/, (route) => route.fulfill({ status: 400, contentType: "application/json",
    body: JSON.stringify({ error: "Invalid search" }) }));
  await search.fill("copy");
  await expect(page.getByRole("main").getByRole("alert")).toBeVisible();
  await expect(page.getByRole("article")).toHaveCount(0);
});

test("a decision being recorded locks its form until the board answers", async ({ page }) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/decisions", async (route) => {
    if (route.request().method() === "POST") await held;
    await route.continue();
  });
  await openBoard(page, "/board/p/nebula/c/12%40leaseweb");
  const form = page.getByRole("form", { name: "Notează o decizie" });
  await form.getByLabel("Decizia").fill("Keep runs for a year");
  await form.getByRole("button", { name: "Notează" }).click();
  await expect(form.getByLabel("Decizia")).toBeDisabled();
  await expect(form.getByLabel("De ce")).toBeDisabled();
  release();
  await expect(section(page, "Keep runs for a year")).toBeVisible();
  await expect(form.getByLabel("Decizia")).toBeEnabled();
  await expect(form.getByLabel("Decizia")).toHaveValue("");
});

test("an artifact keeps its frame and content when the card is read again with a new grant, and takes the new one after a 403", async ({ page }) => {
  storeArtifact("01JG0000000000000000000010", { "index.html": "<!doctype html><title>r</title><p>recap body</p>" });
  addArtifacts(artifact("01JG0000000000000000000010", "recap", "Recap slice 1", { host: "leaseweb", entry: "index.html" }));
  let regrant: (kind: string) => boolean = (kind) => kind === "e2e-run";
  await page.route("**/api/cards/*", async (route) => {
    const response = await route.fetch();
    const card = await response.json();
    for (const a of card.artifacts ?? []) if (a.src && regrant(a.kind)) a.src = a.src.replace(/^\/a\/[^/]+\//, "/a/1.invalid/");
    await route.fulfill({ response, json: card });
  });
  const runReads: string[] = [];
  page.on("request", (r) => { if (r.url().endsWith("/result.json")) runReads.push(r.url()); });
  const reread = async () => {
    const response = page.waitForResponse((r) => new URL(r.url()).pathname.startsWith("/api/cards/"));
    changeBoard(() => {});
    await response;
  };
  await openBoard(page, "/board/c/12%40leaseweb");
  await expect(page.getByText("Linkul a expirat; reîncarcă pagina.")).toBeVisible();
  await expect(page.frameLocator('iframe[title="Recap slice 1"]').getByText("recap body")).toBeVisible();
  const frame = section(page, "Recap slice 1").locator("iframe");
  const src = await frame.getAttribute("src");

  regrant = () => false;
  await reread();
  const run = section(page, "board.spec");
  await expect(run.getByText(/pași au trecut|trecuți/)).toBeVisible();
  const reads = runReads.length;

  regrant = () => true;
  await reread();
  await page.waitForTimeout(500);
  expect(await frame.getAttribute("src")).toBe(src);
  expect(runReads).toHaveLength(reads);
  await expect(run.getByText(/pași au trecut|trecuți/)).toBeVisible();
  await expect(page.frameLocator('iframe[title="Recap slice 1"]').getByText("recap body")).toBeVisible();
});

test("a card page left open past its grants' expiry still loads its frame and lazy thumbnails with a fresh grant", async ({ page }) => {
  const steps = Array.from({ length: 12 }, (_, i) => ({ name: `step ${i + 1}`, status: "passed", duration_ms: 100, screenshot: `shots/${i + 1}.png` }));
  storeArtifact(RUN, { "result.json": JSON.stringify({ title: "board.spec", status: "passed", started_at: 1764000000000, duration_ms: 1200, steps }),
    ...Object.fromEntries(steps.map((s) => [s.screenshot, PNG])) });
  storeArtifact("01JG0000000000000000000010", { "index.html": "<!doctype html><title>r</title><p>recap body</p>" });
  addArtifacts(artifact("01JG0000000000000000000010", "recap", "Recap slice 1", { host: "leaseweb", entry: "index.html" }));
  await page.clock.install({ time: Date.now() });
  const HELD = "/a/1.held/";
  const real = new Map<string, string>();
  let lapsed = false;
  let expires: number | undefined;
  await page.route("**/api/cards/*", async (route) => {
    const response = await route.fetch();
    const card = await response.json();
    if (!lapsed) {
      expires ??= Math.floor((await page.evaluate(() => Date.now())) / 1000) + 600;
      for (const a of card.artifacts ?? []) if (a.src) {
        real.set(a.id, a.src.split("/")[2]);
        Object.assign(a, { src: a.src.replace(/^\/a\/[^/]+\//, HELD), expires });
      }
    }
    await route.fulfill({ response, json: card });
  });
  await page.route(`**${HELD}**`, async (route) => {
    if (lapsed) return route.fulfill({ status: 403, body: "Artifact link expired" });
    const url = new URL(route.request().url());
    const id = url.pathname.split("/")[3];
    url.pathname = url.pathname.replace(HELD, `/a/${real.get(id)}/`);
    await route.fulfill({ response: await route.fetch({ url: url.href }) });
  });
  await openBoard(page, "/board/c/12%40leaseweb");
  await expect(page.frameLocator('iframe[title="Recap slice 1"]').getByText("recap body")).toBeVisible();
  await expect(section(page, "board.spec").getByRole("img")).toHaveCount(12);

  lapsed = true;
  await page.clock.fastForward("10:00");
  const frame = section(page, "Recap slice 1").locator("iframe");
  await expect(frame).not.toHaveAttribute("src", new RegExp(HELD));
  await expect(page.frameLocator('iframe[title="Recap slice 1"]').getByText("recap body")).toBeVisible();
  const run = section(page, "board.spec");
  await expect(run.getByRole("img")).toHaveCount(12);
  for (const image of await run.getByRole("img").all()) {
    await expect(image).not.toHaveAttribute("src", new RegExp(HELD));
    await image.scrollIntoViewIfNeeded();
    await expect.poll(() => image.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBeGreaterThan(0);
  }
});
