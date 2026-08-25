import type { Page } from "@playwright/test";

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
  }> = {},
) {
  const now = Date.now();
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
