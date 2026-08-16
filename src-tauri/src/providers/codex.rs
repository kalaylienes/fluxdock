//! Codex CLI provider.
//!
//! Rate limits are read from the `token_count` events the CLI writes into its
//! rollout transcripts. Those percentages come from the server and are never
//! recomputed from token counts. They only advance while Codex is running, so
//! the age of a snapshot is treated as first class information.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};

use crate::model::{
    ProviderSnapshot, ProviderStatus, Source, TokenTotals, WindowKind, WindowSnapshot,
};
use crate::providers::{blocking, cli_command, resolve_on_path, UsageProvider};
use crate::settings::CodexSettings;

const INSTALL_URL: &str = "https://developers.openai.com/codex/cli/";

/// Enough of the file tail to hold the last `token_count` event.
const TAIL_BYTES: u64 = 512 * 1024;

/// Weekly window plus a margin.
const SCAN_DAYS: i64 = 9;

/// The app server fallback spawns a process, so it has its own cooldown.
const APP_SERVER_COOLDOWN_MINS: i64 = 5;

pub struct CodexProvider {
    settings: CodexSettings,
    last: Option<RateSnapshot>,
    history: Vec<(DateTime<Utc>, f32)>,
    next_app_server: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct RateSnapshot {
    /// Timestamp of the event itself, which is where staleness comes from.
    at: DateTime<Utc>,
    primary: Option<(f32, Option<DateTime<Utc>>)>,
    secondary: Option<(f32, Option<DateTime<Utc>>)>,
    plan_type: Option<String>,
    limit_reached: bool,
    tokens: Option<TokenTotals>,
}

impl CodexProvider {
    pub fn new(settings: CodexSettings) -> Self {
        Self {
            settings,
            last: None,
            history: Vec::new(),
            next_app_server: None,
        }
    }

    pub fn apply_settings(&mut self, settings: CodexSettings) {
        self.settings = settings;
    }

    /// Honours `CODEX_HOME`, the same variable the CLI reads.
    fn codex_dir(&self) -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("CODEX_HOME") {
            let p = PathBuf::from(custom);
            if !p.as_os_str().is_empty() {
                return Some(p);
            }
        }
        dirs::home_dir().map(|h| h.join(".codex"))
    }

    fn sessions_dir(&self) -> Option<PathBuf> {
        self.codex_dir().map(|d| d.join("sessions"))
    }

    fn installed(&self) -> bool {
        self.codex_dir().map(|d| d.exists()).unwrap_or(false)
    }

    /// Picks the newest event across the whole tree rather than the deepest
    /// dated folder, because a resumed session keeps writing to its original
    /// path. Limits are per account, so the freshest event anywhere wins.
    fn scan_rollouts(&self) -> Option<RateSnapshot> {
        let root = self.sessions_dir()?;
        if !root.exists() {
            return None;
        }
        let cutoff = std::time::SystemTime::from(Utc::now() - Duration::days(SCAN_DAYS));

        let mut best: Option<RateSnapshot> = None;
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.modified().map(|m| m < cutoff).unwrap_or(true) {
                continue;
            }

            if let Some(snap) = last_token_count(path, meta.len()) {
                best = match best {
                    Some(prev) if prev.at >= snap.at => Some(prev),
                    _ => Some(snap),
                };
            }
        }
        best
    }

    /// Asks the app server directly. Version stable and needs no interactive
    /// session, but it starts a process, so it is used sparingly.
    fn app_server_snapshot(&self) -> Option<RateSnapshot> {
        let bin = resolve_codex_binary()?;
        let mut child = cli_command(&bin)
            .arg("app-server")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;

        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let write = |w: &mut std::process::ChildStdin, v: serde_json::Value| -> std::io::Result<()> {
            w.write_all(serde_json::to_string(&v).unwrap_or_default().as_bytes())?;
            w.write_all(b"\n")?;
            w.flush()
        };

        let ok = write(
            &mut stdin,
            serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "clientInfo": { "name": "fluxdock", "title": "FluxDock", "version": env!("CARGO_PKG_VERSION") } }
            }),
        )
        .is_ok()
            && write(
                &mut stdin,
                serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
            )
            .is_ok()
            && write(
                &mut stdin,
                serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {} }),
            )
            .is_ok();

        if !ok {
            let _ = child.kill();
            return None;
        }

        let (tx, rx) = std::sync::mpsc::channel::<Option<RateSnapshot>>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if v.get("id").and_then(|x| x.as_i64()) != Some(2) {
                    continue;
                }
                let result = v.get("result").cloned().unwrap_or(serde_json::Value::Null);
                let _ = tx.send(parse_rate_limits(&result, Utc::now(), None));
                return;
            }
            let _ = tx.send(None);
        });

        let out = rx.recv_timeout(std::time::Duration::from_secs(8)).ok().flatten();
        let _ = child.kill();
        let _ = child.wait();
        out
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
        (
            Some(rate),
            Some(now + Duration::seconds((((100.0 - util).max(0.0) / rate) * 3600.0) as i64)),
        )
    }
}

