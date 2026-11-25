import { expect } from "@playwright/test";
import { test, login } from "./helpers";

test("single-project list survives a failed poll", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, "/p/beta");
  const row = page.getByRole("button", { name: /Beta migration/ });
  await expect(row).toBeVisible();
  await page.route("**/api/sessions?project=beta", route => route.fulfill({ status: 503, json: { error: "Synthetic unavailable" } }));
  await expect(page.getByRole("alert")).toContainText("Synthetic unavailable", { timeout: 15000 });
  await expect(row).toBeVisible();
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
