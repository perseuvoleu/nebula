import fs from "node:fs";
import path from "node:path";
import { expect } from "@playwright/test";
import { test, login, noHorizontalOverflow, shot, STATE } from "./helpers";

// Claude's screen includes its message marker, two-space continuation indentation,
// terminal-width line breaks and chrome; it is not a structured chat message.
const snapshot = `● ❓ **Q10** - Cum se numără cele 48h? Exemplu: lead distribuit vineri
  la ora 17.

  - **A.** Ore calendaristice: expiră duminică la 17.
  - **B.** Doar zile lucrătoare: weekendul nu se numără, expiră marți
    la 17.

  ➡️ Recomand **B**: altfel lead-urile date vinerea se retrag în weekend.

  Status: \`termen_contactare_lead_frontoffice_48_ore_lucrătoare\`.

────────────────────────────────────────────────────────────────────────────────
❯
`;

test("Claude snapshot is readable Markdown at 375px with an exact raw view", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  fs.writeFileSync(path.join(STATE, "output.txt"), snapshot);
  await login(page, "/p/alpha/s/claude-1");
  const output = page.getByLabel("Output de terminal", { exact: true });
  await expect(output.getByText("Q10", { exact: true })).toBeVisible();
  await expect(output.locator("strong", { hasText: "Q10" })).toBeVisible();
  const items = output.getByRole("listitem");
  await expect(items).toHaveCount(2);
  await expect(items.nth(1)).toContainText("expiră marți la 17.");
  await expect(output.locator("code")).toHaveText("termen_contactare_lead_frontoffice_48_ore_lucrătoare");
  const paragraph = output.locator("p").filter({ hasText: "➡️ Recomand" });
  await expect(paragraph).toBeVisible();
  const listBox = await output.getByRole("list").boundingBox();
  const paragraphBox = await paragraph.boundingBox();
  expect(paragraphBox!.y - (listBox!.y + listBox!.height)).toBeGreaterThanOrEqual(8);
  const first = await items.nth(0).boundingBox();
  const second = await items.nth(1).boundingBox();
  expect(second!.y - (first!.y + first!.height)).toBeGreaterThanOrEqual(3);
  await noHorizontalOverflow(page);
  // Check the actual reading surface too: root overflow:hidden can hide clipping.
  const overflow = await output.evaluate(el => [...el.querySelectorAll("*")]
    .filter(node => node.clientWidth > 0 && node.scrollWidth > node.clientWidth + 1)
    .map(node => node.tagName));
  expect(overflow).toEqual([]);
  await shot(page, "phone-375-claude-reading");
  await page.getByRole("button", { name: "Terminal brut", exact: true }).click();
  await expect(output.locator("pre")).toBeVisible();
  expect(await output.locator("pre").textContent()).toBe(snapshot);
  await page.getByRole("button", { name: "Lectură", exact: true }).click();
  await expect(output.locator("strong", { hasText: "Q10" })).toBeVisible();
});

test("switching terminal views keeps following the latest output", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const history = `${snapshot}\n`.repeat(20) + "\nUltimul răspuns.\n";
  fs.writeFileSync(path.join(STATE, "output.txt"), history);
  await login(page, "/p/alpha/s/claude-1");
  const content = page.getByLabel("Conținut terminal", { exact: true });
  await expect(content).toContainText("Ultimul răspuns.");
  await page.getByRole("button", { name: "Terminal brut", exact: true }).click();
  await content.evaluate(el => { el.scrollTop = el.scrollHeight; });
  await page.getByRole("button", { name: "Lectură", exact: true }).click();
  await expect(content.getByText("Ultimul răspuns.", { exact: true })).toBeInViewport();
  fs.appendFileSync(path.join(STATE, "output.txt"), "\nRăspuns nou după schimbarea afișării.\n");
  await expect(content.getByText("Răspuns nou după schimbarea afișării.", { exact: true })).toBeInViewport();
});
