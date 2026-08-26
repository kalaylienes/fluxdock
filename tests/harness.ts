import { readFileSync } from "node:fs";

import { expect, type Page } from "@playwright/test";

/**
 * The widget talks to the backend only through window.__TAURI_INTERNALS__, so
 * stubbing that one object is enough to drive the real UI in a browser.
 */
export async function installTauriMock(page: Page) {
  await page.addInitScript(() => {
    const callbacks: Record<number, (arg: unknown) => void> = {};
    const listeners: Record<string, number> = {};
    let counter = 0;

    (window as any).__FD = {
      usage: null as unknown,
      appearance: {
        theme: "system",
        animations: true,
        compact_mode: false,
        show_model_weekly: false,
        placement: "float",
        motion_allowed: true,
        resolved_theme: "dark",
      },
      calls: [] as Array<{ cmd: string; args: unknown }>,
      emit(event: string, payload: unknown) {
        const id = listeners[event];
        if (id && callbacks[id]) callbacks[id]({ event, id, payload });
      },
    };

    (window as any).__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "widget" },
        currentWebview: { windowLabel: "widget", label: "widget" },
      },
      transformCallback(cb: (arg: unknown) => void) {
        const id = ++counter;
        callbacks[id] = cb;
        return id;
      },
      invoke(cmd: string, args: any) {
        (window as any).__FD.calls.push({ cmd, args });
        switch (cmd) {
          case "plugin:event|listen":
            listeners[args.event] = args.handler;
            return Promise.resolve(counter);
          case "plugin:event|unlisten":
            return Promise.resolve();
          case "get_usage":
            return Promise.resolve((window as any).__FD.usage);
          case "get_appearance":
            return Promise.resolve((window as any).__FD.appearance);
          default:
            return Promise.resolve(null);
        }
      },
    };
  });
}

export type Src = "official" | "estimate" | "estimate_local_only";

export function win(
  utilization: number,
  minutesToReset: number | null,
  extra: Partial<{
    source: Src;
    stale: boolean;
    eta: string | null;
    burn_rate: number | null;
    as_of: string;
    label: string | null;
    /** Fixed instant to measure from, for images that must not drift. */
    now: number;
  }> = {},
) {
  const now = extra.now ?? Date.now();
  return {
    utilization,
    label: extra.label ?? null,
    resets_at: minutesToReset === null ? null : new Date(now + minutesToReset * 60_000).toISOString(),
    source: extra.source ?? "official",
    as_of: extra.as_of ?? new Date(now - 60_000).toISOString(),
    stale: extra.stale ?? false,
    eta: extra.eta ?? null,
    burn_rate: extra.burn_rate ?? null,
  };
}

const LABELS = {
  claude: "Claude Code",
  codex: "Codex CLI",
  antigravity: "Antigravity",
} as const;

export function provider(
  id: "claude" | "codex" | "antigravity",
  fields: Record<string, unknown> = {},
) {
  return {
    id,
    label: LABELS[id],
    enabled: true,
    status: "ok",
    detail: null,
    install_url: null,
    five_hour: null,
    weekly: null,
    weekly_opus: null,
    weekly_sonnet: null,
    plan_type: null,
    extra_usage: null,
    tokens: null,
    ...fields,
  };
}

export function payload(providers: unknown[], onboarding = false) {
  return { providers, generated_at: new Date().toISOString(), onboarding };
}

export async function setUsage(page: Page, value: unknown) {
  await page.evaluate((v) => {
    (window as any).__FD.usage = v;
    (window as any).__FD.emit("usage", v);
  }, value);
}

export async function setAppearance(page: Page, value: Record<string, unknown>) {
  await page.evaluate((v) => {
    const fd = (window as any).__FD;
    fd.appearance = { ...fd.appearance, ...v };
    fd.emit("config", fd.appearance);
  }, value);
}

export async function boot(
  page: Page,
  initial: unknown,
  appearance: Record<string, unknown> = {},
) {
  await installTauriMock(page);
  await page.addInitScript(
    ([usage, app]) => {
      const apply = () => {
        const fd = (window as any).__FD;
        if (!fd) return false;
        fd.usage = usage;
        fd.appearance = { ...fd.appearance, ...(app as object) };
        return true;
      };
      if (!apply()) queueMicrotask(apply);
    },
    [initial, appearance] as const,
  );
  await page.goto("/");
  await page.waitForSelector(".fd-shell");
}

/**
 * Position of the sweep highlight inside the visible fill, where 0 is the left
 * edge of the fill and 1 is the right edge.
 */
export async function sampleSweep(page: Page, ms: number, stepMs = 60) {
  const samples: number[] = [];
  const steps = Math.ceil(ms / stepMs);
  for (let i = 0; i < steps; i++) {
    const s = await page.evaluate(() => {
      const wrap = document.querySelector(".fd-track-wrap");
      const sheen = wrap?.querySelector(".fd-sheen") as HTMLElement | null;
      const track = wrap?.querySelector(".fd-track") as HTMLElement | null;
      const clip = wrap?.querySelector(".fd-clip") as HTMLElement | null;
      if (!sheen || !track || !clip) return null;
      const t = track.getBoundingClientRect();
      const c = clip.getBoundingClientRect();
      const sh = sheen.getBoundingClientRect();
      const fillWidth = Math.min(c.right, t.right) - t.left;
      if (fillWidth <= 0) return null;
      return (sh.left + sh.width / 2 - t.left) / fillWidth;
    });
    if (s !== null) samples.push(s);
    await page.waitForTimeout(stepMs);
  }
  return samples;
}

