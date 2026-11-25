import { expect, type Page } from "@playwright/test";
import { test, login } from "./helpers";

const failSessions = (page: Page, project: string) =>
  page.route(`**/api/sessions?project=${project}`, route => route.fulfill({ status: 503, json: { error: "Synthetic unavailable" } }));

test("single-project list survives a failed poll", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/p/beta");
  const row = page.getByRole("button", { name: /Beta migration/ });
  await expect(row).toBeVisible();
  await failSessions(page, "beta");
  await expect(page.getByRole("alert")).toContainText("Synthetic unavailable", { timeout: 15000 });
  await expect(row).toBeVisible();
  await expect(page.getByRole("status")).toContainText("Lista poate fi veche");
});

test("single-project list survives going offline", async ({ page, context }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/p/beta");
  const row = page.getByRole("button", { name: /Beta migration/ });
  await expect(row).toBeVisible();
  await context.setOffline(true);
  await expect(page.getByRole("status")).toContainText("Lista poate fi veche", { timeout: 15000 });
  await expect(row).toBeVisible();
});

test("All omits every failed project's rows when all polls fail", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/?view=all");
  await expect(page.getByRole("button", { name: /Beta migration/ })).toBeVisible();
  await failSessions(page, "alpha");
  await failSessions(page, "beta");
  await expect(page.getByRole("alert")).toContainText("beta", { timeout: 15000 });
  await expect(page.getByRole("button", { name: /Beta migration/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Refactor auth/ })).toHaveCount(0);
});

test("a new agent opens while its project's list poll fails", async ({ page }) => {
  await login(page, "/p/alpha");
  await expect(page.getByRole("button", { name: /Refactor auth/ })).toBeVisible();
  await failSessions(page, "alpha");
  await page.getByRole("button", { name: "Agent nou" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Prima sarcină").fill("Scrie un README nou");
  await dialog.getByRole("button", { name: "Pornește" }).click();
  await expect(page).toHaveURL(/\/p\/alpha\/s\/new-[0-9a-f]{6}$/);
  await expect(page.getByRole("log").getByText("Scrie un README nou")).toBeVisible();
});

test("serverless back returns to the originating list", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const project = page.getByRole("combobox", { name: "Proiect", exact: true });
  const roundTrip = async () => {
    await page.getByRole("button", { name: "Rulări serverless" }).click();
    await expect(page).toHaveURL(/\/serverless$/);
    await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  };
  await login(page, "/p/beta");
  await roundTrip();
  await expect(page).toHaveURL(/\/p\/beta$/);
  await expect(project).toHaveValue("beta");
  await project.selectOption("");
  await expect(page).toHaveURL(/\/\?view=all$/);
  await roundTrip();
  await expect(page).toHaveURL(/\/\?view=all$/);
  await expect(project).toHaveValue("");
  await page.goto("/serverless");
  await page.getByRole("button", { name: "Înapoi la sesiuni" }).click();
  await expect(page).toHaveURL(/\/p\/alpha$/);
  await expect(project).toHaveValue("alpha");
});
