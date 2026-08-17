//! Claude Code provider.
//!
//! The OAuth usage endpoint is authoritative and drives everything the widget
//! shows by default. Local transcripts are a secondary layer used for fallback,
//! for interpolation between polls and for the token breakdown in the tooltip;
//! anything derived from them is labelled as an estimate.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};

use crate::jsonl::JsonlIndex;
use crate::model::{
    ProviderSnapshot, ProviderStatus, Source, TokenTotals, WindowKind, WindowSnapshot,
};
use crate::net::build_client;
use crate::providers::{blocking, cli_command, resolve_on_path, UsageProvider};
use crate::settings::ClaudeSettings;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const FALLBACK_VERSION: &str = "2.0.0";
const INSTALL_URL: &str = "https://claude.com/claude-code";

/// The refresh command consumes quota, so it runs at most once an hour.
const REFRESH_COOLDOWN_MINS: i64 = 60;

/// Sensible bounds for percentage per weighted token.
const K_MIN: f64 = 1e-9;
const K_MAX: f64 = 1e-2;

/// Drift beyond this marks the estimate as machine local.
const DEVIATION_LIMIT: f64 = 0.25;

pub struct ClaudeProvider {
    http: reqwest::Client,
    settings: ClaudeSettings,
    index: Option<JsonlIndex>,
    calibration: HashMap<WindowKind, f64>,
    local_only: bool,
    last_good: Option<Official>,
    last_refresh_attempt: Option<DateTime<Utc>>,
    version_cache: Option<(String, DateTime<Utc>)>,
    /// Utilisation samples used for burn rate and time to exhaustion.
    history: Vec<(DateTime<Utc>, f32)>,
    last_weekly_reset: Option<DateTime<Utc>>,
    http_interval: u64,
    next_http_allowed: Option<DateTime<Utc>>,
    backoff_level: u32,
    last_status: ProviderStatus,
    last_detail: Option<String>,
}

#[derive(Debug, Clone)]
struct Official {
    at: DateTime<Utc>,
    five_hour: Option<(f32, Option<DateTime<Utc>>)>,
    weekly: Option<(f32, Option<DateTime<Utc>>)>,
    weekly_opus: Option<(f32, Option<DateTime<Utc>>)>,
    weekly_sonnet: Option<(f32, Option<DateTime<Utc>>)>,
    plan_type: Option<String>,
    extra_usage: Option<f32>,
}

struct Credential {
    token: String,
    expires_at: Option<DateTime<Utc>>,
}

impl ClaudeProvider {
    pub fn new(settings: ClaudeSettings, last_weekly_reset: Option<DateTime<Utc>>) -> Self {
        let version = detect_version().unwrap_or_else(|| FALLBACK_VERSION.to_string());
        Self {
            http: build_client(&format!("claude-code/{version}")),
            settings,
            index: None,
            calibration: HashMap::new(),
            local_only: false,
            last_good: None,
            last_refresh_attempt: None,
            version_cache: Some((version, Utc::now())),
            history: Vec::new(),
            last_weekly_reset,
            http_interval: 180,
            next_http_allowed: None,
            backoff_level: 0,
            last_status: ProviderStatus::NoDataYet,
            last_detail: None,
        }
    }

    pub fn apply_settings(&mut self, settings: ClaudeSettings) {
        self.settings = settings;
    }

    pub fn set_http_interval(&mut self, secs: u64) {
        self.http_interval = secs.clamp(60, 3600);
    }

    pub fn last_weekly_reset(&self) -> Option<DateTime<Utc>> {
        self.last_weekly_reset
    }

    /// Success returns to the base interval, failure backs off up to half an hour.
    fn schedule_next_http(&mut self, failed: bool) {
        if failed {
            self.backoff_level = (self.backoff_level + 1).min(5);
        } else {
            self.backoff_level = 0;
        }
        let delay = self.http_interval * 2u64.pow(self.backoff_level);
        self.next_http_allowed = Some(Utc::now() + Duration::seconds(delay.min(1800) as i64));
    }

