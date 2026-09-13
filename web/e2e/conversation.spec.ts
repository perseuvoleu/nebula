import { expect } from "@playwright/test";
import { test, appendEntry, assistantText, login, readAgents, shot, writeAgents } from "./helpers";

test.describe("selection and structured conversation", () => {
  test("login → second project → existing session renders real structured reply, not loading", async ({ page }) => {
    await login(page);
    await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("beta");
    await expect(page).toHaveURL(/\/p\/beta$/);
    await page.getByRole("button", { name: /Beta migration/ }).click();
    await expect(page).toHaveURL(/\/p\/beta\/s\/pi-b$/);
    const conversation = page.getByRole("log");
    await expect(conversation.getByText("Pregătește migrarea bazei de date beta.")).toBeVisible();
    await expect(conversation.locator("strong", { hasText: "beta" })).toBeVisible();
    await expect(page.getByLabel("Se încarcă conversația")).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: /Sincronizat/ })).toBeVisible();
    await shot(page, "desktop-beta-conversation");
  });

  test("markdown, tools, hidden context and truncated output on the alpha session", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    const log = page.getByRole("log");
    await expect(log.getByRole("heading", { name: "Ce s-a schimbat" })).toBeVisible();
    // Code block renders in a scrollable monochrome block with a copy control.
    await expect(log.locator("pre code", { hasText: "def verify" })).toBeVisible();
    await expect(log.getByRole("button", { name: "Copiază codul" }).first()).toBeAttached();
    // Unsafe content is inert: no script element, javascript: link has no href.
    await expect(log.locator("script")).toHaveCount(0);
    await expect(log.getByText('<script>alert("nu se execută")</script>')).toBeVisible();
    await expect(log.getByRole("link", { name: "click" })).not.toHaveAttribute("href", /javascript/);
    await expect(log.getByRole("link", { name: "documentația JWT" })).toHaveAttribute("target", "_blank");
    // Hidden reasoning and hidden context never reach the DOM.
    await expect(page.locator("body")).not.toContainText("hidden reasoning");
    await expect(page.locator("body")).not.toContainText("HIDDEN CONTEXT");
    // Tool activity is collapsed, expands, shows an error state and loads the full bounded output.
    const bash = log.locator("[data-tool-state=error]", { hasText: "make test" });
    await expect(bash).toBeVisible();
    await bash.getByRole("button", { name: /bash/ }).click();
    await expect(bash.getByText("Afișat parțial.")).toBeVisible();
    await bash.getByRole("button", { name: "Încarcă tot" }).click();
    await expect(bash.getByText("[0399] compiling module 399")).toBeVisible();
    await shot(page, "desktop-alpha-tool-expanded");
  });

  test("rename keeps the selection and a stale response cannot overwrite a newer selection", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-1");
    const log = page.getByRole("log");
    await expect(log.getByText("Refactorizează modulul de autentificare")).toBeVisible();
    // Delay the first session's conversation load, switch to another session meanwhile.
    let release!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    await page.route(/\/api\/conversation\?.*id=pi-1/, async (route) => { await held; await route.continue(); });
    await page.reload();
    await page.getByRole("button", { name: /A doua sesiune Pi/ }).click();
    await expect(page).toHaveURL(/\/s\/pi-2$/);
    await expect(log.getByText("A doua sesiune Pi lucrează la altceva")).toBeVisible();
    release();
    await page.waitForTimeout(500);
    await expect(log.getByText("Refactorizează modulul de autentificare")).toHaveCount(0);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("A doua sesiune Pi");
    // The agent renames its session: same id stays selected, title follows.
    const rows = readAgents();
    rows.find((r) => r.id === "pi-2")!.name = "Nume nou dat de agent";
    writeAgents(rows);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Nume nou dat de agent", { timeout: 15_000 });
    await expect(page).toHaveURL(/\/s\/pi-2$/);
  });

  test("messages written outside the web UI appear once, in order, with tool completion", async ({ page }) => {
    await login(page, "/p/alpha/s/pi-2");
    const log = page.getByRole("log");
    await expect(log.getByText("A doua sesiune Pi lucrează")).toBeVisible();
    appendEntry("pi-2", { role: "user", content: "Mesaj scris din terminal", timestamp: Date.now() });
    await expect(log.getByText("Mesaj scris din terminal")).toHaveCount(1);
    appendEntry("pi-2", { ...assistantText(""), content: [{ type: "toolCall", id: "late-call", name: "bash", arguments: { command: "ls" } }], stopReason: "toolUse" }, "late-a");
    await expect(log.locator("[data-tool-state=pending]", { hasText: "ls" })).toBeVisible();
    appendEntry("pi-2", { role: "toolResult", toolCallId: "late-call", toolName: "bash", content: [{ type: "text", text: "README.md" }], isError: false, timestamp: Date.now() });
    await expect(log.locator("[data-tool-state=done]", { hasText: "ls" })).toBeVisible();
    await expect(log.locator("[data-tool-state]", { hasText: "ls" })).toHaveCount(1);
    appendEntry("pi-2", assistantText("Gata din terminal."));
    await expect(log.getByText("Gata din terminal.")).toHaveCount(1);
    const order = await log.locator("article").allInnerTexts();
    expect(order.findIndex((t) => t.includes("Mesaj scris din terminal"))).toBeLessThan(order.findIndex((t) => t.includes("Gata din terminal.")));
  });
});

test.describe("capability fallback", () => {
  test("non-Pi agents get explicit terminal mode; a Pi session without identity is not an empty chat", async ({ page }) => {
    await login(page, "/p/alpha/s/claude-1");
    await expect(page.getByText("Conversație structurată indisponibilă")).toBeVisible();
    await expect(page.getByText("Output de terminal, actualizat periodic. Nu este istoric de conversație.")).toBeVisible();
    await expect(page.getByLabel("Output de terminal").getByText("terminal snapshot for Claude pe feature")).toBeVisible();
    await expect(page.getByRole("log")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Trimite" })).toBeVisible();
    await shot(page, "desktop-terminal-fallback");
    await page.getByRole("button", { name: /Pi fără identitate/ }).click();
    await expect(page.getByText(/nu a înregistrat|identity|identitate/i).first()).toBeVisible();
    await expect(page.getByText("Sesiunea nu are încă mesaje persistate")).toHaveCount(0);
  });
});
