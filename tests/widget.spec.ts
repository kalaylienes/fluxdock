import { expect, test } from "@playwright/test";
import { boot, payload, provider, setAppearance, setUsage, win } from "./harness";

const healthy = () =>
  payload([
    provider("claude", {
      five_hour: win(34, 134, {
        burn_rate: 6.2,
        eta: new Date(Date.now() + 3 * 3600_000).toISOString(),
      }),
      weekly: win(12, 3 * 24 * 60),
      plan_type: "max",
      tokens: { input: 1200, output: 340, cache_write: 90, cache_read: 5400, total: 7030 },
    }),
    provider("codex", {
      five_hour: win(5, 47),
      weekly: win(13, 6 * 24 * 60 + 30),
      plan_type: "team",
    }),
  ]);

test("renders four rows with the expected values and countdown formats", async ({ page }) => {
  await boot(page, healthy());

  await expect(page.locator(".fd-row")).toHaveCount(4);

  const pcts = await page.locator(".fd-pct").allInnerTexts();
  expect(pcts.map((t) => t.trim())).toEqual(["34%", "12%", "5%", "13%"]);

  const counts = await page.locator(".fd-countdown").allInnerTexts();
  expect(counts[0]).toMatch(/^2h 1[34]m$/);
  expect(counts[1]).toMatch(/^\dd \d+h$/);
  expect(counts[2]).toMatch(/^4[5-7]m \d{2}s$/);
  expect(counts[3]).toMatch(/^\dd \d+h$/);
});

test("fill geometry matches the value and keeps its pill cap", async ({ page }) => {
  await boot(page, healthy());
  await page.waitForTimeout(1500);

  const geo = await page.evaluate(() => {
    const wraps = Array.from(document.querySelectorAll(".fd-track-wrap"));
    return wraps.map((wrap) => {
      const track = wrap.querySelector(".fd-track") as HTMLElement;
      const clip = wrap.querySelector(".fd-clip") as HTMLElement;
      const fill = wrap.querySelector(".fd-fill") as HTMLElement;
      const trackBox = track.getBoundingClientRect();
      const clipBox = clip.getBoundingClientRect();
      const fillBox = fill.getBoundingClientRect();
      return {
        trackW: trackBox.width,
        visibleW: Math.max(0, Math.min(trackBox.right, clipBox.right) - trackBox.left),
        fillAligned: Math.abs(fillBox.left - trackBox.left) < 1.5,
        clipRadius: getComputedStyle(clip).borderTopRightRadius,
        clipTransform: getComputedStyle(clip).transform,
      };
    });
  });

  const expected = [34, 12, 5, 13];
  geo.forEach((g, i) => {
    const ratio = (g.visibleW / g.trackW) * 100;
    expect(Math.abs(ratio - expected[i])).toBeLessThan(2);
    expect(g.fillAligned).toBe(true);
    // Scaling the cap would flatten the radius, so the geometry uses translation.
    expect(g.clipRadius).toBe("3px");
    expect(g.clipTransform).not.toContain("matrix(0.");
  });
});

test("colours escalate above seventy and ninety percent", async ({ page }) => {
  await boot(page, payload([provider("claude", { five_hour: win(94, 20), weekly: win(75, 1000) })]));
  await page.waitForTimeout(300);

  const classes = await page.locator(".fd-pct").evaluateAll((els) => els.map((e) => e.className));
  expect(classes[0]).toContain("fd-danger");
  expect(classes[1]).toContain("fd-warn");

  const fillBg = await page
    .locator(".fd-fill")
    .first()
    .evaluate((el) => getComputedStyle(el).backgroundImage);
  expect(fillBg).not.toContain("var(");
  expect(fillBg).toContain("gradient");
});

test("the estimate marker appears only on estimated values", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", {
        five_hour: win(20, 60, { source: "estimate" }),
        weekly: win(30, 6000, { source: "estimate_local_only" }),
      }),
    ]),
  );

  expect(await page.locator(".fd-est").allInnerTexts()).toEqual(["est.", "est*"]);

  await setUsage(
    page,
    payload([provider("claude", { five_hour: win(20, 60), weekly: win(30, 6000) })]),
  );
  await expect(page.locator(".fd-est")).toHaveCount(0);
});

test("a stale snapshot greys out and stops moving", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("codex", {
        status: "stale",
        detail: "snapshot 400 minutes old",
        five_hour: win(45, null, { stale: true }),
        weekly: win(13, 4000),
      }),
    ]),
  );
  await page.waitForTimeout(300);

  const first = page.locator(".fd-track-wrap").first();
  await expect(first.locator(".fd-sheen")).toHaveCount(0);
  expect(await first.locator(".fd-glow").evaluate((el) => getComputedStyle(el).boxShadow)).toBe(
    "none",
  );
  await expect(page.locator(".fd-countdown").first()).toHaveText("--");
});

