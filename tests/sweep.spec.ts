import { expect, test } from "@playwright/test";
import { boot, payload, provider, sampleSweep, setUsage, win } from "./harness";

// One sweep plus one pause is a little over five seconds, so the sampling
// window has to cover a full cycle to be meaningful.
const FULL_CYCLE_MS = 7000;

test("the highlight travels the whole fill", async ({ page }) => {
  await boot(page, payload([provider("claude", { five_hour: win(60, 128), weekly: win(20, 4000) })]));
  await page.waitForTimeout(600);

  const samples = await sampleSweep(page, FULL_CYCLE_MS);
  const min = Math.min(...samples);
  const max = Math.max(...samples);

  expect(samples.length).toBeGreaterThan(40);
  expect(min, `sweep does not start at the left edge (min ${min.toFixed(2)})`).toBeLessThan(0.05);
  expect(max, `sweep does not reach the right edge (max ${max.toFixed(2)})`).toBeGreaterThan(0.95);
});

test("frequent data updates do not interrupt the sweep", async ({ page }) => {
  // The watcher pushes a payload every few seconds and interpolation nudges the
  // percentage by fractions, which used to re-target the animation mid-flight.
  await boot(page, payload([provider("claude", { five_hour: win(60, 128), weekly: win(20, 4000) })]));
  await page.waitForTimeout(600);

  let value = 60;
  const ticker = setInterval(() => {
    value += 0.2;
    setUsage(
      page,
      payload([provider("claude", { five_hour: win(value, 128), weekly: win(20, 4000) })]),
    ).catch(() => {});
  }, 700);

  const samples = await sampleSweep(page, FULL_CYCLE_MS);
  clearInterval(ticker);

  const min = Math.min(...samples);
  const max = Math.max(...samples);
  expect(min, `left edge lost during updates (min ${min.toFixed(2)})`).toBeLessThan(0.05);
  expect(max, `right edge lost during updates (max ${max.toFixed(2)})`).toBeGreaterThan(0.95);
});
