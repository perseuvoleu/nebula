import { expect } from "@playwright/test";
import { test, changeBoard, login } from "./helpers";

test("home cards keep their own scope through a reload, and a card page keeps its project", async ({ page }) => {
  changeBoard((board) => board.cards.push({ ...board.cards[1], id: "01JD0000000000000000000030", number: 30, ref: "#30@leaseweb",
    title: "A home chore", stage: "backlog", project: null, checkout: null, blocked_by: [] }));
  await login(page, "/board");
  const sidebar = page.getByRole("navigation", { name: "Proiecte" });
  await sidebar.getByRole("button", { name: /\bhome\s*1$/ }).click();
  await expect(page).toHaveURL(/\/board\/home$/);
  await page.reload();
  await expect(page.getByRole("article").filter({ hasText: "A home chore" })).toBeVisible();
  await expect(page.getByRole("article").filter({ hasText: "Keep verification results" })).toHaveCount(0);

  await page.goto("/board/p/nebula?peek=12%40leaseweb");
  const open = page.getByRole("dialog").getByRole("link", { name: "Deschide pagina" });
  await expect(open).toHaveAttribute("href", "/board/p/nebula/c/12%40leaseweb");
  await open.click();
  await expect(page).toHaveURL(/\/board\/p\/nebula\/c\/12%40leaseweb$/);
  await page.reload();
  await expect(page.getByText("/home/perseu/nebula-worktrees/agent-home-02")).toBeVisible();
  await expect(page.getByText("worktree", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Board" }).click();
  await expect(page).toHaveURL(/\/board\/p\/nebula$/);
});