test("with no CLI installed the onboarding card takes over", async ({ page }) => {
  await boot(
    page,
    payload(
      [
        provider("claude", { status: "cli_not_found", detail: "Claude Code not found" }),
        provider("codex", { status: "cli_not_found", detail: "Codex CLI not found" }),
        provider("antigravity", { status: "cli_not_found", detail: "Antigravity not found" }),
      ],
      true,
    ),
  );

  await expect(page.locator(".fd-onboard-title")).toBeVisible();
  await expect(page.locator(".fd-row")).toHaveCount(0);
  await expect(page.locator(".fd-link")).toHaveCount(3);
  // The card names every tool it can read, so a fourth link must not push the
  // row into a second line the window was not sized for.
  const wrapped = await page.evaluate(() => {
    const links = Array.from(document.querySelectorAll(".fd-link"));
    const tops = new Set(links.map((el) => Math.round(el.getBoundingClientRect().top)));
    return tops.size;
  });
  expect(wrapped).toBe(1);
});

test("a re-auth state shows a warning strip", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", {
        status: "reauth_needed",
        detail: "re-auth required",
        five_hour: win(40, 90, { source: "estimate" }),
        weekly: win(20, 5000, { source: "estimate" }),
      }),
    ]),
  );
  await expect(page.locator(".fd-status-strip")).toContainText("re-auth");
});

test("a missing CLI collapses only its own provider", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(10, 60), weekly: win(20, 5000) }),
      provider("codex", {
        status: "cli_not_found",
        detail: "Codex CLI not found",
        install_url: "https://developers.openai.com/codex/cli/",
      }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(2);
  await expect(page.locator(".fd-status")).toContainText("Codex CLI not found");
  await expect(page.locator(".fd-status .fd-link")).toHaveText("install");
});

test("a provider without window data shows a line instead of empty bars", async ({ page }) => {
  await boot(
    page,
    payload([provider("claude", { status: "endpoint_error", detail: "endpoint 429, backing off" })]),
  );
  await expect(page.locator(".fd-row")).toHaveCount(0);
  await expect(page.locator(".fd-status")).toContainText("429");
});

test("motion stops when animations are off or the window is occluded", async ({ page }) => {
  await boot(page, payload([provider("claude", { five_hour: win(60, 60), weekly: win(45, 5000) })]));
  await expect(page.locator(".fd-sheen").first()).toBeVisible();

  await setAppearance(page, { animations: false });
  await expect(page.locator(".fd-sheen")).toHaveCount(0);

  await setAppearance(page, { animations: true, motion_allowed: false });
  await expect(page.locator(".fd-sheen")).toHaveCount(0);

  await setAppearance(page, { motion_allowed: true });
  await expect(page.locator(".fd-sheen").first()).toBeVisible();
});

test("the operating system reduced motion setting is enough on its own", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await boot(page, payload([provider("claude", { five_hour: win(60, 60), weekly: win(45, 5000) })]));
  await expect(page.locator(".fd-sheen")).toHaveCount(0);
});

test("compact mode leaves one bar per provider", async ({ page }) => {
  await boot(page, healthy(), { compact_mode: true });
  await expect(page.locator(".fd-row")).toHaveCount(2);
  // Claude peaks on the five hour window, Codex on the weekly one.
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["5h", "7d"]);
});

test("per model weekly rows follow the setting", async ({ page }) => {
  const p = payload([
    provider("claude", {
      five_hour: win(34, 134),
      weekly: win(12, 4000),
      weekly_opus: win(48, 4000),
      weekly_sonnet: win(9, 4000),
    }),
  ]);
  await boot(page, p);
  await expect(page.locator(".fd-row")).toHaveCount(2);

  await setAppearance(page, { show_model_weekly: true });
  await expect(page.locator(".fd-row")).toHaveCount(4);
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["5h", "7d", "Op", "So"]);
});

