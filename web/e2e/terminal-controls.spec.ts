import fs from "node:fs";
import path from "node:path";
import { expect } from "@playwright/test";
import { test, login, sends, setBehaviour, STATE, readAgents, writeAgents } from "./helpers";

test.use({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });

test("terminal keys send independently of the composer and failures are not retried", async ({ page }) => {
  await login(page, "/p/alpha/s/claude-1");
  const keys = page.getByRole("group", { name: "Taste terminal" });
  for (const key of ["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"]) {
    await keys.getByRole("button", { name: key, exact: true }).click();
    await expect(keys.getByRole("status")).toHaveText(`${key}: livrat către PTY`);
  }
  expect(sends().map(s => [s.id, s.key])).toEqual(["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"].map(k => ["claude-1", k]));
  await expect(page.getByLabel("Mesaj către agent")).toHaveValue("");
  setBehaviour("fail");
  await keys.getByRole("button", { name: "Enter", exact: true }).click();
  await expect(keys.getByRole("alert")).toContainText("PTY closed");
  await expect(keys.getByRole("status")).toHaveCount(0);
  await page.waitForTimeout(3000);
  expect(sends()).toHaveLength(8);
  await page.getByLabel("Mesaj către agent").fill("composer stays independent");
  expect(sends()).toHaveLength(8);
});

test("key buttons stay disabled until delivery completes", async ({ page }) => {
  await login(page, "/p/alpha/s/claude-1");
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/key", async route => { await gate; await route.fulfill({ json: { ok: true } }); });
  const keys = page.getByRole("group", { name: "Taste terminal" });
  await keys.getByRole("button", { name: "Up", exact: true }).click();
  for (const button of await keys.getByRole("button").all()) await expect(button).toBeDisabled();
  release();
  await expect(keys.getByRole("button", { name: "Up", exact: true })).toBeEnabled();
});

test("session deletion cancel, failure and success preserve exact selection semantics", async ({ page }) => {
  await login(page);
  const button = page.getByRole("listitem").filter({ hasText: "Refactor auth" }).getByRole("button", { name: "Șterge sesiunea", exact: true });
  page.once("dialog", async dialog => { expect(dialog.message()).toContain("ID: pi-1"); await dialog.dismiss(); });
  await button.click();
  expect(sends()).toHaveLength(0);
  setBehaviour("fail");
  page.once("dialog", dialog => dialog.accept());
  await button.click();
  await expect(page.getByRole("alert")).toContainText("PTY closed");
  expect(readAgents().some(a => a.id === "pi-1")).toBeTruthy();
  setBehaviour("ok");
  page.once("dialog", dialog => dialog.accept());
  await button.click();
  await expect(button).toHaveCount(0);
  expect(readAgents().some(a => a.id === "pi-1")).toBeFalsy();
  await expect(page.getByRole("listitem").filter({ hasText: "A doua sesiune Pi" }).getByRole("button", { name: "Șterge sesiunea", exact: true })).toBeVisible();
  expect(sends()).toHaveLength(2);
});

test("deletion keeps its captured ID across rename and selection changes, then clears an active tab", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await login(page, "/p/alpha/s/pi-1");
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/sessions/delete", async route => { await gate; await route.continue(); });
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("listitem").filter({ hasText: "Refactor auth" }).getByRole("button", { name: "Șterge sesiunea", exact: true }).click();
  const rows = readAgents(); rows.find(a => a.id === "pi-1")!.name = "Renamed while deleting"; writeAgents(rows);
  await page.getByRole("navigation", { name: "Sesiuni în alpha" }).getByRole("button", { name: /A doua sesiune Pi/ }).click();
  release();
  await expect.poll(() => readAgents().some(a => a.id === "pi-1")).toBeFalsy();
  await expect(page).toHaveURL(/\/s\/pi-2$/);
  await page.unroute("**/api/sessions/delete");
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("listitem").filter({ hasText: "A doua sesiune Pi" }).getByRole("button", { name: "Șterge sesiunea", exact: true }).click();
  await expect(page).toHaveURL(/\/p\/alpha$/);
  await expect(page.getByRole("listitem").filter({ hasText: "A doua sesiune Pi" })).toHaveCount(0);
  expect(sends().map(s => s.id)).toEqual(["pi-1", "pi-2"]);
});

test("checkout removal shows protection, affected sessions, cancel and refreshed selection", async ({ page }) => {
  await login(page);
  await expect(page.getByRole("button", { name: "Elimină worktree · protejat", exact: true })).toBeDisabled();
  const picker = page.getByRole("combobox", { name: "Branch / worktree", exact: true });
  await picker.selectOption("/srv/alpha-wt");
  await page.getByRole("button", { name: "Elimină worktree", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("/srv/alpha-wt");
  await expect(dialog).toContainText("Claude pe feature");
  await expect(dialog.getByRole("alert")).toContainText("Sessions still exist");
  await expect(dialog.getByRole("button", { name: "Confirmă eliminarea worktree-ului" })).toBeDisabled();
  await dialog.getByRole("button", { name: "Anulează" }).click();
  writeAgents(readAgents().filter(a => a.id !== "claude-1"));
  const file = path.join(STATE, "worktrees.json");
  const trees = JSON.parse(fs.readFileSync(file, "utf8"));
  trees.find((w: { id: string }) => w.id === "wt-feature").dirty = true;
  fs.writeFileSync(file, JSON.stringify(trees));
  await page.getByRole("button", { name: "Elimină worktree", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("Dirty checkout");
  await dialog.getByRole("button", { name: "Anulează" }).click();
  trees.find((w: { id: string }) => w.id === "wt-feature").dirty = false;
  fs.writeFileSync(file, JSON.stringify(trees));
  await page.getByRole("button", { name: "Elimină worktree", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Confirmă eliminarea worktree-ului" })).toBeEnabled();
  await dialog.getByRole("button", { name: "Anulează" }).click();
  expect(sends()).toHaveLength(0);
  await page.getByRole("button", { name: "Elimină worktree", exact: true }).click();
  await dialog.getByRole("button", { name: "Confirmă eliminarea worktree-ului" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(picker).toHaveValue("/srv/alpha");
  expect(sends()).toEqual([{ action: "delete-safe", id: "wt-feature" }]);
  await expect(picker.locator("option", { hasText: "feature/homepage" })).toHaveCount(0);
});