impl UsageProvider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        self.sessions_dir().into_iter().filter(|p| p.exists()).collect()
    }

    async fn poll(&mut self, force: bool) -> ProviderSnapshot {
        if !self.installed() {
            let mut snap = ProviderSnapshot::empty("codex", "Codex CLI");
            snap.status = ProviderStatus::CliNotFound;
            snap.detail = Some("Codex CLI not found".into());
            snap.install_url = Some(INSTALL_URL);
            return snap;
        }

        let scanned = blocking(|| self.scan_rollouts());
        let fresh = match (&scanned, &self.last) {
            (Some(s), Some(prev)) if prev.at > s.at => self.last.clone(),
            (Some(s), _) => Some(s.clone()),
            (None, prev) => prev.clone(),
        };

        let rollout_stale = fresh
            .as_ref()
            .map(|s| Utc::now() - s.at >= Duration::hours(5))
            .unwrap_or(true);
        let cooled = self.next_app_server.map(|t| Utc::now() >= t).unwrap_or(true);

        let fresh = if rollout_stale && (force || cooled) {
            self.next_app_server = Some(Utc::now() + Duration::minutes(APP_SERVER_COOLDOWN_MINS));
            blocking(|| self.app_server_snapshot()).or(fresh)
        } else {
            fresh
        };

        let Some(rate) = fresh else {
            let mut snap = ProviderSnapshot::empty("codex", "Codex CLI");
            snap.status = ProviderStatus::NoDataYet;
            snap.detail = Some("no recent Codex usage".into());
            return snap;
        };

        self.last = Some(rate.clone());

        let mut snap = ProviderSnapshot::empty("codex", "Codex CLI");
        snap.plan_type = rate.plan_type.clone();
        snap.tokens = rate.tokens;

        let now = Utc::now();
        let age = now - rate.at;

        if let Some((util, resets)) = rate.primary {
            self.history.push((rate.at, util));
            let cutoff = now - Duration::hours(6);
            self.history.retain(|(t, _)| *t > cutoff);
            let (burn, eta) = self.burn_and_eta(util);

            let expired = resets.map(|r| r <= now).unwrap_or(false);
            snap.five_hour = Some(WindowSnapshot {
                utilization: if expired { 0.0 } else { util },
                resets_at: if expired { None } else { resets },
                source: if expired { Source::Estimate } else { Source::Official },
                as_of: rate.at,
                stale: age > WindowKind::FiveHour.duration(),
                eta,
                burn_rate: burn,
            });
        }
        if let Some((util, resets)) = rate.secondary {
            let expired = resets.map(|r| r <= now).unwrap_or(false);
            snap.weekly = Some(WindowSnapshot {
                utilization: if expired { 0.0 } else { util },
                resets_at: if expired { None } else { resets },
                source: if expired { Source::Estimate } else { Source::Official },
                as_of: rate.at,
                stale: age > WindowKind::Weekly.duration(),
                eta: None,
                burn_rate: None,
            });
        }

        let five_full = snap
            .five_hour
            .as_ref()
            .map(|w| w.utilization >= 100.0)
            .unwrap_or(false);

        snap.status = if rate.limit_reached || five_full {
            ProviderStatus::LimitReached
        } else if snap.all_stale() {
            ProviderStatus::Stale
        } else {
            ProviderStatus::Ok
        };

        if snap.status == ProviderStatus::Stale {
            snap.detail = Some(format!("snapshot {} min old", age.num_minutes().max(0)));
        }
        if self.settings.allow_http_fallback {
            // Left unimplemented on purpose: refreshing tokens over HTTP could
            // invalidate the refresh token the CLI stores and sign the user out.
            snap.detail
                .get_or_insert_with(|| "HTTP fallback is not enabled".to_string());
        }

        snap
    }
}

