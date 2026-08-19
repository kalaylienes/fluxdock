/** Mirrors the Rust `UsagePayload` and friends. */

export type Source = "official" | "estimate" | "estimate_local_only";

export type ProviderStatus =
  | "ok"
  | "cli_not_found"
  | "no_data_yet"
  | "plan_unsupported"
  | "reauth_needed"
  | "endpoint_error"
  | "stale"
  | "limit_reached"
  | "disabled";

export interface WindowSnapshot {
  /** Percentage of the window consumed, 0 to 100. */
  utilization: number;
  /**
   * Short row label when the provider states how long the window is. Codex
   * does, and its first window is not always the five hour one.
   */
  label: string | null;
  resets_at: string | null;
  source: Source;
  /** When the value was measured, not when it was rendered. */
  as_of: string;
  stale: boolean;
  eta: string | null;
  /** Percentage points per hour. */
  burn_rate: number | null;
}

export interface TokenTotals {
  input: number;
  output: number;
  cache_write: number;
  cache_read: number;
  total: number;
}

export interface ProviderSnapshot {
  id: "claude" | "codex";
  label: string;
  enabled: boolean;
  status: ProviderStatus;
  detail: string | null;
  install_url: string | null;
  five_hour: WindowSnapshot | null;
  weekly: WindowSnapshot | null;
  weekly_opus: WindowSnapshot | null;
  weekly_sonnet: WindowSnapshot | null;
  plan_type: string | null;
  extra_usage: number | null;
  tokens: TokenTotals | null;
}

export interface UsagePayload {
  providers: ProviderSnapshot[];
  generated_at: string;
  /** True when no supported CLI was found at all. */
  onboarding: boolean;
}

export interface AppearanceConfig {
  theme: "system" | "dark" | "light";
  animations: boolean;
  compact_mode: boolean;
  show_model_weekly: boolean;
  placement: "float" | "taskbar";
  /** Occlusion and power state, decided on the Rust side. */
  motion_allowed: boolean;
  resolved_theme: "dark" | "light";
}
