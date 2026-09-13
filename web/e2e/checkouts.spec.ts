import { expect } from "@playwright/test";
import { test, login, STATE } from "./helpers";
import { readFileSync } from "node:fs";

test("sidebar creates a worktree from the selected session branch and preselects it for the next agent", async ({ page }) => {
  await login(page, "/p/alpha/s/claude-1");
  await page.getByRole("button", { name: "Branch / worktree nou" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByLabel("Branch de bază")).toHaveValue("/srv/alpha-wt");
  await dialog.getByLabel("Nume branch nou").fill("mobile-task");
  await dialog.getByRole("button", { name: "Creează", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "Branch / worktree", exact: true })).toHaveValue("/srv/alpha-worktrees/mobile-task");
  const trees = JSON.parse(readFileSync(`${STATE}/worktrees.json`, "utf8"));
  expect(trees.find((w: { branch: string }) => w.branch === "mobile-task")).toMatchObject({ created_from: "feature/homepage", is_main: false });
  await page.getByRole("button", { name: "Agent nou", exact: true }).click();
  await expect(page.getByRole("dialog").getByLabel("Checkout", { exact: true })).toHaveValue("/srv/alpha-worktrees/mobile-task");
});

test("branch mode explains why an occupied primary cannot be changed; a lost create ack is never retried", async ({ page }) => {
  await login(page, "/p/alpha");
  await page.getByRole("button", { name: "Branch / worktree nou" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("radio", { name: "Branch", exact: true }).check();
  await expect(dialog.getByText(/sesiuni deschise.*Worktree/)).toBeVisible();
  await dialog.getByLabel("Nume branch nou").fill("another-task");
  await expect(dialog.getByRole("button", { name: "Creează", exact: true })).toBeDisabled();
  await dialog.getByRole("radio", { name: "Worktree", exact: true }).check();
  let posts = 0;
  await page.route("**/api/worktrees", async route => {
    if (route.request().method() === "GET") return route.continue();
    posts++; await route.fetch(); await route.abort("connectionreset");
  });
  await dialog.getByRole("button", { name: "Creează", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("incertă");
  await expect(dialog.getByRole("button", { name: "Creează", exact: true })).toBeDisabled();
  await page.waitForTimeout(1200);
  expect(posts).toBe(1);
});