/// Reads backwards from the end of the file for the last `token_count` event.
fn last_token_count(path: &std::path::Path, len: u64) -> Option<RateSnapshot> {
    let mut file = std::fs::File::open(path).ok()?;
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let buf = String::from_utf8_lossy(&bytes);

    let mut lines: Vec<&str> = buf.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    for line in lines.iter().rev() {
        if !line.contains("\"token_count\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|x| x.as_str()) != Some("event_msg") {
            continue;
        }
        let payload = v.get("payload")?;
        if payload.get("type").and_then(|x| x.as_str()) != Some("token_count") {
            continue;
        }
        let at = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let tokens = payload
            .get("info")
            .filter(|i| !i.is_null())
            .and_then(|i| i.get("total_token_usage"))
            .map(|t| {
                let n = |k: &str| t.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                let input = n("input_tokens");
                let output = n("output_tokens");
                TokenTotals {
                    input,
                    output,
                    cache_write: 0,
                    cache_read: n("cached_input_tokens"),
                    total: n("total_tokens").max(input + output),
                }
            });

        // Events that carry only rate limits keep `info` null, and the first
        // event of a session is one of them, so they must not be skipped.
        if let Some(snap) = parse_rate_limits(payload.get("rate_limits")?, at, tokens) {
            return Some(snap);
        }
    }
    None
}

/// Both reset shapes are accepted. `resets_in_seconds` is relative to the event
/// timestamp, not to now, otherwise resumed sessions produce wrong countdowns.
fn parse_rate_limits(
    rate_limits: &serde_json::Value,
    at: DateTime<Utc>,
    tokens: Option<TokenTotals>,
) -> Option<RateSnapshot> {
    if rate_limits.is_null() {
        return None;
    }
    let root = rate_limits
        .get("rate_limits")
        .filter(|v| !v.is_null())
        .unwrap_or(rate_limits);

    let window = |key: &str| -> Option<(f32, Option<DateTime<Utc>>)> {
        let w = root.get(key)?;
        if w.is_null() {
            return None;
        }
        let used = w.get("used_percent").and_then(|x| x.as_f64())?;
        let resets = w
            .get("resets_at")
            .and_then(|x| x.as_i64())
            .and_then(epoch_to_utc)
            .or_else(|| {
                w.get("resets_in_seconds")
                    .and_then(|x| x.as_i64())
                    .map(|s| at + Duration::seconds(s))
            });
        Some((used as f32, resets))
    };

    let primary = window("primary");
    let secondary = window("secondary");
    if primary.is_none() && secondary.is_none() {
        return None;
    }

    Some(RateSnapshot {
        at,
        primary,
        secondary,
        plan_type: root.get("plan_type").and_then(|x| x.as_str()).map(str::to_string),
        limit_reached: root
            .get("rate_limit_reached_type")
            .map(|v| !v.is_null())
            .unwrap_or(false),
        tokens,
    })
}

/// Current builds write seconds; milliseconds are accepted in case that changes.
fn epoch_to_utc(v: i64) -> Option<DateTime<Utc>> {
    if v.abs() < 100_000_000_000 {
        DateTime::from_timestamp(v, 0)
    } else {
        DateTime::from_timestamp_millis(v)
    }
}

fn resolve_codex_binary() -> Option<String> {
    if let Some(found) = resolve_on_path("codex") {
        return Some(found);
    }
    let home = dirs::home_dir()?;
    [
        home.join("AppData/Roaming/npm/codex.cmd"),
        home.join(".codex/bin/codex.exe"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_windows() {
        let v = serde_json::json!({
            "primary": { "used_percent": 5.0, "window_minutes": 300, "resets_at": 1781239950i64 },
            "secondary": { "used_percent": 13.0, "window_minutes": 10080, "resets_at": 1781807972i64 },
            "plan_type": "team",
            "rate_limit_reached_type": null
        });
        let s = parse_rate_limits(&v, Utc::now(), None).expect("parses");
        assert_eq!(s.primary.unwrap().0, 5.0);
        assert_eq!(s.secondary.unwrap().0, 13.0);
        assert_eq!(s.plan_type.as_deref(), Some("team"));
        assert!(!s.limit_reached);
    }

    #[test]
    fn a_relative_reset_is_anchored_to_the_event() {
        let at = DateTime::parse_from_rfc3339("2026-08-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = serde_json::json!({ "primary": { "used_percent": 20.0, "resets_in_seconds": 3600 } });
        let s = parse_rate_limits(&v, at, None).expect("parses");
        assert_eq!(s.primary.unwrap().1.unwrap(), at + Duration::hours(1));
    }

    #[test]
    fn rejects_an_event_without_windows() {
        let v = serde_json::json!({ "plan_type": "team" });
        assert!(parse_rate_limits(&v, Utc::now(), None).is_none());
    }
}
