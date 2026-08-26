import { test, type Browser } from "@playwright/test";

import {
  boot,
  expectTransparentShot,
  opaqueSurface,
  payload,
  provider,
  requestedHeight,
  settle,
  win,
} from "./harness";

/**
 * Produces the images used in the README. Every value here is invented: no
 * number, reset time or plan from a real account may enter the repository.
 *
 * npm run docs:shots
 */

const OUT = "docs/media";
const DSF = 3;

/** A fixed instant, so a countdown renders the same string on every run. */
const BASE = Date.UTC(2026, 5, 1, 9, 30, 0);

/** Logical width of the floating window, from monitor.rs LOGICAL_W. */
const FLOAT_W = 300;

/** Corner radii from styles.css, in logical pixels. */
const FLOAT_RADIUS = 10;
const STRIP_RADIUS = 6;

/**
 * The pinned strip, from monitor.rs: 12 + 3 x 112 + 2 x 16 for three columns,
 * and the height the strip leaves once taskbar_position has taken its margin.
 */
const STRIP = { w: 380, h: 44 };

/**
 * Animation off. The sheen sweeps for 1.6 s and rests for 3.5 s, so a
 * screenshot taken at an arbitrary moment caught it in a different place every
 * time and every regeneration produced a diff.
 */
const STILL = { animations: false, motion_allowed: false, resolved_theme: "dark" } as const;

const w = (utilization: number, minutes: number | null, extra = {}) =>
  win(utilization, minutes, { now: BASE, ...extra });

const scene = () =>
  payload([
    provider("claude", {
      five_hour: w(62, 137, { burn_rate: 8.4 }),
      weekly: w(38, 3 * 24 * 60 + 210),
      plan_type: "max",
    }),
    provider("codex", {
      five_hour: w(23, 64),
      weekly: w(71, 5 * 24 * 60 + 60),
      plan_type: "plus",
    }),
    // Antigravity shares one weekly limit per model family rather than per
    // tool, so its rows are named after the family and the label rides on the
    // window, which is how providers/antigravity.rs sends it.
    provider("antigravity", {
      five_hour: w(44, 3 * 24 * 60 + 540, { label: "Gem" }),
      weekly: w(67, 6 * 24 * 60 + 120, { label: "3P" }),
      plan_type: "pro",
    }),
  ]);

const states = () =>
  payload([
    provider("claude", {
      five_hour: w(94, 95, { burn_rate: 12.1 }),
      weekly: w(76, 2 * 24 * 60 + 330, { source: "estimate" }),
      plan_type: "max",
    }),
    provider("codex", {
      five_hour: w(23, 64),
      weekly: w(58, 5 * 24 * 60 + 60),
      plan_type: "plus",
    }),
    provider("antigravity", {
      status: "stale",
      five_hour: w(44, null, { label: "Gem", stale: true }),
      weekly: w(67, null, { label: "3P", stale: true }),
    }),
  ]);

/**
 * The floating window is sized by the backend from the height the interface
 * asks for, so the image is too. Repeating the arithmetic here is how an image
 * ends up sized for a layout the app no longer has.
 */
async function floating(browser: Browser, file: string, build: () => unknown) {
  const context = await browser.newContext({
    viewport: { width: FLOAT_W, height: 400 },
    deviceScaleFactor: DSF,
  });
  const page = await context.newPage();
  await page.clock.install({ time: BASE });
  await boot(page, build(), STILL);
  await settle(page);

  const height = await requestedHeight(page);
  test.expect(height, "the interface never reported a height").toBeGreaterThan(0);
  await page.setViewportSize({ width: FLOAT_W, height });

  await opaqueSurface(page);
  await settle(page);
  // omitBackground is the whole point. The page is already transparent, and
  // without this Chromium paints its own white base under the rounded corners,
  // which is the white that bled into the README on GitHub.
  await page.screenshot({ path: `${OUT}/${file}`, omitBackground: true });
  await expectTransparentShot(page, `${OUT}/${file}`, {
    width: FLOAT_W * DSF,
    height: height * DSF,
    radius: FLOAT_RADIUS * DSF,
  });
  await context.close();
}

test("floating widget", async ({ browser }) => {
  await floating(browser, "floating-dark.png", scene);
});

test("what a row says besides the number", async ({ browser }) => {
  await floating(browser, "states.png", states);
});

test("pinned to taskbar", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: STRIP.w, height: STRIP.h },
    deviceScaleFactor: DSF,
  });
  const page = await context.newPage();
  await page.clock.install({ time: BASE });
  await boot(page, scene(), { ...STILL, placement: "taskbar" });
  await opaqueSurface(page);
  await settle(page);
  await page.screenshot({ path: `${OUT}/taskbar.png`, omitBackground: true });
  await expectTransparentShot(page, `${OUT}/taskbar.png`, {
    width: STRIP.w * DSF,
    height: STRIP.h * DSF,
    radius: STRIP_RADIUS * DSF,
  });
  await context.close();
});