    /// Honours `CLAUDE_CONFIG_DIR`, the same variable the CLI reads.
    fn claude_dir(&self) -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("CLAUDE_CONFIG_DIR") {
            let p = PathBuf::from(custom);
            if !p.as_os_str().is_empty() {
                return Some(p);
            }
        }
        dirs::home_dir().map(|h| h.join(".claude"))
    }

    fn projects_dir(&self) -> Option<PathBuf> {
        self.claude_dir().map(|d| d.join("projects"))
    }

    fn credential_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .settings
            .credential_paths
            .iter()
            .map(PathBuf::from)
            .collect();
        if let Some(dir) = self.claude_dir() {
            out.push(dir.join(".credentials.json"));
        }
        out
    }

    fn installed(&self) -> bool {
        self.claude_dir().map(|d| d.exists()).unwrap_or(false)
            || self.credential_paths().iter().any(|p| p.exists())
    }

    /// Reads the environment variable first, then every configured credential
    /// file, keeping the one with the furthest expiry.
    fn read_token(&self) -> Option<Credential> {
        if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
            if !t.trim().is_empty() {
                return Some(Credential {
                    token: t,
                    expires_at: None,
                });
            }
        }

        let mut best: Option<Credential> = None;
        for path in self.credential_paths() {
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            let oauth = v.get("claudeAiOauth").unwrap_or(&v);
            let Some(token) = oauth.get("accessToken").and_then(|x| x.as_str()) else {
                continue;
            };
            let expires_at = oauth
                .get("expiresAt")
                .and_then(|x| x.as_i64())
                .and_then(DateTime::from_timestamp_millis);

            let candidate = Credential {
                token: token.to_string(),
                expires_at,
            };
            best = match best {
                None => Some(candidate),
                Some(prev) if candidate.expires_at > prev.expires_at => Some(candidate),
                Some(prev) => Some(prev),
            };
        }
        best
    }

    fn ensure_index(&mut self) -> &mut JsonlIndex {
        if self.index.is_none() {
            let roots = self.projects_dir().into_iter().collect();
            self.index = Some(JsonlIndex::new(roots));
        }
        let idx = self.index.as_mut().expect("index");
        blocking(|| idx.refresh());
        idx
    }

    /// The endpoint rejects requests without a plausible client version, so the
    /// installed one is read once a day.
    fn user_agent(&mut self) -> String {
        let stale = self
            .version_cache
            .as_ref()
            .map(|(_, at)| Utc::now() - *at > Duration::days(1))
            .unwrap_or(true);
        if stale {
            let version = blocking(detect_version).unwrap_or_else(|| FALLBACK_VERSION.to_string());
            self.version_cache = Some((version, Utc::now()));
        }
        let v = self
            .version_cache
            .as_ref()
            .map(|(v, _)| v.clone())
            .unwrap_or_else(|| FALLBACK_VERSION.to_string());
        format!("claude-code/{v}")
    }

    fn try_cli_refresh(&mut self) -> bool {
        if !self.settings.allow_cli_refresh {
            return false;
        }
        // Starting a process while a game is in front can flash a console window
        // over it, and that is enough to knock it out of fullscreen. A token
        // that is already expired can wait until the game is over.
        if crate::window::fullscreen_now() {
            return false;
        }
        let now = Utc::now();
        if let Some(last) = self.last_refresh_attempt {
            if now - last < Duration::minutes(REFRESH_COOLDOWN_MINS) {
                return false;
            }
        }
        self.last_refresh_attempt = Some(now);

        blocking(|| {
            let Some(bin) = resolve_claude_binary() else {
                return false;
            };
            tracing::info!("asking the CLI to refresh its token");
            let mut cmd = cli_command(&bin);
            cmd.args(["-p", "."]);
            match cmd.spawn() {
                Ok(mut child) => {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => {
                                if std::time::Instant::now() > deadline {
                                    let _ = child.kill();
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(250));
                            }
                            Err(_) => break,
                        }
                    }
                    true
                }
                Err(e) => {
                    tracing::warn!("token refresh failed: {e}");
                    false
                }
            }
        })
    }

    /// Derives percentage per weighted token for each window and smooths it.
    fn calibrate(&mut self, official: &Official) {
        let (five_local, weekly_local) = {
            let now = Utc::now();
            let idx = self.ensure_index();
            let block = idx.active_block_start().unwrap_or(now - Duration::hours(5));
            (
                idx.weighted_since(block),
                idx.weighted_since(now - Duration::days(7)),
            )
        };

        let mut deviated = false;
        for (kind, official_pct, local) in [
            (WindowKind::FiveHour, official.five_hour.map(|(u, _)| u), five_local),
            (WindowKind::Weekly, official.weekly.map(|(u, _)| u), weekly_local),
        ] {
            let Some(pct) = official_pct else { continue };
            if local <= 0.0 {
                continue;
            }
            let fresh = (pct as f64 / local).clamp(K_MIN, K_MAX);

            if let Some(prev) = self.calibration.get(&kind).copied() {
                let predicted = prev * local;
                if pct > 1.0 && ((predicted - pct as f64).abs() / pct as f64) > DEVIATION_LIMIT {
                    deviated = true;
                }
                self.calibration.insert(kind, prev * 0.7 + fresh * 0.3);
            } else {
                self.calibration.insert(kind, fresh);
            }
        }
        self.local_only = deviated;
    }

    /// Last official reading plus the calibrated local delta since then.
    fn interpolate(&mut self, kind: WindowKind, base: f32, since: DateTime<Utc>) -> f32 {
        let Some(k) = self.calibration.get(&kind).copied() else {
            return base;
        };
        let delta = {
            let idx = self.ensure_index();
            idx.weighted_since(since)
        };
        ((base as f64) + delta * k).clamp(0.0, 100.0) as f32
    }

    fn burn_and_eta(&self, util: f32) -> (Option<f32>, Option<DateTime<Utc>>) {
        let now = Utc::now();
        let Some((t0, u0)) = self
            .history
            .iter()
            .rev()
            .find(|(t, _)| now - *t >= Duration::minutes(10))
            .copied()
        else {
            return (None, None);
        };
        let hours = (now - t0).num_seconds() as f32 / 3600.0;
        if hours <= 0.0 {
            return (None, None);
        }
        let rate = (util - u0) / hours;
        if rate <= 0.1 {
            return (Some(rate.max(0.0)), None);
        }
        let remaining = (100.0 - util).max(0.0);
        (
            Some(rate),
            Some(now + Duration::seconds(((remaining / rate) * 3600.0) as i64)),
        )
    }

    fn push_history(&mut self, util: f32) {
        let now = Utc::now();
        self.history.push((now, util));
        let cutoff = now - Duration::hours(6);
        self.history.retain(|(t, _)| *t > cutoff);
    }

    fn snapshot_from_official(&mut self, official: &Official) -> ProviderSnapshot {
        let mut snap = ProviderSnapshot::empty("claude", "Claude Code");
        snap.plan_type = official.plan_type.clone();
        snap.extra_usage = official.extra_usage;

        if let Some((util, resets)) = official.five_hour {
            self.push_history(util);
            let (burn, eta) = self.burn_and_eta(util);
            let mut w = WindowSnapshot::official(util, resets);
            w.as_of = official.at;
            w.burn_rate = burn;
            w.eta = eta;
            snap.five_hour = Some(w);
        }
        if let Some((util, resets)) = official.weekly {
            let mut w = WindowSnapshot::official(util, resets);
            w.as_of = official.at;
            snap.weekly = Some(w);
            if resets.is_some() {
                self.last_weekly_reset = resets;
            }
        }
        if let Some((util, resets)) = official.weekly_opus {
            let mut w = WindowSnapshot::official(util, resets);
            w.as_of = official.at;
            snap.weekly_opus = Some(w);
        }
        if let Some((util, resets)) = official.weekly_sonnet {
            let mut w = WindowSnapshot::official(util, resets);
            w.as_of = official.at;
            snap.weekly_sonnet = Some(w);
        }

        let exhausted = |w: &Option<WindowSnapshot>| {
            w.as_ref().map(|w| w.utilization >= 100.0).unwrap_or(false)
        };
        snap.status = if exhausted(&snap.five_hour) || exhausted(&snap.weekly) {
            ProviderStatus::LimitReached
        } else {
            ProviderStatus::Ok
        };

        snap.tokens = Some(self.local_totals());
        snap
    }

    /// Token totals for the active block. The block starts on the hour, which
    /// lines up exactly with the hourly buckets.
    fn local_totals(&mut self) -> TokenTotals {
        let idx = self.ensure_index();
        let since = idx
            .active_block_start()
            .unwrap_or_else(|| Utc::now() - Duration::hours(5));
        idx.totals_since(since)
    }

    /// Remembers the state, then returns an interpolated snapshot. Repeating the
    /// same state while waiting for the next poll keeps the label steady.
    fn record(&mut self, status: ProviderStatus, detail: Option<String>) -> ProviderSnapshot {
        self.last_status = status;
        self.last_detail = detail.clone();
        self.interpolated(status, detail)
    }

    fn interpolated(&mut self, status: ProviderStatus, detail: Option<String>) -> ProviderSnapshot {
        let mut snap = ProviderSnapshot::empty("claude", "Claude Code");
        snap.status = status;
        snap.detail = detail;

        let source = if self.local_only {
            Source::EstimateLocalOnly
        } else {
            Source::Estimate
        };

        if let Some(last) = self.last_good.clone() {
            snap.plan_type = last.plan_type.clone();
            snap.extra_usage = last.extra_usage;

            if let Some((util, resets)) = last.five_hour {
                let expired = resets.map(|r| r <= Utc::now()).unwrap_or(false);
                let value = if expired {
                    0.0
                } else {
                    self.interpolate(WindowKind::FiveHour, util, last.at)
                };
                snap.five_hour = Some(WindowSnapshot {
                    utilization: value,
                    resets_at: if expired { None } else { resets },
                    source: label_source(source, util, value, expired),
                    as_of: last.at,
                    stale: Utc::now() - last.at > WindowKind::FiveHour.duration(),
                    eta: None,
                    burn_rate: None,
                });
            }
            if let Some((util, resets)) = last.weekly {
                let expired = resets.map(|r| r <= Utc::now()).unwrap_or(false);
                let value = if expired {
                    0.0
                } else {
                    self.interpolate(WindowKind::Weekly, util, last.at)
                };
                snap.weekly = Some(WindowSnapshot {
                    utilization: value,
                    // A weekly reset cannot be derived locally, so the last known
                    // one is projected forward until the next successful poll.
                    resets_at: if expired {
                        resets.map(|r| r + Duration::days(7))
                    } else {
                        resets
                    },
                    source: label_source(source, util, value, expired),
                    as_of: last.at,
                    stale: Utc::now() - last.at > WindowKind::Weekly.duration(),
                    eta: None,
                    burn_rate: None,
                });
            }
        }

        snap.tokens = Some(self.local_totals());
        snap
    }
}