/**
 * Flattens the widget's own translucency.
 *
 * `--surface` carries an alpha of about 0.92 and is painted twice, once on the
 * shell and again on the bars, so the grip rail and the bar area settle at
 * different opacities. On screen that is invisible, because the desktop behind
 * fills both in. In a screenshot with no background it is a visible seam, and
 * whatever is behind the image on the page shows through the whole widget.
 *
 * The shipped widget really is translucent; these images say so in words
 * instead of half showing it.
 */
export async function opaqueSurface(page: Page) {
  await page.addStyleTag({
    content: `
      html[data-theme="dark"], :root { --surface: #1b1b1b; }
      html[data-theme="light"] { --surface: #f7f7f5; }
    `,
  });
}

/** Waits for the thing being photographed to have finished arriving. */
export async function settle(page: Page) {
  await page.waitForSelector(".fd-track");
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  });
}

/**
 * The height the interface asks the backend for, which is what the floating
 * window is sized to. Taking it from the page rather than repeating the
 * arithmetic means an image can never be sized for a layout the app no longer
 * has.
 */
export async function requestedHeight(page: Page): Promise<number> {
  return page.evaluate(() => {
    const calls = (window as any).__FD.calls as Array<{ cmd: string; args: any }>;
    const heights = calls.filter((c) => c.cmd === "set_content_height").map((c) => c.args.height);
    return heights[heights.length - 1] ?? 0;
  });
}

export interface ShotExpectation {
  width: number;
  height: number;
  /** Corner radius in device pixels. */
  radius: number;
}

/**
 * Asserts a written PNG is what the README needs: an alpha channel, corners
 * that are actually empty, and no pale fringe where the rounded edge was
 * composited against something.
 *
 * Decoding happens in the browser that just took the shot, so there is no image
 * library to add and no second decoder to disagree with the first.
 */
export async function expectTransparentShot(page: Page, file: string, want: ShotExpectation) {
  const bytes = readFileSync(file);

  // The header is read directly, because the colour type is the one fact a
  // canvas cannot report: it hands back RGBA whatever the file said.
  expect(bytes.subarray(0, 8).toString("hex"), `${file} is not a PNG`).toBe("89504e470d0a1a0a");
  expect(bytes.readUInt32BE(16), `${file} width`).toBe(want.width);
  expect(bytes.readUInt32BE(20), `${file} height`).toBe(want.height);
  expect(bytes[24], `${file} bit depth`).toBe(8);
  expect(bytes[25], `${file} colour type, 6 is RGBA and 2 is RGB`).toBe(6);

  const stats = await page.evaluate(
    async ([b64, radius]) => {
      const img = new Image();
      img.src = `data:image/png;base64,${b64}`;
      await img.decode();

      const canvas = document.createElement("canvas");
      canvas.width = img.width;
      canvas.height = img.height;
      const ctx = canvas.getContext("2d")!;
      ctx.drawImage(img, 0, 0);
      const { data, width, height } = ctx.getImageData(0, 0, img.width, img.height);

      const at = (x: number, y: number) => {
        const i = (y * width + x) * 4;
        return { r: data[i], g: data[i + 1], b: data[i + 2], a: data[i + 3] };
      };

      // The largest square that fits outside a quarter circle of this radius,
      // one pixel in from the edge of it.
      const corner = Math.max(1, Math.floor(radius * (1 - Math.SQRT1_2)) - 1);
      let opaqueCorners = 0;
      let whiteFringe = 0;
      for (const [ox, oy] of [
        [0, 0],
        [width - radius, 0],
        [0, height - radius],
        [width - radius, height - radius],
      ]) {
        for (let y = 0; y < radius; y++) {
          for (let x = 0; x < radius; x++) {
            const p = at(ox + x, oy + y);
            const outside =
              (ox === 0 ? x < corner : x >= radius - corner) &&
              (oy === 0 ? y < corner : y >= radius - corner);
            if (outside && p.a !== 0) opaqueCorners++;
            // A pale pixel anywhere in the corner box is the halo left behind
            // when a rounded edge was composited over white.
            if (p.a > 0 && p.r >= 252 && p.g >= 252 && p.b >= 252) whiteFringe++;
          }
        }
      }

      let partial = 0;
      for (let i = 3; i < data.length; i += 4) {
        if (data[i] > 0 && data[i] < 255) partial++;
      }

      return {
        opaqueCorners,
        whiteFringe,
        partialShare: partial / (width * height),
        centre: at(Math.floor(width / 2), Math.floor(height / 2)).a,
        leftEdge: at(1, Math.floor(height / 2)).a,
        topEdge: at(Math.floor(width / 2), 1).a,
      };
    },
    [bytes.toString("base64"), want.radius] as const,
  );

  expect(stats.opaqueCorners, `${file} has painted corners`).toBe(0);
  expect(stats.whiteFringe, `${file} has a white fringe on its rounded edge`).toBe(0);
  expect(stats.centre, `${file} centre is not opaque, so the page did not render`).toBe(255);
  expect(stats.leftEdge, `${file} left edge`).toBe(255);
  expect(stats.topEdge, `${file} top edge`).toBe(255);
  expect(stats.partialShare, `${file} is mostly edge, which means it is blank`).toBeLessThan(0.01);
}
