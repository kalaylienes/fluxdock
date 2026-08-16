import * as React from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";

import FluxBar from "@/components/FluxBar";
import { providerMark } from "@/components/Marks";
import type {
  AppearanceConfig,
  ProviderSnapshot,
  UsagePayload,
  WindowSnapshot,
} from "@/lib/types";
import { formatClock, formatCountdown, minutesSince, tickInterval } from "@/lib/utils";

const DEFAULT_APPEARANCE: AppearanceConfig = {
  theme: "system",
  animations: true,
  compact_mode: false,
  show_model_weekly: false,
  placement: "float",
  motion_allowed: true,
  resolved_theme: "dark",
};

export default function App() {
  const [payload, setPayload] = React.useState<UsagePayload | null>(null);
  const [appearance, setAppearance] = React.useState<AppearanceConfig>(DEFAULT_APPEARANCE);
  const [now, setNow] = React.useState(() => Date.now());
  const [osReduced, setOsReduced] = React.useState(false);

  React.useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const apply = () => setOsReduced(mq.matches);
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, []);

  React.useEffect(() => {
    const unlisteners: Array<() => void> = [];

    listen<UsagePayload>("usage", (e) => setPayload(e.payload)).then((un) => unlisteners.push(un));
    listen<AppearanceConfig>("config", (e) => setAppearance(e.payload)).then((un) =>
      unlisteners.push(un),
    );

    invoke<UsagePayload>("get_usage").then(setPayload).catch(() => {});
    invoke<AppearanceConfig>("get_appearance").then(setAppearance).catch(() => {});

    return () => unlisteners.forEach((un) => un());
  }, []);

  React.useEffect(() => {
    document.documentElement.dataset.theme = appearance.resolved_theme;
  }, [appearance.resolved_theme]);

  const providers = React.useMemo(
    () => (payload?.providers ?? []).filter((p) => p.enabled),
    [payload],
  );

  const resetTimes = React.useMemo(() => {
    const out: number[] = [];
    for (const p of providers) {
      for (const w of [p.five_hour, p.weekly, p.weekly_opus, p.weekly_sonnet]) {
        if (w?.resets_at) {
          const t = new Date(w.resets_at).getTime();
          if (!Number.isNaN(t)) out.push(t);
        }
      }
    }
    return out;
  }, [providers]);

  // Self rescheduling so the cadence tightens the moment a countdown drops
  // under an hour, without waiting for the next payload.
  React.useEffect(() => {
    let id = 0;
    const schedule = () => {
      const period = tickInterval(resetTimes, Date.now());
      id = window.setTimeout(() => {
        setNow(Date.now());
        schedule();
      }, period);
    };
    schedule();
    return () => window.clearTimeout(id);
  }, [resetTimes]);

  const motionOn = appearance.animations && !osReduced && appearance.motion_allowed;

  React.useEffect(() => {
    const onContext = (e: MouseEvent) => {
      e.preventDefault();
      invoke("show_context_menu").catch(() => {});
    };
    window.addEventListener("contextmenu", onContext);
    return () => window.removeEventListener("contextmenu", onContext);
  }, []);

  const startDrag = React.useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return;
    getCurrentWindow()
      .startDragging()
      .catch(() => {});
  }, []);

  const pinned = appearance.placement === "taskbar";

  return (
    <div className="fd-shell" data-placement={appearance.placement}>
      {!pinned && (
        <div className="fd-grip" onMouseDown={startDrag} title="Drag">
          <div className="fd-grip-dots" />
        </div>
      )}
      <div className="fd-bars">
        {payload?.onboarding ? (
          <Onboarding pinned={pinned} />
        ) : pinned ? (
          <div className="fd-columns">
            {providers.map((p) => (
              <ProviderColumn key={p.id} provider={p} now={now} motionOn={motionOn} />
            ))}
          </div>
        ) : (
          providers.map((p) => (
            <ProviderGroup
              key={p.id}
              provider={p}
              now={now}
              motionOn={motionOn}
              compact={appearance.compact_mode}
              showModelWeekly={appearance.show_model_weekly}
            />
          ))
        )}
        {!payload && <div className="fd-status">Loading</div>}
      </div>
    </div>
  );
}