impl UsageProvider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(p) = self.projects_dir() {
            if p.exists() {
                out.push(p);
            }
        }
        if let Some(d) = self.claude_dir() {
            if d.exists() {
                out.push(d);
            }
        }
        out
    }

    async fn poll(&mut self, force: bool) -> ProviderSnapshot {
        if !self.installed() {
            let mut snap = ProviderSnapshot::empty("claude", "Claude Code");
            snap.status = ProviderStatus::CliNotFound;
            snap.detail = Some("Claude Code not found".into());
            snap.install_url = Some(INSTALL_URL);
            return snap;
        }

        // The file watcher can fire several times a second; none of that turns
        // into an HTTP request. The local layer still refreshes.
        if !force {
            if let Some(next) = self.next_http_allowed {
                if Utc::now() < next {
                    let (status, detail) = (self.last_status, self.last_detail.clone());
                    return self.interpolated(status, detail);
                }
            }
        }

        let mut cred = self.read_token();
        if let Some(c) = &cred {
            if c.expires_at.map(|e| e <= Utc::now()).unwrap_or(false) && self.try_cli_refresh() {
                cred = self.read_token();
            }
        }

        let Some(cred) = cred else {
            self.schedule_next_http(true);
            return self.record(
                ProviderStatus::ReauthNeeded,
                Some("no credentials found, run claude once".into()),
            );
        };

        let ua = self.user_agent();
        let response = self
            .http
            .get(USAGE_URL)
            .header("Authorization", format!("Bearer {}", cred.token))
            .header("anthropic-beta", OAUTH_BETA)
            .header("User-Agent", ua)
            .header("Content-Type", "application/json")
            .send()
            .await;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                self.schedule_next_http(true);
                return self.record(ProviderStatus::EndpointError, Some(format!("network error: {e}")));
            }
        };

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            self.schedule_next_http(true);
            let detail = if self.try_cli_refresh() {
                "refreshing token"
            } else {
                "re-auth required, run claude"
            };
            return self.record(ProviderStatus::ReauthNeeded, Some(detail.into()));
        }
        if !status.is_success() {
            self.schedule_next_http(true);
            let detail = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                format!("endpoint 429, backing off (x{})", self.backoff_level)
            } else {
                format!("endpoint {status}")
            };
            return self.record(ProviderStatus::EndpointError, Some(detail));
        }

        let body: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                self.schedule_next_http(true);
                return self.record(
                    ProviderStatus::EndpointError,
                    Some(format!("response could not be read: {e}")),
                );
            }
        };

        self.schedule_next_http(false);

        let Some(official) = parse_usage(&body) else {
            self.last_status = ProviderStatus::PlanUnsupported;
            self.last_detail = Some("this plan does not report usage".into());
            let mut snap = ProviderSnapshot::empty("claude", "Claude Code");
            snap.status = ProviderStatus::PlanUnsupported;
            snap.detail = self.last_detail.clone();
            return snap;
        };

        self.calibrate(&official);
        self.last_good = Some(official.clone());
        let snap = self.snapshot_from_official(&official);
        self.last_status = snap.status;
        self.last_detail = None;
        snap
    }
}

