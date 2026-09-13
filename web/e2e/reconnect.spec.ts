import { expect } from "@playwright/test";
import { test, appendEntry, assistantText, login, TOKEN } from "./helpers";

test.describe("reconnection, logout and PWA/security invariants", () => {
  test("offline → online restores the authoritative conversation without duplicates and keeps the draft", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    const log = page.getByRole("log");
    await expect(log.locator("article")).toHaveCount(6);
    const composer = page.getByLabel("Mesaj către agent");
    await composer.fill("ciornă în timpul deconectării");
    await page.context().setOffline(true);
    await page.waitForTimeout(3500);
    await expect(page.getByRole("status").filter({ hasText: /Fără conexiune|Reconectare/ })).toBeVisible();
    appendEntry("pi-1", assistantText("Mesaj sosit cât ai fost offline."));
    await page.context().setOffline(false);
    await expect(log.getByText("Mesaj sosit cât ai fost offline.")).toHaveCount(1, { timeout: 15_000 });
    await expect(log.locator("article")).toHaveCount(7);
    await expect(composer).toHaveValue("ciornă în timpul deconectării");
    await expect(page.getByRole("status").filter({ hasText: /Sincronizat/ })).toBeVisible();
  });

  test("background/resume re-syncs from the cursor and a native reset replaces rather than appends", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-2");
    const log = page.getByRole("log");
    await expect(log.locator("article")).toHaveCount(2);
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, value: true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    // Branch from the first user entry, abandoning the old assistant reply.
    appendEntry("pi-2", { role: "user", content: "Reluăm de la început", timestamp: Date.now() }, "branch1", "x1");
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, value: false });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await expect(log.getByText("Reluăm de la început")).toBeVisible();
    await expect(log.locator("article")).toHaveCount(2);
    await expect(log.getByText("A doua sesiune Pi lucrează la altceva pe același checkout.")).toHaveCount(0);
  });

  test("logout clears credentials and late responses cannot repopulate the UI", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    await expect(page.getByRole("log")).toBeVisible();
    let release!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    await page.route("**/api/conversation/updates**", async (route) => { await held; await route.continue(); });
    await page.waitForTimeout(3000); // a poll is now held by the route
    await page.getByRole("button", { name: "Deconectare" }).click();
    await expect(page.getByLabel("Token de acces")).toBeVisible();
    release();
    await page.waitForTimeout(800);
    await expect(page.getByRole("log")).toHaveCount(0);
    expect(await page.evaluate(() => sessionStorage.getItem("nebula-token"))).toBeNull();
    expect(await page.evaluate(() => document.body.innerText)).not.toContain("Refactorizează modulul");
    await page.reload();
    await expect(page.getByLabel("Token de acces")).toBeVisible();
  });

  test("service worker caches only the public shell, never API responses; token never in URL", async ({ page }) => {
    await login(page, "/p/beta/s/pi-b");
    await expect(page.getByRole("log").getByText("Migrarea beta este pregătită: 3 tabele, 0 conflicte.", { exact: true })).toBeVisible();
    await page.evaluate(async () => { await navigator.serviceWorker.ready; });
    await page.waitForTimeout(1500);
    const cached = await page.evaluate(async () => {
      const keys = await caches.keys();
      const urls: string[] = [];
      for (const key of keys) for (const request of await (await caches.open(key)).keys()) urls.push(new URL(request.url).pathname);
      return { keys, urls };
    });
    expect(cached.keys.some((k) => k.startsWith("nebula-shell-"))).toBe(true);
    expect(cached.urls.filter((u) => u.startsWith("/api/"))).toEqual([]);
    expect(page.url()).not.toContain(TOKEN);
    const response = await page.request.get("/api/projects", { headers: { Authorization: `Bearer ${TOKEN}` } });
    expect(response.headers()["cache-control"]).toBe("no-store");
  });

  test("a bad token gets a specific explanation and nothing is stored", async ({ page }) => {
    await page.goto("/");
    await page.getByLabel("Token de acces").fill("x".repeat(40));
    await page.getByRole("button", { name: "Conectare" }).click();
    await expect(page.getByRole("alert")).toContainText("NEBULA_WEB_TOKEN");
    expect(await page.evaluate(() => sessionStorage.getItem("nebula-token"))).toBeNull();
    // The same mounted form can recover with valid credentials.
    await page.getByLabel("Token de acces").fill(TOKEN);
    await page.getByRole("button", { name: "Conectare" }).click();
    await expect(page.getByRole("combobox", { name: "Proiect", exact: true })).toBeVisible();
  });
});