function Onboarding({ pinned }: { pinned: boolean }) {
  if (pinned) {
    return (
      <div className="fd-status">
        <span>No supported CLI found</span>
      </div>
    );
  }
  return (
    <div className="fd-onboard">
      <div className="fd-onboard-title">No supported CLI found</div>
      <div>FluxDock tracks Claude Code and Codex CLI usage. Install one and it is picked up.</div>
      <div style={{ display: "flex", gap: 10 }}>
        <span className="fd-link" onClick={() => openUrl("https://claude.com/claude-code")}>
          Claude Code
        </span>
        <span
          className="fd-link"
          onClick={() => openUrl("https://developers.openai.com/codex/cli/")}
        >
          Codex CLI
        </span>
      </div>
    </div>
  );
}

interface WindowRow {
  key: string;
  label: string;
  window: WindowSnapshot | null;
}

function windowRows(provider: ProviderSnapshot, showModelWeekly: boolean): WindowRow[] {
  const rows: WindowRow[] = [
    { key: "5h", label: "5h", window: provider.five_hour },
    { key: "7d", label: "7d", window: provider.weekly },
  ];
  if (showModelWeekly) {
    if (provider.weekly_opus) rows.push({ key: "opus", label: "Op", window: provider.weekly_opus });
    if (provider.weekly_sonnet)
      rows.push({ key: "sonnet", label: "So", window: provider.weekly_sonnet });
  }
  return rows;
}

function hasWindows(provider: ProviderSnapshot): boolean {
  return Boolean(provider.five_hour || provider.weekly);
}

interface GroupProps {
  provider: ProviderSnapshot;
  now: number;
  motionOn: boolean;
  compact: boolean;
  showModelWeekly: boolean;
}

function ProviderGroup({ provider, now, motionOn, compact, showModelWeekly }: GroupProps) {
  const mark = providerMark(provider.id);

  if (
    provider.status === "cli_not_found" ||
    provider.status === "plan_unsupported" ||
    !hasWindows(provider)
  ) {
    return (
      <div className="fd-group">
        <div className="fd-status">
          <span className="fd-logo">{mark}</span>
          <span>{provider.detail ?? `${provider.label}: no data`}</span>
          {provider.install_url && (
            <span className="fd-link" onClick={() => openUrl(provider.install_url!)}>
              install
            </span>
          )}
        </div>
      </div>
    );
  }

  let rows = windowRows(provider, showModelWeekly);
  if (compact) {
    const five = provider.five_hour;
    const week = provider.weekly;
    rows = [
      (five?.utilization ?? -1) >= (week?.utilization ?? -1)
        ? { key: "5h", label: "5h", window: five }
        : { key: "7d", label: "7d", window: week },
    ];
  }

  return (
    <div className="fd-group">
      {rows.map((r, i) => (
        <Row
          key={r.key}
          mark={i === 0 ? mark : null}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          motionOn={motionOn}
          showCountdown
        />
      ))}
      {provider.status === "reauth_needed" && (
        <div className="fd-status">
          <span className="fd-status-strip">re-auth required, run claude</span>
        </div>
      )}
    </div>
  );
}

/**
 * Taskbar layout. Each provider becomes a narrow column with both windows
 * stacked, and the columns sit side by side so the strip stays short.
 */
