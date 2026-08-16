import { expect, test } from "@playwright/test";
import { boot, payload, provider, win } from "./harness";

const realistic = () =>
  payload([
    provider("claude", { five_hour: win(62, 128), weekly: win(24, 4000) }),
    provider("codex", { five_hour: win(8, 200), weekly: win(41, 6000) }),
  ]);

test("the gradient is scaled to the fill, not the track", async ({ page }) => {
  await boot(page, realistic());
  await page.waitForTimeout(1200);

  const sizes = await page
    .locator(".fd-fill")
    .evaluateAll((els) => els.map((el) => getComputedStyle(el).backgroundSize));

  // Anchored to the track this would read "100% 100%" on every row and the
  // bright core would never appear on a short bar.
  expect(sizes).toEqual(["62% 100%", "24% 100%", "8% 100%", "41% 100%"]);
});

test("the bright core fades in with the fill", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(8, 200), weekly: win(70, 4000) }),
    ]),
  );
  await page.waitForTimeout(600);

  const [low, high] = await page
    .locator(".fd-fill")
    .evaluateAll((els) => els.map((el) => getComputedStyle(el).backgroundImage));

  // A short bar collapses to a solid ramp instead of squeezing five stops into
  // a few pixels, which used to render as a single bright dot.
  expect(low).not.toEqual(high);
  expect(high).toContain("gradient");
});

test("the sweep stays off until the fill can carry it", async ({ page }) => {
  await boot(page, payload([provider("claude", { five_hour: win(12, 200), weekly: win(65, 4000) })]));
  await page.waitForTimeout(400);

  const wraps = page.locator(".fd-track-wrap");
  await expect(wraps.nth(0).locator(".fd-sheen")).toHaveCount(0);
  await expect(wraps.nth(1).locator(".fd-sheen")).toHaveCount(1);
});

test("sweep frames", async ({ page }) => {
  await boot(page, realistic());
  await page.waitForTimeout(400);
  for (let i = 0; i < 8; i++) {
    await page.screenshot({ path: `tests/screenshots/sheen-${String(i).padStart(2, "0")}.png` });
    await page.waitForTimeout(150);
  }
});
