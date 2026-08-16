import { test } from "@playwright/test";
import { boot, payload, provider, setAppearance, win } from "./harness";

/**
 * Produces the images used in the README. Values are synthetic so nothing from
 * a real account ends up in the repository.
 *
 * npx playwright test tests/docs-shots.spec.ts
 */

const OUT = "docs/media";

const scene = () =>
  payload([
    provider("claude", {
      five_hour: win(62, 137, { burn_rate: 8.4 }),
      weekly: win(38, 3 * 24 * 60 + 210),
      plan_type: "max",
    }),
    provider("codex", {
      five_hour: win(23, 64),
      weekly: win(71, 5 * 24 * 60 + 60),
      plan_type: "plus",
    }),
  ]);

test("floating widget, dark", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 3,
  });
  const page = await context.newPage();
  await boot(page, scene());
  await page.waitForTimeout(1400);
  await page.screenshot({ path: `${OUT}/floating-dark.png` });
  await context.close();
});

test("floating widget, light", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 3,
  });
  const page = await context.newPage();
  await boot(page, scene(), { resolved_theme: "light" });
  await page.waitForTimeout(1400);
  await page.screenshot({ path: `${OUT}/floating-light.png` });
  await context.close();
});

test("pinned to taskbar", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 244, height: 44 },
    deviceScaleFactor: 3,
  });
  const page = await context.newPage();
  await boot(page, scene(), { placement: "taskbar" });
  await page.waitForTimeout(1400);
  await page.screenshot({ path: `${OUT}/taskbar.png` });
  await context.close();
});

test("warning and stale states", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 3,
  });
  const page = await context.newPage();
  await boot(
    page,
    payload([
      provider("claude", {
        five_hour: win(94, 21, { burn_rate: 12.1 }),
        weekly: win(76, 2 * 24 * 60),
      }),
      provider("codex", {
        status: "stale",
        five_hour: win(45, null, { stale: true }),
        weekly: win(31, 4 * 24 * 60),
      }),
    ]),
  );
  await page.waitForTimeout(1400);
  await page.screenshot({ path: `${OUT}/states.png` });
  await context.close();
});

test("estimate labelling", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 3,
  });
  const page = await context.newPage();
  await boot(
    page,
    payload([
      provider("claude", {
        five_hour: win(58, 96),
        weekly: win(41, 2 * 24 * 60, { source: "estimate" }),
      }),
      provider("codex", {
        five_hour: win(12, 180),
        weekly: win(64, 6 * 24 * 60),
      }),
    ]),
  );
  await setAppearance(page, { resolved_theme: "dark" });
  await page.waitForTimeout(1400);
  await page.screenshot({ path: `${OUT}/estimates.png` });
  await context.close();
});
