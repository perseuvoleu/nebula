import { expect } from "@playwright/test";
import { test, login, STATE } from "./helpers";
import { writeFileSync } from "node:fs";

test("outgoing messages and composer are white/black; terminal scrolling has no visible bar", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 500 });
  await login(page, "/p/alpha/s/pi-1");
  const bubble = page.locator("article[data-role=user] > div").first();
  await expect(bubble).toHaveCSS("background-color", "rgb(255, 255, 255)");
  await expect(bubble).toHaveCSS("color", "rgb(0, 0, 0)");
  const input = page.getByLabel("Mesaj către agent");
  await expect(input).toHaveCSS("background-color", "rgb(255, 255, 255)");
  await expect(input).toHaveCSS("color", "rgb(0, 0, 0)");
  await page.goto("/p/alpha/s/claude-1");
  await page.getByRole("button", { name: "Terminal brut", exact: true }).click();
  const output = page.getByLabel("Conținut terminal", { exact: true });
  await expect(output).toContainText("line 39:");
  await expect(output).toHaveCSS("scrollbar-width", "none");
  expect(await output.evaluate((el) => { el.scrollTop = 50; return el.scrollTop; })).toBe(50);
  // Even without native Claude history, the actual web send remains visible, not a guessed transcript.
  await input.fill("Mesaj vizibil către Claude");
  await page.getByRole("button", { name: "Trimite", exact: true }).click();
  const sent = page.locator("article[data-attempt=accepted]");
  await expect(sent).toContainText("Mesaj vizibil către Claude");
  await expect(sent).toContainText("Trimis către sesiune");
  await expect(sent.locator("div").first()).toHaveCSS("background-color", "rgb(255, 255, 255)");
});

test("chat tabs navigate without losing drafts; working session names are yellow", async ({ page }) => {
  // With motion allowed the yellow comes from an animated text gradient and `color` is transparent.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await login(page, "/p/alpha/s/pi-1");
  const nav = page.getByRole("navigation", { name: "Sesiuni în alpha" });
  await expect(nav.getByText("A doua sesiune Pi", { exact: true })).toHaveCSS("color", "rgb(250, 204, 21)");
  await page.getByLabel("Mesaj către agent").fill("Ciornă păstrată prin taburi");
  await page.getByRole("tab", { name: "A doua sesiune Pi", exact: true }).click();
  await expect(page).toHaveURL(/\/s\/pi-2$/);
  await expect(page.getByLabel("Mesaj către agent")).toHaveValue("");
  await page.getByRole("tab", { name: "Refactor auth", exact: true }).click();
  await expect(page.getByLabel("Mesaj către agent")).toHaveValue("Ciornă păstrată prin taburi");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("tab", { name: "Claude pe feature", exact: true }).click();
  await expect(page.getByLabel("Output de terminal")).toBeVisible();
  await expect(page.getByText(/Conversație structurată indisponibilă/)).toHaveCount(0);
  await expect(page.getByRole("tablist")).toHaveCSS("scrollbar-width", "none");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)).toBe(true);
});

test("scrolling up loads older retained terminal output beyond the initial 250 lines", async ({ page }) => {
  writeFileSync(`${STATE}/output-lines`, "600");
  await login(page, "/p/alpha/s/claude-1");
  await page.getByRole("button", { name: "Terminal brut", exact: true }).click();
  const output = page.getByLabel("Conținut terminal", { exact: true });
  await expect(output).toContainText("line 599:");
  await expect(output).not.toContainText("line 1:");
  await output.evaluate(el => { el.scrollTop = 0; });
  await expect(output).toContainText("line 100:");
  // The reader remains anchored after prepending instead of being pushed to the bottom.
  expect(await output.evaluate(el => el.scrollTop > 100 && el.scrollTop < el.scrollHeight - el.clientHeight - 80)).toBe(true);
  await output.evaluate(el => { el.scrollTop = 0; });
  await expect(output).toContainText("line 1:");
  await expect(page.getByRole("button", { name: "Output mai vechi" })).toHaveCount(0);
});

test("iPhone PWA has install guidance, standalone manifest, touch icon and a ready service worker", async ({ page, request }) => {
  await page.goto("/");
  await page.getByText("Instalează pe telefon", { exact: true }).click();
  await expect(page.getByText(/Safari.*Partajare.*ecranul principal/)).toBeVisible();
  await expect(page.locator('meta[name="apple-mobile-web-app-capable"]')).toHaveAttribute("content", "yes");
  const manifest = await (await request.get("/manifest.webmanifest")).json();
  expect(manifest).toMatchObject({ display: "standalone", start_url: "/", scope: "/" });
  const icon = await page.locator('link[rel="apple-touch-icon"]').getAttribute("href");
  expect((await request.get(icon!)).status()).toBe(200);
  for (const asset of manifest.icons) expect((await request.get(asset.src)).status()).toBe(200);
  await page.evaluate(() => navigator.serviceWorker.ready);
});
