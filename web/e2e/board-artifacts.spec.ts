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
  await openBoard(page, "/board/c/12%40leaseweb");
  const run = section(page, "board.spec");
  await expect(run.getByRole("img")).toHaveCount(12);
  const polled = page.waitForResponse((r) => r.url().includes("/api/board") && r.ok());
  for (const [i, image] of (await run.getByRole("img").all()).entries()) {
    await image.scrollIntoViewIfNeeded();
    await expect(image).toHaveAccessibleName(`Captură: step ${i + 1}`);
    await expect.poll(() => image.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBeGreaterThan(0);
  }
  await polled;
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
  await expect(row.getByRole("link", { name: /sesiunea worker/ })).toHaveAttribute("href", `/p/nebula/s/${SESSION.id}`);
  await row.getByRole("link", { name: /#12@leaseweb/ }).click();
  await expect(page).toHaveURL(/\/board\/p\/nebula\/c\/12%40leaseweb$/);
  await expect(page.getByRole("heading", { name: "Keep verification results" })).toBeVisible();
});

test("record decision", async ({ page }) => {
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
});