test("the theme switches the colour tokens", async ({ page }) => {
  await boot(page, healthy());
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  const darkSurface = await page
    .locator(".fd-shell")
    .evaluate((el) => getComputedStyle(el).backgroundColor);

  await setAppearance(page, { resolved_theme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  const lightSurface = await page
    .locator(".fd-shell")
    .evaluate((el) => getComputedStyle(el).backgroundColor);

  expect(lightSurface).not.toBe(darkSurface);
});

test("content fits the window without scrolling", async ({ page }) => {
  await boot(page, healthy());
  const box = await page.evaluate(() => ({
    scrollH: document.documentElement.scrollHeight,
    innerH: window.innerHeight,
    scrollW: document.documentElement.scrollWidth,
    innerW: window.innerWidth,
  }));
  expect(box.scrollH).toBeLessThanOrEqual(box.innerH);
  expect(box.scrollW).toBeLessThanOrEqual(box.innerW);
});

test("a exhausted window fills the bar and keeps the countdown readable", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", {
        status: "limit_reached",
        five_hour: win(100, 42),
        weekly: win(88, 5000),
      }),
    ]),
  );
  await page.waitForTimeout(1200);

  const ratio = await page.evaluate(() => {
    const wrap = document.querySelector(".fd-track-wrap") as HTMLElement;
    const track = wrap.querySelector(".fd-track")!.getBoundingClientRect();
    const clip = wrap.querySelector(".fd-clip")!.getBoundingClientRect();
    return (Math.min(track.right, clip.right) - track.left) / track.width;
  });
  expect(ratio).toBeGreaterThan(0.98);
  await expect(page.locator(".fd-countdown").first()).toHaveText(/^4[01]m \d{2}s$/);
});

test("right click asks the backend for the tray menu", async ({ page }) => {
  await boot(page, healthy());
  await page.locator(".fd-shell").click({ button: "right" });
  const calls = await page.evaluate(() => (window as any).__FD.calls.map((c: any) => c.cmd));
  expect(calls).toContain("show_context_menu");
});

test("reference screenshots", async ({ page }, testInfo) => {
  await boot(page, healthy());
  await page.waitForTimeout(1200);
  await testInfo.attach("dark", { body: await page.screenshot(), contentType: "image/png" });
  await page.screenshot({ path: "tests/screenshots/widget-dark.png" });

  await setAppearance(page, { resolved_theme: "light" });
  await page.waitForTimeout(400);
  await page.screenshot({ path: "tests/screenshots/widget-light.png" });

  await setUsage(
    page,
    payload([
      provider("claude", { five_hour: win(94, 18), weekly: win(72, 4000) }),
      provider("codex", {
        status: "stale",
        five_hour: win(45, null, { stale: true }),
        weekly: win(13, 4000),
      }),
    ]),
  );
  await setAppearance(page, { resolved_theme: "dark" });
  await page.waitForTimeout(1200);
  await page.screenshot({ path: "tests/screenshots/widget-warning.png" });
});

/**
 * OpenAI took the five hour window away from Plus, Pro and Business in July
 * 2026. What arrives now is a single weekly window, and it arrives in the slot
 * the widget used to call "5h". A row is only drawn for a window the account
 * actually has, and it is named by the length the provider stated.
 */
test("a lone weekly window is one row named 7d, not a 5h row at zero", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("codex", {
        five_hour: win(37, 4 * 24 * 60, { label: "7d" }),
        plan_type: "plus",
      }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(1);
  await expect(page.locator(".fd-label")).toHaveText("7d");
  await expect(page.locator(".fd-pct")).toContainText("37%");
  await expect(page.locator(".fd-pct .fd-est")).toHaveCount(0);
});

test("an unfamiliar window length is shown as the provider described it", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("codex", {
        five_hour: win(20, 700, { label: "12h" }),
        weekly: win(60, 43_000, { label: "1M" }),
      }),
    ]),
  );

  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["12h", "1M"]);

  // The widest of these labels still has to fit its column, in both layouts.
  // The taskbar strip gives it less room than the floating window does.
  for (const placement of ["float", "taskbar"] as const) {
    await setAppearance(page, { placement });
    await expect(page.locator(".fd-column")).toHaveCount(placement === "taskbar" ? 1 : 0);
    for (const label of await page.locator(".fd-label").all()) {
      // The column does not clip, so scrollWidth would report a fit either
      // way. Measuring the text itself is the only thing that catches a label
      // spilling into the bar next to it.
      const room = await label.evaluate((el) => {
        const range = document.createRange();
        range.selectNodeContents(el);
        return el.getBoundingClientRect().width - range.getBoundingClientRect().width;
      });
      expect(room).toBeGreaterThan(1);
    }
  }
});

test("a provider that states no window length keeps the standing labels", async ({ page }) => {
  await boot(
    page,
    payload([provider("claude", { five_hour: win(34, 134), weekly: win(12, 4000) })]),
  );
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["5h", "7d"]);
});