function ProviderColumn({
  provider,
  now,
  motionOn,
}: {
  provider: ProviderSnapshot;
  now: number;
  motionOn: boolean;
}) {
  const mark = providerMark(provider.id);

  if (!hasWindows(provider)) {
    return (
      <div className="fd-column fd-column-empty">
        <span className="fd-logo">{mark}</span>
        <span className="fd-column-empty-text">{shortStatus(provider)}</span>
      </div>
    );
  }

  return (
    <div className="fd-column">
      {windowRows(provider, false).map((r, i) => (
        <Row
          key={r.key}
          mark={i === 0 ? mark : null}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          motionOn={motionOn}
          showCountdown={false}
        />
      ))}
    </div>
  );
}

function shortStatus(provider: ProviderSnapshot): string {
  switch (provider.status) {
    case "cli_not_found":
      return "not installed";
    case "plan_unsupported":
      return "plan not supported";
    case "reauth_needed":
      return "re-auth";
    case "no_data_yet":
      return "no usage yet";
    default:
      return "no data";
  }
}

interface RowProps {
  mark: React.ReactNode;
  label: string;
  window: WindowSnapshot | null;
  provider: ProviderSnapshot;
  now: number;
  motionOn: boolean;
  showCountdown: boolean;
}

function Row({ mark, label, window: w, provider, now, motionOn, showCountdown }: RowProps) {
  const value = w?.utilization ?? 0;
  const stale = w?.stale ?? false;
  const est = w ? w.source !== "official" : true;
  const localOnly = w?.source === "estimate_local_only";

  const resetMs = w?.resets_at ? new Date(w.resets_at).getTime() - now : null;
  const countdown =
    resetMs === null ? "--" : resetMs <= 0 ? "resetting" : formatCountdown(resetMs);

  const pctClass = value >= 90 ? "fd-danger" : value >= 70 ? "fd-warn" : undefined;

  return (
    <div className="fd-row" title={tooltip(provider, label, w, now)}>
      <span className="fd-logo">{mark}</span>
      <span className="fd-label">{label}</span>
      <FluxBar value={value} palette={provider.id} motion={motionOn} stale={stale} />
      <span className={`fd-pct fd-tabular ${pctClass ?? ""}`}>
        {Math.round(value)}%{est && <span className="fd-est">{localOnly ? "est*" : "est."}</span>}
      </span>
      {showCountdown && <span className="fd-countdown fd-tabular">{countdown}</span>}
    </div>
  );
}

function tooltip(
  p: ProviderSnapshot,
  label: string,
  w: WindowSnapshot | null,
  now: number,
): string {
  const windowName = label === "5h" ? "five hour" : label === "7d" ? "weekly" : label;
  const lines: string[] = [`${p.label}, ${windowName} window`];

  if (!w) {
    lines.push(p.detail ?? "no data");
    return lines.join("\n");
  }

  lines.push(
    `Source: ${
      w.source === "official"
        ? "official"
        : w.source === "estimate_local_only"
          ? "estimate, this machine only"
          : "estimate"
    }`,
  );
  lines.push(`Measured: ${formatClock(w.as_of)} (${minutesSince(w.as_of, now)} min ago)`);
  if (w.stale) lines.push("Stale, no fresh data in this window");
  if (w.burn_rate !== null && w.burn_rate > 0)
    lines.push(`Burn rate: ${w.burn_rate.toFixed(1)} %/h (est.)`);
  if (w.eta) lines.push(`At this rate the limit is reached around ${formatClock(w.eta)} (est.)`);
  if (w.resets_at) lines.push(`Resets: ${formatClock(w.resets_at)}`);
  if (p.plan_type) lines.push(`Plan: ${p.plan_type}`);
  if (p.extra_usage !== null && p.extra_usage > 0) lines.push(`Extra usage: ${p.extra_usage}`);
  if (p.tokens)
    lines.push(
      `Tokens: in ${fmt(p.tokens.input)}, out ${fmt(p.tokens.output)}, cache w/r ${fmt(
        p.tokens.cache_write,
      )}/${fmt(p.tokens.cache_read)}`,
    );
  if (p.detail) lines.push(p.detail);

  return lines.join("\n");
}

function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