/// Interpolation runs continuously between polls, so a value that still rounds
/// to the official reading is not marked as an estimate. Labelling every row
/// would drain the marker of meaning.
fn label_source(fallback: Source, official: f32, shown: f32, expired: bool) -> Source {
    if expired {
        return Source::Estimate;
    }
    if (shown - official).abs() < 0.5 {
        Source::Official
    } else {
        fallback
    }
}

/// Unknown fields are ignored and the payload may be nested, so the shape is
/// probed rather than deserialised into a fixed struct.
fn parse_usage(body: &serde_json::Value) -> Option<Official> {
    let root = body
        .get("usage")
        .or_else(|| body.get("data"))
        .unwrap_or(body);

    let window = |key: &str| -> Option<(f32, Option<DateTime<Utc>>)> {
        let w = root.get(key)?;
        if w.is_null() {
            return None;
        }
        let util = w
            .get("utilization")
            .and_then(|x| x.as_f64())
            .or_else(|| w.get("used_percent").and_then(|x| x.as_f64()))?;
        let resets = w
            .get("resets_at")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        Some((util as f32, resets))
    };

    let five_hour = window(WindowKind::FiveHour.key());
    let weekly = window(WindowKind::Weekly.key());
    if five_hour.is_none() && weekly.is_none() {
        return None;
    }

    Some(Official {
        at: Utc::now(),
        five_hour,
        weekly,
        weekly_opus: window(WindowKind::WeeklyOpus.key()),
        weekly_sonnet: window(WindowKind::WeeklySonnet.key()),
        plan_type: root
            .get("plan_type")
            .or_else(|| root.get("plan"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
        // The field is an object in current responses but a plain number in
        // older ones, so both are accepted.
        extra_usage: root.get("extra_usage").and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_f64().map(|x| x as f32),
            serde_json::Value::Object(_) => {
                if !v.get("is_enabled").and_then(|x| x.as_bool()).unwrap_or(false) {
                    return None;
                }
                v.get("utilization").and_then(|x| x.as_f64()).map(|x| x as f32)
            }
            _ => None,
        }),
    })
}