/**
 * The mixed case, which is what a machine in this state actually renders:
 * Claude names its windows in the response so it keeps the standing labels,
 * while Codex is down to one window and says how long it is.
 */
test("a derived label and the standing ones sit side by side", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(34, 134), weekly: win(12, 4000) }),
      provider("codex", { five_hour: win(37, 4 * 24 * 60, { label: "7d" }), plan_type: "plus" }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(3);
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["5h", "7d", "7d"]);
});


/**
 * Antigravity has no five hour window at all. Both of its limits are weekly,
 * one per model family, so the rows are named after the family rather than
 * after the slot the value travelled in.
 */
test("antigravity draws one row per model family, named after the family", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("antigravity", {
        five_hour: win(25, 6 * 24 * 60, { label: "Gem" }),
        weekly: win(0, 6 * 24 * 60, { label: "3P" }),
        detail: "Gemini Models",
      }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(2);
  expect(await page.locator(".fd-label").allInnerTexts()).toEqual(["Gem", "3P"]);
  expect(await page.locator(".fd-pct").allInnerTexts()).toEqual(["25%", "0%"]);
  await expect(page.locator(".fd-pct .fd-est")).toHaveCount(0);
});

test("three providers stack without crowding either layout", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(34, 134), weekly: win(12, 4000) }),
      provider("codex", { five_hour: win(5, 47), weekly: win(13, 9000) }),
      provider("antigravity", {
        five_hour: win(84, 6 * 24 * 60, { label: "Gem" }),
        weekly: win(0, 6 * 24 * 60, { label: "3P" }),
      }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(6);
  await expect(page.locator(".fd-group")).toHaveCount(3);

  // The floating window is sized by the backend from what the page reports, so
  // what has to be checked here is that the page asks for room for all three
  // rather than that three fit in a window built for two.
  const asked = await page.evaluate(() => {
    const calls = (window as any).__FD.calls as Array<{ cmd: string; args: any }>;
    const heights = calls.filter((c) => c.cmd === "set_content_height").map((c) => c.args.height);
    return heights[heights.length - 1] ?? null;
  });
  expect(asked).toBeGreaterThan(88);

  await page.setViewportSize({ width: 300, height: asked });
  const overflow = await page.evaluate(() => ({
    x: document.documentElement.scrollWidth - window.innerWidth,
    y: document.documentElement.scrollHeight - window.innerHeight,
  }));
  expect(overflow.x).toBeLessThanOrEqual(0);
  expect(overflow.y).toBeLessThanOrEqual(0);

  // The strip is 12 + 3 x 112 + 2 x 16 logical pixels wide for three columns,
  // which is what monitor::taskbar_width_logical hands the window.
  await setAppearance(page, { placement: "taskbar" });
  await expect(page.locator(".fd-column")).toHaveCount(3);
  await page.setViewportSize({ width: 380, height: 40 });
  const stripOverflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(stripOverflow).toBeLessThanOrEqual(0);

  for (const label of await page.locator(".fd-label").all()) {
    const room = await label.evaluate((el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      return el.getBoundingClientRect().width - range.getBoundingClientRect().width;
    });
    expect(room).toBeGreaterThan(1);
  }
});

/**
 * The CLI is what serves the numbers, so closing it stops them arriving. The
 * last reading is kept and greyed rather than dropped, because a weekly figure
 * from an hour ago is still the best statement anyone has.
 */
test("antigravity with the cli closed keeps the last reading, marked stale", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("antigravity", {
        status: "stale",
        detail: "Antigravity not running, last reading 412 min ago",
        five_hour: win(84, 5 * 24 * 60, { label: "Gem", stale: true }),
        weekly: win(0, 5 * 24 * 60, { label: "3P", stale: true }),
      }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(2);
  await expect(page.locator(".fd-sheen")).toHaveCount(0);

  // A stale bar is drawn flat: both ends of the gradient are the same colour,
  // which is what tells it apart from a live one at the same percentage.
  const stops = await page.locator(".fd-fill").first().evaluate((el) => {
    const image = getComputedStyle(el).backgroundImage;
    return image.match(/rgba?\([^)]*\)/g) ?? [];
  });
  expect(stops.length).toBeGreaterThanOrEqual(2);
  expect(new Set(stops).size).toBe(1);
});

test("antigravity that was never started shows a line, not empty bars", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("antigravity", { status: "no_data_yet", detail: "Antigravity is not running" }),
    ]),
  );

  await expect(page.locator(".fd-row")).toHaveCount(0);
  await expect(page.locator(".fd-status")).toContainText("Antigravity is not running");
});
