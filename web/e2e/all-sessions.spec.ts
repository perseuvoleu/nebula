import { expect } from "@playwright/test";
import { test, login, readAgents, writeAgents } from "./helpers";

// Synthetic external CLI state only; names and IDs intentionally collide across projects.
test("All preserves mixed-project routes, tabs, sends and explicit creation target", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const rows = readAgents();
  const alpha = rows.find(r => r.id === "pi-1")!;
  const beta = rows.find(r => r.id === "pi-b")!;
  alpha.name = beta.name = "Shared name";
  beta.id = alpha.id;
  alpha.status = beta.status = "running";
  writeAgents(rows);
  const scopes: Array<string | null> = [];
  page.on("request", request => {
    const url = new URL(request.url());
    if (!url.pathname.startsWith("/api/")) return;
    scopes.push(request.method() === "POST" ? request.postDataJSON()?.project ?? null : url.searchParams.get("project"));
  });
  await login(page, "/?view=all");
  const list = page.getByRole("navigation", { name: "Sesiuni în All" });
  await expect(list.getByRole("button", { name: /Shared name/ })).toHaveCount(2);
  await expect(list.getByText("Arhivată", { exact: true })).toHaveCount(0);
  alpha.status = beta.status = "finished";
  writeAgents(rows);
  await expect(list.getByRole("button", { name: "alpha: Shared name, finalizată și nevizualizată", exact: true })).toBeVisible({ timeout: 15000 });
  await list.getByRole("button", { name: "beta: Shared name, finalizată și nevizualizată", exact: true }).click();
  await expect(page).toHaveURL(/\/p\/beta\/s\/pi-1\?view=all$/);
  await expect(page.getByRole("tab", { name: "beta: Shared name", exact: true })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("tab", { name: /^alpha: Shared name/ })).toHaveAttribute("aria-selected", "false");
  await page.getByLabel("Mesaj către agent").fill("Synthetic aggregate send");
  const send = page.waitForRequest(r => new URL(r.url()).pathname === "/api/send");
  await page.getByRole("button", { name: "Trimite", exact: true }).click();
  expect((await send).postDataJSON()).toMatchObject({ project: "beta", id: "pi-1" });
  await page.getByRole("tab", { name: /^alpha: Shared name/ }).click();
  await expect(page).toHaveURL(/\/p\/alpha\/s\/pi-1\?view=all$/);
  await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  await page.reload();
  await expect(page.getByRole("combobox", { name: "Proiect", exact: true })).toHaveValue("");
  await page.getByRole("button", { name: "Agent nou", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: "Alege un proiect" })).toBeVisible();
  await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("beta");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.getByLabel("Prima sarcină").fill("Synthetic new task");
  const create = page.waitForRequest(r => new URL(r.url()).pathname === "/api/new");
  await page.getByRole("button", { name: "Pornește", exact: true }).click();
  expect((await create).postDataJSON()).toMatchObject({ project: "beta" });
  expect(scopes.filter(Boolean).every(p => p === "alpha" || p === "beta")).toBe(true);
});

test("All partial failures are visible and do not replace a failed project's data with another's", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route("**/api/sessions?project=beta", route => route.fulfill({ status: 503, json: { error: "Synthetic unavailable" } }));
  await login(page, "/?view=all");
  await expect(page.getByRole("alert")).toContainText("beta");
  await expect(page.getByRole("button", { name: /Refactor auth/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Beta migration/ })).toHaveCount(0);
  await page.unroute("**/api/sessions?project=beta");
  await expect(page.getByRole("button", { name: /Beta migration/ })).toBeVisible({ timeout: 15000 });
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("priority and activity ordering survives polls, project filters, and persistent read transitions", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const rows = readAgents();
  Object.assign(rows.find(r => r.id === "pi-1")!, { status: "finished", status_changed_at: 900 });
  Object.assign(rows.find(r => r.id === "pi-2")!, { status: "running", status_changed_at: 100 });
  Object.assign(rows.find(r => r.id === "claude-1")!, { status: "needs_feedback", status_changed_at: 200 });
  Object.assign(rows.find(r => r.id === "pi-b")!, { status: "running", status_changed_at: 50 });
  writeAgents(rows);
  await login(page, "/?view=all");
  const names = page.getByRole("navigation").getByRole("button", { name: /Claude pe feature|A doua sesiune Pi|Beta migration|Refactor auth|Pi fără identitate/ });
  const titles = (values: string[]) => values.map(name => new RegExp(name));
  const initial = ["Claude pe feature", "A doua sesiune Pi", "Beta migration", "Refactor auth", "Pi fără identitate"];
  await expect(names).toContainText(titles(initial));
  // A successful second poll must not turn request time into session activity.
  await page.waitForResponse(r => r.url().includes("/api/sessions?project=beta"));
  await expect(names).toContainText(titles(initial));
  Object.assign(rows.find(r => r.id === "pi-b")!, { status: "finished", status_changed_at: 1000 });
  writeAgents(rows);
  const blue = page.getByRole("button", { name: "beta: Beta migration, finalizată și nevizualizată", exact: true });
  await expect(blue).toBeVisible({ timeout: 15000 });
  await expect(blue.getByText("Beta migration", { exact: true })).toHaveCSS("color", "rgb(127, 180, 202)");
  await page.reload();
  await expect(blue).toBeVisible();
  await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("alpha");
  await expect(names).toContainText(titles(["Claude pe feature", "A doua sesiune Pi", "Refactor auth", "Pi fără identitate"]));
  await page.getByRole("combobox", { name: "Proiect", exact: true }).selectOption("");
  await expect(blue).toBeVisible();
  await blue.click();
  await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  await expect(blue).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole("button", { name: /Beta migration/ })).toBeVisible();
  await expect(blue).toHaveCount(0);
  Object.assign(rows.find(r => r.id === "pi-b")!, { status: "running", status_changed_at: 2000 });
  writeAgents(rows);
  await expect(names).toContainText(titles(["Beta migration", "Claude pe feature", "A doua sesiune Pi", "Refactor auth", "Pi fără identitate"]), { timeout: 15000 });
  Object.assign(rows.find(r => r.id === "pi-b")!, { status: "finished", status_changed_at: 3000 });
  writeAgents(rows);
  await expect(blue).toBeVisible({ timeout: 15000 });
});

test("in All a row's age never runs under its Delete button", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const rows = readAgents();
  for (const r of rows) Object.assign(r, { kind: "claude", path: "/srv/alpha-wt" });
  writeAgents(rows);
  await login(page, "/?view=all");
  const list = page.getByRole("navigation", { name: "Sesiuni în All" });
  await expect(list.getByRole("listitem").first()).toBeVisible();
  for (const item of await list.getByRole("listitem").all()) {
    const age = await item.getByTitle("Ultima schimbare de stare").boundingBox();
    const del = await item.getByRole("button", { name: "Șterge sesiunea" }).boundingBox();
    expect(age!.x + age!.width).toBeLessThanOrEqual(del!.x);
  }
});
