import { expect, test } from "@playwright/test";
import { boot, payload, provider, setAppearance, win } from "./harness";

/**
 * Pixel level checks that catch the kind of regression a functional assertion
 * misses: glow bleeding past the bar, a bright core collapsing on a short fill,
 * or a column overflowing the taskbar strip.
 */

test("the glow stays close to the bar", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 300, height: 88 },
    deviceScaleFactor: 4,
  });
  const page = await context.newPage();
  await boot(page, payload([provider("claude", { five_hour: win(80, 90), weekly: win(50, 4000) })]));
  await page.waitForTimeout(1400);

  const spread = await page.evaluate(() => {
    const glow = document.querySelector(".fd-glow") as HTMLElement;
    const shadow = getComputedStyle(glow).boxShadow;
    // "rgba(...) 0px 0px Npx 0px" -> the blur radius is the third length.
    const lengths = shadow.match(/(-?\d+(?:\.\d+)?)px/g) ?? [];
    return lengths.map((v) => parseFloat(v));
  });

  const blur = spread[2] ?? 0;
  const spreadRadius = spread[3] ?? 0;
  // A six pixel bar cannot carry more glow than its own height without the
  // bleed reading as a halo above and below the row.
  expect(blur + spreadRadius).toBeLessThanOrEqual(4);
  await context.close();
});

test("a short fill does not show a pinched bright spot", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(6, 200), weekly: win(70, 4000) }),
    ]),
  );
  await page.waitForTimeout(800);

  // Stops can be serialised in different colour spaces, so each one is painted
  // and compared as pixels.
  const stops = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const ctx = canvas.getContext("2d")!;
    const toRgb = (colour: string) => {
      ctx.clearRect(0, 0, 1, 1);
      ctx.fillStyle = colour;
      ctx.fillRect(0, 0, 1, 1);
      const d = ctx.getImageData(0, 0, 1, 1).data;
      return [d[0], d[1], d[2]] as [number, number, number];
    };
    const parse = (el: Element) =>
      (getComputedStyle(el).backgroundImage.match(/(?:rgba?|oklab|color)\([^)]*\)/g) ?? []).map(
        toRgb,
      );
    const fills = Array.from(document.querySelectorAll(".fd-fill"));
    return { low: parse(fills[0]), high: parse(fills[1]) };
  });

  const distance = (a: number[], b: number[]) =>
    Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]);

  expect(stops.low.length).toBeGreaterThanOrEqual(5);
  // Below the fade-in threshold the core has collapsed onto the mid tone.
  expect(distance(stops.low[1], stops.low[2])).toBeLessThanOrEqual(3);
  // Above it the core is clearly brighter than the tone either side of it.
  expect(distance(stops.high[1], stops.high[2])).toBeGreaterThan(20);
});

test("taskbar columns never overflow the strip", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", { five_hour: win(100, 12), weekly: win(100, 4000) }),
      provider("codex", { five_hour: win(100, 12), weekly: win(100, 4000) }),
    ]),
    { placement: "taskbar" },
  );
  await page.setViewportSize({ width: 244, height: 44 });
  await page.waitForTimeout(600);

  const overflow = await page.evaluate(() => {
    const shell = document.querySelector(".fd-shell") as HTMLElement;
    const rows = Array.from(document.querySelectorAll(".fd-row"));
    const box = shell.getBoundingClientRect();
    return rows.map((r) => {
      const b = r.getBoundingClientRect();
      return { overRight: b.right - box.right, overBottom: b.bottom - box.bottom };
    });
  });

  for (const o of overflow) {
    expect(o.overRight).toBeLessThanOrEqual(1);
    expect(o.overBottom).toBeLessThanOrEqual(1);
  }
});

test("every row keeps its numbers readable at the widest values", async ({ page }) => {
  await boot(
    page,
    payload([
      provider("claude", {
        five_hour: win(100, 60 * 24 * 9, { source: "estimate_local_only" }),
        weekly: win(100, 60 * 24 * 9, { source: "estimate_local_only" }),
      }),
      provider("codex", {
        five_hour: win(100, 60 * 24 * 9),
        weekly: win(100, 60 * 24 * 9),
      }),
    ]),
  );
  await page.waitForTimeout(600);

  const clipped = await page.evaluate(() =>
    Array.from(document.querySelectorAll(".fd-pct, .fd-countdown, .fd-label")).map((el) => ({
      text: (el.textContent ?? "").trim(),
      overflowing: el.scrollWidth > el.clientWidth + 1,
    })),
  );

  const bad = clipped.filter((c) => c.overflowing);
  expect(bad, `clipped cells: ${JSON.stringify(bad)}`).toHaveLength(0);
});

test("both themes keep percentage text legible against the surface", async ({ page }) => {
  await boot(page, payload([provider("claude", { five_hour: win(45, 90), weekly: win(80, 4000) })]));

  for (const theme of ["dark", "light"] as const) {
    await setAppearance(page, { resolved_theme: theme });
    await page.waitForTimeout(200);

    const contrast = await page.evaluate(() => {
      const parse = (c: string) => {
        const m = c.match(/[\d.]+/g)?.map(Number) ?? [0, 0, 0];
        return [m[0], m[1], m[2]] as [number, number, number];
      };
      const lum = ([r, g, b]: [number, number, number]) => {
        const f = (v: number) => {
          const s = v / 255;
          return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
        };
        return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
      };
      const pct = document.querySelector(".fd-pct") as HTMLElement;
      const bars = document.querySelector(".fd-bars") as HTMLElement;
      const l1 = lum(parse(getComputedStyle(pct).color));
      const l2 = lum(parse(getComputedStyle(bars).backgroundColor));
      const [hi, lo] = l1 > l2 ? [l1, l2] : [l2, l1];
      return (hi + 0.05) / (lo + 0.05);
    });

    expect(contrast, `${theme} theme contrast`).toBeGreaterThan(4.5);
  }
});

test("the widget never scrolls in any supported layout", async ({ page }) => {
  const cases: Array<{ appearance: Record<string, unknown>; w: number; h: number }> = [
    { appearance: {}, w: 300, h: 88 },
    { appearance: { compact_mode: true }, w: 300, h: 88 },
    { appearance: { show_model_weekly: true }, w: 300, h: 88 },
    { appearance: { placement: "taskbar" }, w: 244, h: 44 },
    { appearance: { placement: "taskbar" }, w: 244, h: 36 },
  ];

  for (const c of cases) {
    await boot(
      page,
      payload([
        provider("claude", {
          five_hour: win(62, 137),
          weekly: win(38, 4000),
          weekly_opus: win(50, 4000),
          weekly_sonnet: win(20, 4000),
        }),
        provider("codex", { five_hour: win(23, 64), weekly: win(71, 8000) }),
      ]),
      c.appearance,
    );
    await page.setViewportSize({ width: c.w, height: c.h });
    await page.waitForTimeout(300);

    const box = await page.evaluate(() => ({
      sh: document.documentElement.scrollHeight,
      ih: window.innerHeight,
      sw: document.documentElement.scrollWidth,
      iw: window.innerWidth,
    }));
    expect(box.sh, JSON.stringify(c)).toBeLessThanOrEqual(box.ih);
    expect(box.sw, JSON.stringify(c)).toBeLessThanOrEqual(box.iw);
  }
});
