import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { login } from "../helpers";

type Card = { number: number; title: string; stage: string; sessions: { name: string; role: string }[] };

const LAUNCHER = path.resolve(__dirname, "../real_nebula.sh");
const REPO = path.join(process.env.REAL_NEBULA_STATE!, "repo");
const RUN = path.resolve(__dirname, "../../..", process.env.REAL_E2E_RUN ?? ".scratch/mobile-chat-e2e/e2e-run");
// The default source, the fake lane's whole run, is the 12-screenshot load case; REAL_E2E_RUN may name this lane's own smaller run.
const MIN_SHOTS = process.env.REAL_E2E_RUN ? 1 : 12;

const nebula = (...args: string[]) => execFileSync(LAUNCHER, args, { cwd: REPO, encoding: "utf8" });
const show = (number: number): Card => JSON.parse(nebula("todo", "show", String(number), "--project", "repo", "--json"));
const add = (title: string): Card => JSON.parse(nebula("todo", "add", "--project", "repo", "--json", title));
const column = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const tile = (scope: Locator, title: string) => scope.getByRole("article").filter({ hasText: title });
const section = (page: Page, title: string) => page.getByRole("region", { name: title, exact: true });

test("a card made on the web is the daemon's card: a drag moves it there and a CLI move shows on the web", async ({ page }) => {
  await login(page, "/board/p/repo");
  await page.getByRole("button", { name: "Card nou", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Card nou" });
  await dialog.getByLabel("Titlu").fill("Ship the real lane");
  await dialog.getByRole("button", { name: "Creează cardul" }).click();
  await expect(tile(column(page, "Backlog"), "Ship the real lane")).toBeVisible();
  const { board }: { board: { cards: Card[] } } = JSON.parse(nebula("todo", "board", "--project", "repo", "--json"));
  const made = board.cards.find((c) => c.title === "Ship the real lane")!;
  expect(made.stage).toBe("backlog");
  await tile(column(page, "Backlog"), "Ship the real lane").dragTo(column(page, "Urmează"));
  await expect.poll(() => show(made.number).stage).toBe("next");
  nebula("todo", "move", String(made.number), "doing");
  await expect(tile(column(page, "În lucru"), "Ship the real lane")).toBeVisible();
});

test("Run starts the replay agent on the card, and its finished turn moves the card to review", async ({ page }) => {
  const card = add("Run the replay agent");
  await login(page, `/board/p/repo/c/${card.number}`);
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Pornește un agent pe card" });
  await expect(dialog.getByRole("radio", { name: "worker" })).toBeChecked();
  await dialog.getByText("Claude", { exact: true }).click();
  await dialog.getByRole("button", { name: "Pornește", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => show(card.number).stage).toBe("doing");
  const [worker] = show(card.number).sessions;
  expect(worker.role).toBe("worker");
  nebula("agent", "send", "--project", "repo", "--", worker.name, "go");
  await page.getByRole("button", { name: "Board" }).click();
  await expect(tile(column(page, "Review"), "Run the replay agent")).toBeVisible();
});

test("a decision recorded on the card is found by the decisions search", async ({ page }) => {
  const card = add("Decide the poll");
  await login(page, `/board/p/repo/c/${card.number}`);
  const form = page.getByRole("form", { name: "Notează o decizie" });
  await form.getByLabel("Decizia").fill("Poll the real board every three seconds");
  await form.getByLabel("De ce").fill("One CLI call per poll is cheap");
  await form.getByRole("button", { name: "Notează" }).click();
  await expect(section(page, "Poll the real board every three seconds").getByText("One CLI call per poll is cheap")).toBeVisible();
  nebula("decision", "add", "Keep the drag handle on the left", "--why", "Thumbs rest there", "--card", String(card.number));
  await page.goto("/board/decisions");
  await expect(page.getByRole("article")).toHaveCount(2);
  await page.getByRole("searchbox", { name: "Caută decizii" }).fill("three seconds");
  await expect(page.getByRole("article")).toHaveCount(1);
  await expect(page.getByRole("article").filter({ hasText: "Keep the drag handle on the left" })).toHaveCount(0);
  await expect(page.getByRole("article").getByRole("link", { name: new RegExp(`#${card.number}`) })).toBeVisible();
  expect(nebula("decision", "list", "--project", "repo")).toContain("Poll the real board every three seconds");
});

test("a reporter run published with nebula artifact add opens with every screenshot, beside a recap in its sandbox", async ({ page }) => {
  expect(fs.existsSync(path.join(RUN, "result.json")), `no reporter run in ${RUN}; run the fake lane first`).toBe(true);
  const run = JSON.parse(fs.readFileSync(path.join(RUN, "result.json"), "utf8")) as { steps: { name: string; screenshot?: string }[] };
  const shots = run.steps.filter((s) => s.screenshot);
  expect(shots.length, `${RUN} must hold a reporter run with at least ${MIN_SHOTS} screenshots`).toBeGreaterThanOrEqual(MIN_SHOTS);
  const recap = test.info().outputPath("recap");
  fs.mkdirSync(recap, { recursive: true });
  fs.writeFileSync(path.join(recap, "index.html"), "<!doctype html><title>r</title><p id=out>static</p><script>out.textContent = 'recap script ran'</script>");
  const card = add("Show the runs");
  nebula("artifact", "add", RUN, "--kind", "e2e-run", "--title", "Reporter run", "--card", String(card.number), "--json");
  nebula("artifact", "add", recap, "--kind", "recap", "--title", "Real recap", "--card", String(card.number), "--json");
  const images = section(page, "Reporter run").getByRole("img");
  const incomplete = () => images.evaluateAll((imgs: HTMLImageElement[]) => imgs.filter((img) => !(img.complete && img.naturalWidth)).length);
  let incompleteAtPoll: Promise<number> | undefined;
  await page.route("**/a/**/shots/*.png", async (route) => {
    const response = await route.fetch();
    incompleteAtPoll ??= page.waitForResponse((r) => new URL(r.url()).pathname === "/api/board" && r.ok()).then(incomplete);
    await incompleteAtPoll;
    await route.fulfill({ response });
  });
  await login(page, `/board/p/repo/c/${card.number}`);
  await expect(images).toHaveCount(shots.length);
  for (const image of await images.all()) await image.scrollIntoViewIfNeeded();
  await expect.poll(incomplete).toBe(0);
  expect(await incompleteAtPoll, "a board poll succeeded while screenshots were still loading").toBeGreaterThan(0);
  const frame = section(page, "Real recap").locator("iframe");
  await frame.scrollIntoViewIfNeeded();
  await expect(frame).toHaveAttribute("sandbox", "allow-scripts");
  await expect(page.frameLocator('iframe[title="Real recap"]').getByText("recap script ran")).toBeVisible();
});

test("HOME at / reads the daemon's board: a CLI card is listed by stage, opens in the card pane, and its decision is in the rail", async ({ page }) => {
  const card = add("Land on HOME");
  nebula("decision", "add", "HOME is the landing page", "--why", "Agent first", "--card", String(card.number));
  await login(page, "/");
  await page.getByRole("region", { name: "Backlog", exact: true }).getByRole("button", { name: /Land on HOME/ }).click();
  const pane = page.getByRole("region", { name: `Card #${card.number}` });
  await expect(pane.getByRole("heading", { name: "Land on HOME" })).toBeVisible();
  await expect(pane.getByRole("region", { name: "Jurnal" })).toContainText("HOME is the landing page");
  await expect(page.getByRole("complementary", { name: "Context și decizii" })).toContainText("HOME is the landing page");
  await page.keyboard.press("2");
  await expect(page.getByRole("combobox", { name: "Proiect", exact: true })).toBeVisible();
});
