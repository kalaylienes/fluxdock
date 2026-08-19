import { expect, test } from "@playwright/test";
import { boot, payload, provider, setAppearance, win } from "./harness";

const both = () =>
  payload([
    provider("claude", { five_hour: win(55, 128), weekly: win(30, 4000) }),
    provider("codex", { five_hour: win(72, 200), weekly: win(18, 6000) }),
  ]);

test("taskbar placement lays providers out side by side", async ({ page }) => {
  await boot(page, both());
  await expect(page.locator(".fd-grip")).toHaveCount(1);

  await setAppearance(page, { placement: "taskbar" });

  await expect(page.locator(".fd-shell")).toHaveAttribute("data-placement", "taskbar");
  await expect(page.locator(".fd-column")).toHaveCount(2);
  // Both windows stay visible, two rows per provider.
  await expect(page.locator(".fd-row")).toHaveCount(4);
  // These fixtures state no window length, so this is the fallback naming, not
  // evidence that the labels are still fixed. Codex supplies its own.
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["5h", "7d", "5h", "7d"]);
  // Position is derived from the strip, so there is nothing to drag.
  await expect(page.locator(".fd-grip")).toHaveCount(0);
});

test("provider columns sit next to each other, not stacked", async ({ page }) => {
  await boot(page, both(), { placement: "taskbar" });
  await page.setViewportSize({ width: 244, height: 44 });
  await page.waitForTimeout(300);

  const boxes = await page
    .locator(".fd-column")
    .evaluateAll((els) => els.map((el) => el.getBoundingClientRect()));

  expect(boxes).toHaveLength(2);
  expect(boxes[1].x).toBeGreaterThan(boxes[0].x + boxes[0].width - 1);
  expect(Math.abs(boxes[0].y - boxes[1].y)).toBeLessThan(1);
});

test("taskbar placement fits the strip height", async ({ page }) => {
  await boot(page, both(), { placement: "taskbar" });
  await page.setViewportSize({ width: 244, height: 44 });
  await page.waitForTimeout(300);

  const fits = await page.evaluate(() => ({
    scrollH: document.documentElement.scrollHeight,
    innerH: window.innerHeight,
    scrollW: document.documentElement.scrollWidth,
    innerW: window.innerWidth,
  }));
  expect(fits.scrollH).toBeLessThanOrEqual(fits.innerH);
  expect(fits.scrollW).toBeLessThanOrEqual(fits.innerW);
});

test("a provider without data collapses to a short label", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(55, 128), weekly: win(30, 4000) }),
      provider("codex", { status: "cli_not_found", detail: "Codex CLI not found" }),
    ]),
    { placement: "taskbar" },
  );
  await page.setViewportSize({ width: 244, height: 44 });

  await expect(page.locator(".fd-column")).toHaveCount(2);
  await expect(page.locator(".fd-column-empty")).toHaveCount(1);
  await expect(page.locator(".fd-column-empty-text")).toHaveText("not installed");
});

test("taskbar screenshot", async ({ page }) => {
  await boot(page, both(), { placement: "taskbar" });
  await page.setViewportSize({ width: 244, height: 44 });
  await page.waitForTimeout(1200);
  await page.screenshot({ path: "tests/screenshots/placement-taskbar.png" });
});