fn detect_version() -> Option<String> {
    let bin = resolve_claude_binary()?;
    let out = cli_command(&bin).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
}

fn resolve_claude_binary() -> Option<String> {
    if let Some(found) = resolve_on_path("claude") {
        return Some(found);
    }

    let home = dirs::home_dir()?;
    [
        home.join("AppData/Roaming/npm/claude.cmd"),
        home.join("AppData/Local/Programs/claude/claude.exe"),
        home.join(".local/bin/claude.exe"),
        home.join(".claude/local/claude.exe"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_official_windows() {
        let body = serde_json::json!({
            "five_hour": { "utilization": 34.5, "resets_at": "2026-08-15T12:00:00Z" },
            "seven_day": { "utilization": 12.0, "resets_at": "2026-08-20T00:00:00Z" },
            "seven_day_opus": null
        });
        let o = parse_usage(&body).expect("parses");
        assert!((o.five_hour.unwrap().0 - 34.5).abs() < 1e-6);
        assert!(o.weekly.is_some());
        assert!(o.weekly_opus.is_none());
    }

    #[test]
    fn rejects_a_response_without_windows() {
        let body = serde_json::json!({ "account_type": "console" });
        assert!(parse_usage(&body).is_none());
    }

    #[test]
    fn accepts_a_nested_payload() {
        let body = serde_json::json!({
            "data": { "five_hour": { "utilization": 5, "resets_at": "2026-08-15T12:00:00Z" } }
        });
        assert!(parse_usage(&body).is_some());
    }

    #[test]
    fn understands_both_shapes_of_extra_usage() {
        let base = |extra: serde_json::Value| {
            serde_json::json!({
                "five_hour": { "utilization": 10.0, "resets_at": "2026-08-15T12:00:00Z" },
                "extra_usage": extra
            })
        };

        assert_eq!(
            parse_usage(&base(serde_json::json!({ "is_enabled": false, "utilization": null })))
                .unwrap()
                .extra_usage,
            None
        );
        assert_eq!(
            parse_usage(&base(serde_json::json!({ "is_enabled": true, "utilization": 42.5 })))
                .unwrap()
                .extra_usage,
            Some(42.5)
        );
        assert_eq!(
            parse_usage(&base(serde_json::json!(7.0))).unwrap().extra_usage,
            Some(7.0)
        );
    }

    #[test]
    fn the_estimate_marker_follows_the_deviation() {
        assert_eq!(label_source(Source::Estimate, 80.0, 80.2, false), Source::Official);
        assert_eq!(label_source(Source::Estimate, 80.0, 81.4, false), Source::Estimate);
        assert_eq!(label_source(Source::Estimate, 80.0, 80.0, true), Source::Estimate);
        assert_eq!(
            label_source(Source::EstimateLocalOnly, 80.0, 90.0, false),
            Source::EstimateLocalOnly
        );
    }
}
