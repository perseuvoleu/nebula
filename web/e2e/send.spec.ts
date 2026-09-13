import { expect, test } from "@playwright/test";
import { appendEntry, assistantText, login, sends, setBehaviour, setStatus } from "./helpers";

test.describe("sending", () => {
  test.afterEach(() => setBehaviour("ok"));

  test("one tap sends once, repeated taps cannot duplicate, and the pending bubble reconciles with history", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    const before = sends().length;
    const composer = page.getByLabel("Mesaj către agent");
    await composer.fill("Rulează testele din nou");
    const send = page.getByRole("button", { name: "Trimite" });
    await send.click();
    await send.click({ force: true }).catch(() => undefined);
    await send.click({ force: true }).catch(() => undefined);
    await expect(page.getByText("Rulează testele din nou")).toHaveCount(1, { timeout: 15_000 });
    await expect.poll(() => sends().length).toBe(before + 1);
    // Draft cleared on acceptance; the message appears exactly once as an authoritative user message.
    await expect(composer).toHaveValue("");
    await expect(page.locator("article[data-role=user]", { hasText: "Rulează testele din nou" })).toHaveCount(1, { timeout: 15_000 });
    await expect(page.locator("article[data-attempt]")).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: "Agentul lucrează" })).toBeVisible();
    appendEntry("pi-1", assistantText("Testele au trecut (42 passed)."));
    setStatus("pi-1", "finished");
    await expect(page.getByText("Testele au trecut (42 passed).")).toHaveCount(1);
    await expect(page.locator("article[data-role=user]", { hasText: "Rulează testele din nou" })).toHaveCount(1);
  });

  test("a lost acknowledgement shows uncertain delivery, keeps the text and never resends on its own", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-2");
    const before = sends().length;
    await page.route("**/api/send", async (route) => {
      // The gateway receives the request (the fake CLI logs it) but the browser never sees the reply.
      await new Promise((resolve) => setTimeout(resolve, 300));
      await route.abort("connectionreset");
    });
    await page.getByLabel("Mesaj către agent").fill("Instrucțiune cu confirmare pierdută");
    await page.getByRole("button", { name: "Trimite" }).click();
    const pending = page.locator("article[data-attempt=uncertain]");
    await expect(pending).toBeVisible();
    await expect(pending).toContainText("Livrare incertă");
    await expect(pending).toContainText("Instrucțiune cu confirmare pierdută");
    await expect(pending.getByRole("button", { name: "Retrimite" })).toBeVisible();
    await page.unroute("**/api/send");
    // Reconnect / resume: nothing is replayed automatically.
    await page.context().setOffline(true);
    await page.waitForTimeout(500);
    await page.context().setOffline(false);
    await page.waitForTimeout(3500);
    await expect.poll(() => sends().length).toBe(before + 1);
    await expect(pending).toBeVisible();
  });

  test("a failed send keeps the draft for editing", async ({ page }) => {
    setBehaviour("fail");
    await login(page, "/p/alpha/s/pi-2");
    const composer = page.getByLabel("Mesaj către agent");
    await composer.fill("Va eșua");
    await page.getByRole("button", { name: "Trimite" }).click();
    const failed = page.locator("article[data-attempt=failed]");
    await expect(failed).toContainText("Trimiterea a eșuat");
    await expect(composer).toHaveValue("Va eșua");
    await failed.getByRole("button", { name: "Renunță" }).click();
    await expect(failed).toHaveCount(0);
    await expect(composer).toHaveValue("Va eșua");
  });

  test("drafts are kept per session across switches", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    const composer = page.getByLabel("Mesaj către agent");
    await composer.fill("Ciornă pentru prima sesiune");
    await page.getByRole("button", { name: /A doua sesiune Pi/ }).click();
    await expect(composer).toHaveValue("");
    await composer.fill("Ciornă pentru a doua");
    await page.getByRole("button", { name: /Refactor auth/ }).click();
    await expect(composer).toHaveValue("Ciornă pentru prima sesiune");
    await page.getByRole("button", { name: /A doua sesiune Pi/ }).click();
    await expect(composer).toHaveValue("Ciornă pentru a doua");
  });

  test("starting an agent shows existing checkout activity and selects the new session by id", async ({ page }) => {
    await login(page, "/p/alpha");
    await page.getByRole("button", { name: "Agent nou" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByText(/Pe acest checkout lucrează deja/)).toContainText("Refactor auth");
    await dialog.getByLabel("Prima sarcină").fill("Scrie un README nou");
    await dialog.getByRole("button", { name: "Pornește" }).click();
    await expect(page).toHaveURL(/\/p\/alpha\/s\/new-[0-9a-f]{6}$/);
    await expect(page.getByRole("log").getByText("Scrie un README nou")).toBeVisible();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Mobile /);
  });
});
