import { expect, type Page } from "@playwright/test";
import { test, appendEntry, assistantText, login, noHorizontalOverflow, shot } from "./helpers";

const VIEWPORTS = {
  "phone-320": { width: 320, height: 568 },
  "phone-390": { width: 390, height: 844 },
  "landscape-844x390": { width: 844, height: 390 },
  "tablet-820": { width: 820, height: 1180 },
  "desktop-1440": { width: 1440, height: 900 },
  "desktop-1920": { width: 1920, height: 1080 },
};

async function expectSendVisible(page: Page) {
  const box = await page.getByRole("button", { name: "Trimite" }).boundingBox();
  const viewport = page.viewportSize()!;
  expect(box, "send button must be laid out").not.toBeNull();
  expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height);
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width);
  expect(box!.height).toBeGreaterThanOrEqual(40);
}

for (const [name, viewport] of Object.entries(VIEWPORTS)) {
  test(`layout at ${name}: no horizontal overflow, reachable composer, long code block contained`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await login(page, "/p/alpha/s/pi-1");
    await expect(page.getByRole("log").locator("pre code", { hasText: "def verify" })).toBeVisible();
    await noHorizontalOverflow(page);
    await expectSendVisible(page);
    const pre = page.getByRole("log").locator("pre").filter({ hasText: "def verify" }).first();
    const width = await pre.evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth, doc: document.documentElement.scrollWidth, win: window.innerWidth }));
    expect(width.client).toBeLessThanOrEqual(width.win);
    await shot(page, `${name}-conversation`);
  });
}

test("phone: list → conversation → back, sheet navigation, project switch and login screenshots", async ({ page }) => {
  await page.setViewportSize(VIEWPORTS["phone-390"]);
  await page.goto("/");
  await shot(page, "phone-390-login");
  await login(page);
  await expect(page.getByRole("heading", { name: "Sesiuni" })).toBeVisible();
  await noHorizontalOverflow(page);
  await shot(page, "phone-390-sessions");
  await page.getByRole("button", { name: /Refactor auth/ }).click();
  await expect(page.getByRole("log")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Sesiuni" })).toHaveCount(0);
  await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  await expect(page.getByRole("heading", { name: "Sesiuni" })).toBeVisible();
  await expect(page).toHaveURL(/\/p\/alpha$/);
  // Browser back from a conversation also returns to the list.
  await page.getByRole("button", { name: /Refactor auth/ }).click();
  await expect(page.getByRole("log")).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("heading", { name: "Sesiuni" })).toBeVisible();
  // Second project on the phone.
  await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("beta");
  await page.getByRole("button", { name: /Beta migration/ }).click();
  await expect(page.getByRole("log").getByText("Migrarea beta este pregătită: 3 tabele, 0 conflicte.", { exact: true })).toBeVisible();
  await noHorizontalOverflow(page);
  await shot(page, "phone-390-beta-conversation");
  // Network error state is distinguishable from an empty list.
  await page.context().setOffline(true);
  await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("alpha");
  await expect(page.getByRole("status").filter({ hasText: /Fără conexiune|Reconectare/ }).or(page.getByRole("alert"))).toBeVisible({ timeout: 15_000 });
  await shot(page, "phone-390-offline");
  await page.context().setOffline(false);
});

test("phone: follow-output only near the bottom, jump-to-latest otherwise; keyboard focus order", async ({ page }) => {
  await page.setViewportSize(VIEWPORTS["phone-390"]);
  await login(page, "/p/alpha/s/pi-1");
  const log = page.getByRole("log");
  await expect(log.locator("pre code", { hasText: "def verify" })).toBeVisible();
  const scroller = log.locator("> div").first();
  // Reading an earlier answer: scroll to the top, then a new message arrives.
  await scroller.evaluate((el) => { el.scrollTop = 0; });
  await page.waitForTimeout(300);
  appendEntry("pi-1", assistantText("Mesaj nou în timp ce citeai mai sus."));
  await expect(page.getByRole("button", { name: "Sari la ultimul mesaj" })).toBeVisible({ timeout: 15_000 });
  const top = await scroller.evaluate((el) => el.scrollTop);
  expect(top).toBeLessThan(80);
  await page.getByRole("button", { name: "Sari la ultimul mesaj" }).click();
  await expect(log.getByText("Mesaj nou în timp ce citeai mai sus.")).toBeInViewport();
  await expect(page.getByRole("button", { name: "Sari la ultimul mesaj" })).toBeHidden();
  await shot(page, "phone-390-alpha-bottom");
  // Keyboard: Tab reaches the composer and the send button; Enter on a coarse-pointer device inserts a newline.
  await page.getByLabel("Mesaj către agent").focus();
  await page.keyboard.type("linia unu");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("linia doi");
  await expect(page.getByLabel("Mesaj către agent")).toHaveValue("linia unu\nlinia doi");
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Trimite" })).toBeFocused();
});

test("desktop: Enter sends, Shift+Enter inserts a newline, sidebar stays beside the conversation", async ({ page }) => {
  await page.setViewportSize(VIEWPORTS["desktop-1440"]);
  await login(page, "/p/alpha/s/pi-2");
  await expect(page.getByRole("navigation", { name: /Sesiuni în alpha/ })).toBeVisible();
  const projectBox = await page.getByRole("combobox", { name: "Proiect", exact: true }).boundingBox();
  expect(projectBox!.width, "host metadata must leave the project picker readable").toBeGreaterThanOrEqual(100);
  const composer = page.getByLabel("Mesaj către agent");
  await composer.focus();
  await page.keyboard.type("prima");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("a doua");
  await expect(composer).toHaveValue("prima\na doua");
  await page.keyboard.press("Enter");
  await expect(composer).toHaveValue("");
  await expect(page.getByText("prima\na doua")).toBeVisible();
  await shot(page, "desktop-1440-after-send");
});
