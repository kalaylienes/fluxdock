import { test } from "@playwright/test";
import { boot, payload, provider, win } from "./harness";

/**
 * A six pixel bar hides a lot at normal scale, so edge treatments and glow
 * spill are checked on a magnified capture rather than by eye.
 */
test("magnified bar", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 4,
  });
  const page = await context.newPage();

  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(55, 128), weekly: win(30, 4000) }),
      provider("codex", { five_hour: win(72, 200), weekly: win(18, 6000) }),
    ]),
  );
  await page.waitForTimeout(1400);

  const wrap = page.locator(".fd-track-wrap").first();
  const box = await wrap.boundingBox();
  if (!box) throw new Error("bar not found");

  await page.screenshot({
    path: "tests/screenshots/zoom-bar.png",
    clip: { x: box.x - 4, y: box.y - 6, width: box.width + 8, height: box.height + 12 },
  });
  await page.screenshot({ path: "tests/screenshots/zoom-full.png" });
  await context.close();
});
