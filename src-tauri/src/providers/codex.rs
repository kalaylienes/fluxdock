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

/// However long a window runs, a reading stops being presented as live after a
/// day. Codex only writes these numbers while it is running, so a week old
/// weekly percentage is a week of someone else's work away from the truth.
const MAX_FRESH_HOURS: i64 = 24;

/// The last payload Codex handed over, for the diagnostic report. Which windows
/// an account is given varies by plan, and this is the only way to see what a
/// machine on the other side of a bug report actually received.
static LAST_RAW: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

pub fn last_raw_payload() -> Option<String> {
    LAST_RAW.lock().clone()
}

pub struct CodexProvider {
    settings: CodexSettings,
    last: Option<RateSnapshot>,
    history: Vec<(DateTime<Utc>, f32)>,
    next_app_server: Option<DateTime<Utc>>,
}

/// One rate limit window as Codex reports it.
///
/// `window_minutes` is the only statement of what the window actually is. The
/// `primary` slot is not a promise of five hours: Codex fills whatever windows
/// the account has, and its own status view derives the label from the length.
#[derive(Debug, Clone, Copy)]
struct CodexWindow {
    used: f32,
    resets: Option<DateTime<Utc>>,
    minutes: Option<i64>,
}

#[derive(Debug, Clone)]
struct RateSnapshot {
    /// Timestamp of the event itself, which is where staleness comes from.
    at: DateTime<Utc>,
    primary: Option<CodexWindow>,
    secondary: Option<CodexWindow>,
    plan_type: Option<String>,
    limit_reached: bool,
    tokens: Option<TokenTotals>,
    /// The payload as it arrived, kept for the diagnostic report. Without it
    /// there is no way to tell a missing window from a mislabelled one on a
    /// machine nobody can inspect.
    raw: Option<String>,
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
        // Starting a process while a game is in front can flash a console window
        // over it, and that is enough to knock it out of fullscreen. Codex is
        // not running during a game anyway, so there is nothing new to read.
        if crate::window::fullscreen_now() {
            return None;
        }
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

    async fn poll(&mut self, _force: bool) -> ProviderSnapshot {
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
        // The cooldown holds for a forced refresh too. `force` is set on every
        // timer tick, not just on a user request, so honouring it here meant a
        // process every poll interval for as long as the rollouts stayed stale,
        // which is exactly the case where they never stop being stale.
        let cooled = self.next_app_server.map(|t| Utc::now() >= t).unwrap_or(true);

        let fresh = if rollout_stale && cooled {
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
        if rate.raw.is_some() {
            *LAST_RAW.lock() = rate.raw.clone();
        }

        let mut snap = ProviderSnapshot::empty("codex", "Codex CLI");
        snap.plan_type = rate.plan_type.clone();
        snap.tokens = rate.tokens;

        let now = Utc::now();
        let age = now - rate.at;

        let windows = ordered_windows(&rate);

        for (i, w) in windows.iter().enumerate() {
            let label = window_label(w.minutes);
            let expired = w.resets.map(|r| r <= now).unwrap_or(false);
            let span = trusted_for(w.minutes, i);

            // Burn rate only means anything on the window that moves within a
            // session, so it is tracked for the shortest one.
            let (burn, eta) = if i == 0 {
                self.history.push((rate.at, w.used));
                let cutoff = now - Duration::hours(6);
                self.history.retain(|(t, _)| *t > cutoff);
                let (burn, eta) = self.burn_and_eta(w.used);
                // An estimate that lands after the window has already reset is
                // an estimate of nothing, so it is dropped rather than shown.
                let eta = match (eta, w.resets) {
                    (Some(e), Some(r)) if e >= r => None,
                    (eta, _) => eta,
                };
                (burn, eta)
            } else {
                (None, None)
            };

            let snapshot = WindowSnapshot {
                utilization: if expired { 0.0 } else { w.used },
                label,
                resets_at: if expired { None } else { w.resets },
                source: if expired { Source::Estimate } else { Source::Official },
                as_of: rate.at,
                stale: age > span,
                eta,
                burn_rate: burn,
            };
            if i == 0 {
                snap.five_hour = Some(snapshot);
            } else {
                snap.weekly = Some(snapshot);
            }
        }

        let any_full = [snap.five_hour.as_ref(), snap.weekly.as_ref()]
            .into_iter()
            .flatten()
            .any(|w| w.utilization >= 100.0);

        snap.status = if rate.limit_reached || any_full {
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
    let root = field(rate_limits, &["rate_limits", "rateLimits"]).unwrap_or(rate_limits);

    let window = |key: &str| -> Option<CodexWindow> {
        let w = root.get(key)?;
        if w.is_null() {
            return None;
        }
        let used = field(w, &["used_percent", "usedPercent"])?.as_f64()?;
        let resets = field(w, &["resets_at", "resetsAt"])
            .and_then(|x| x.as_i64())
            .and_then(epoch_to_utc)
            .or_else(|| {
                field(w, &["resets_in_seconds", "resetsInSeconds"])
                    .and_then(|x| x.as_i64())
                    .map(|s| at + Duration::seconds(s))
            });
        Some(CodexWindow {
            used: used as f32,
            resets,
            minutes: field(w, &["window_minutes", "windowDurationMins"])
                .and_then(|x| x.as_i64())
                .filter(|m| *m > 0),
        })
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
        plan_type: field(root, &["plan_type", "planType"])
            .and_then(|x| x.as_str())
            .map(str::to_string),
        limit_reached: field(root, &["rate_limit_reached_type", "rateLimitReachedType"]).is_some(),
        tokens,
        raw: serde_json::to_string_pretty(root).ok(),
    })
}

/// The same snapshot arrives under two spellings. Rollout transcripts are
/// snake_case; the app server speaks camelCase and calls the window length
/// `windowDurationMins`. Reading only one of them is how the app server
/// fallback managed to return nothing at all.
fn field<'a>(v: &'a serde_json::Value, names: &[&str]) -> Option<&'a serde_json::Value> {
    names.iter().filter_map(|n| v.get(n)).find(|x| !x.is_null())
}

/// How long a reading about this window stays worth showing as live.
///
/// A window is stale once the snapshot is older than the window itself, because
/// by then everything in it could have rolled over. The declared length beats
/// the guess the slot would imply, and nothing is trusted past a day: Codex only
/// writes these numbers while it is running, so a week old weekly percentage is
/// a week of somebody else's work away from the truth.
fn trusted_for(minutes: Option<i64>, slot: usize) -> Duration {
    minutes
        .map(Duration::minutes)
        .unwrap_or_else(|| {
            if slot == 0 {
                WindowKind::FiveHour.duration()
            } else {
                WindowKind::Weekly.duration()
            }
        })
        .min(Duration::hours(MAX_FRESH_HOURS))
}

/// Shortest window first. The two fields on the snapshot are carriers, not
/// claims: an account whose only limit is weekly puts that weekly window in the
/// first one, and the label is what says so. Ordering by length also keeps the
/// fast moving window on the top row, where it has always been for the accounts
/// that have both.
///
/// Only when every window states its length, though. Sorting a declared length
/// against an undeclared one would rank the undeclared window on a number it
/// never gave, which is how a pair could end up drawn as two identical rows.
/// Without that information the slot order is all there is, and it is also what
/// the widget did before any of this, so old payloads are unaffected.
fn ordered_windows(rate: &RateSnapshot) -> Vec<CodexWindow> {
    let mut windows: Vec<CodexWindow> = [rate.primary, rate.secondary]
        .into_iter()
        .flatten()
        .collect();
    if windows.iter().all(|w| w.minutes.is_some()) {
        windows.sort_by_key(|w| w.minutes.unwrap_or_default());
    }
    windows
}

/// Codex names a window by how long it is, never by which slot it arrived in.
/// These are the same buckets and the same five percent tolerance the CLI's own
/// status view uses, shortened to fit a row two characters wide.
fn window_label(minutes: Option<i64>) -> Option<String> {
    let m = minutes.filter(|m| *m > 0)?;
    let near = |expected: i64| {
        let e = expected as f64;
        (m as f64) >= e * 0.95 && (m as f64) <= e * 1.05
    };
    Some(match m {
        _ if near(5 * 60) => "5h".into(),
        _ if near(24 * 60) => "1d".into(),
        _ if near(7 * 24 * 60) => "7d".into(),
        _ if near(30 * 24 * 60) => "1M".into(),
        _ if near(365 * 24 * 60) => "1y".into(),
        // Anything else is described rather than guessed at.
        _ if m < 60 => format!("{m}m"),
        _ if m < 48 * 60 => format!("{}h", m / 60),
        _ => format!("{}d", m / (24 * 60)),
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

    #[cfg(windows)]
    let candidates = [
        home.join("AppData/Roaming/npm/codex.cmd"),
        home.join(".codex/bin/codex.exe"),
    ];

    #[cfg(not(windows))]
    let candidates = [
        home.join(".local/bin/codex"),
        home.join(".codex/bin/codex"),
        home.join(".npm-global/bin/codex"),
        home.join(".bun/bin/codex"),
        std::path::PathBuf::from("/usr/local/bin/codex"),
        std::path::PathBuf::from("/usr/bin/codex"),
    ];

    candidates
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
        let p = s.primary.unwrap();
        assert_eq!(p.used, 5.0);
        assert_eq!(p.minutes, Some(300));
        assert_eq!(s.secondary.unwrap().used, 13.0);
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
        assert_eq!(s.primary.unwrap().resets.unwrap(), at + Duration::hours(1));
    }

    #[test]
    fn rejects_an_event_without_windows() {
        let v = serde_json::json!({ "plan_type": "team" });
        assert!(parse_rate_limits(&v, Utc::now(), None).is_none());
    }

    #[test]
    fn a_window_is_named_by_its_length() {
        assert_eq!(window_label(Some(300)).as_deref(), Some("5h"));
        assert_eq!(window_label(Some(10080)).as_deref(), Some("7d"));
        assert_eq!(window_label(Some(1440)).as_deref(), Some("1d"));
        assert_eq!(window_label(Some(43200)).as_deref(), Some("1M"));
        assert_eq!(window_label(Some(525600)).as_deref(), Some("1y"));
        // Five percent either side, the same tolerance the CLI itself allows.
        assert_eq!(window_label(Some(295)).as_deref(), Some("5h"));
        assert_eq!(window_label(Some(10500)).as_deref(), Some("7d"));
    }

    #[test]
    fn an_unfamiliar_length_is_described_rather_than_guessed() {
        assert_eq!(window_label(Some(30)).as_deref(), Some("30m"));
        assert_eq!(window_label(Some(720)).as_deref(), Some("12h"));
        assert_eq!(window_label(Some(4320)).as_deref(), Some("3d"));
        assert_eq!(window_label(None), None);
        assert_eq!(window_label(Some(0)), None);
    }

    /// The case this whole mapping exists for: an account whose only limit is
    /// weekly still sends it as `primary`, and calling that row "5h" would be a
    /// number under a name that was never true.
    #[test]
    fn a_lone_weekly_window_is_not_called_five_hours() {
        let v = serde_json::json!({
            "primary": { "used_percent": 42.0, "window_minutes": 10080, "resets_at": 1781807972i64 },
            "secondary": null
        });
        let s = parse_rate_limits(&v, Utc::now(), None).expect("parses");
        let p = s.primary.unwrap();
        assert!(s.secondary.is_none());
        assert_eq!(window_label(p.minutes).as_deref(), Some("7d"));
    }

    fn snap(primary: serde_json::Value, secondary: serde_json::Value) -> RateSnapshot {
        let v = serde_json::json!({ "primary": primary, "secondary": secondary });
        parse_rate_limits(&v, Utc::now(), None).expect("parses")
    }

    fn win(used: f64, minutes: Option<i64>) -> serde_json::Value {
        match minutes {
            Some(m) => serde_json::json!({ "used_percent": used, "window_minutes": m }),
            None => serde_json::json!({ "used_percent": used }),
        }
    }

    #[test]
    fn both_windows_are_ordered_shortest_first() {
        let out = ordered_windows(&snap(win(5.0, Some(300)), win(13.0, Some(10080))));
        assert_eq!(
            out.iter().map(|w| w.minutes).collect::<Vec<_>>(),
            vec![Some(300), Some(10080)]
        );
    }

    /// The slot carries no meaning, so a pair that arrives the other way round
    /// is still drawn shortest first.
    #[test]
    fn a_reversed_pair_is_reordered_by_length() {
        let out = ordered_windows(&snap(win(13.0, Some(10080)), win(5.0, Some(300))));
        assert_eq!(
            out.iter().map(|w| (w.minutes, w.used)).collect::<Vec<_>>(),
            vec![(Some(300), 5.0), (Some(10080), 13.0)]
        );
    }

    #[test]
    fn a_lone_weekly_window_lands_in_the_first_slot_named_7d() {
        let out = ordered_windows(&snap(win(42.0, Some(10080)), serde_json::Value::Null));
        assert_eq!(out.len(), 1);
        assert_eq!(window_label(out[0].minutes).as_deref(), Some("7d"));
    }

    /// The server builds the two windows from separate headers, so the second
    /// can arrive without the first. Dropping it would lose the only limit the
    /// account has.
    #[test]
    fn a_secondary_only_payload_is_not_dropped() {
        let out = ordered_windows(&snap(serde_json::Value::Null, win(8.0, Some(300))));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].used, 8.0);
        assert_eq!(window_label(out[0].minutes).as_deref(), Some("5h"));
    }

    /// Payloads older than `window_minutes` keep the order they arrived in,
    /// which is the order the widget has always drawn them in.
    #[test]
    fn slot_order_survives_a_payload_without_lengths() {
        let out = ordered_windows(&snap(win(4.0, None), win(19.0, None)));
        assert_eq!(
            out.iter().map(|w| w.used).collect::<Vec<_>>(),
            vec![4.0, 19.0]
        );
        assert!(out.iter().all(|w| window_label(w.minutes).is_none()));
    }

    /// One declared length and one missing is not enough to rank them. Sorting
    /// on a number the other window never gave would put the weekly row first
    /// and leave the undeclared one falling back to the same name.
    #[test]
    fn a_partially_declared_pair_keeps_slot_order() {
        let out = ordered_windows(&snap(win(4.0, None), win(19.0, Some(10080))));
        assert_eq!(
            out.iter().map(|w| (w.minutes, w.used)).collect::<Vec<_>>(),
            vec![(None, 4.0), (Some(10080), 19.0)]
        );
    }

    #[test]
    fn a_reading_is_trusted_for_as_long_as_the_window_runs() {
        // Eight hours is nothing to a weekly window and everything to a five
        // hour one, and the declared length is what decides which is which.
        assert!(Duration::hours(8) < trusted_for(Some(10080), 0));
        assert!(Duration::hours(8) > trusted_for(Some(300), 0));
        // Never past a day, however long the window runs.
        assert_eq!(trusted_for(Some(10080), 0), Duration::hours(24));
        // With no declared length the slot is the only hint left.
        assert_eq!(trusted_for(None, 0), Duration::hours(5));
        assert_eq!(trusted_for(None, 1), Duration::hours(24));
    }

    /// The app server answers in camelCase and names the length differently.
    /// This is the exact shape a Plus account returned in August 2026, with the
    /// five hour window gone and the weekly one arriving as `primary`.
    #[test]
    fn reads_the_app_server_spelling_too() {
        let v = serde_json::json!({
            "rateLimits": {
                "limitId": "codex",
                "planType": "plus",
                "primary": { "usedPercent": 37, "windowDurationMins": 10080, "resetsAt": 1781807972i64 },
                "secondary": null
            }
        });
        let s = parse_rate_limits(&v, Utc::now(), None).expect("parses");
        let p = s.primary.unwrap();
        assert_eq!(p.used, 37.0);
        assert_eq!(p.minutes, Some(10080));
        assert_eq!(window_label(p.minutes).as_deref(), Some("7d"));
        assert!(s.secondary.is_none());
        assert_eq!(s.plan_type.as_deref(), Some("plus"));
        assert!(!s.limit_reached);
    }

    /// Older payloads predate `window_minutes`. Nothing is invented for them.
    #[test]
    fn a_window_without_a_declared_length_carries_no_label() {
        let v =
            serde_json::json!({ "primary": { "used_percent": 9.0, "resets_at": 1781239950i64 } });
        let s = parse_rate_limits(&v, Utc::now(), None).expect("parses");
        let p = s.primary.unwrap();
        assert_eq!(p.minutes, None);
        assert_eq!(window_label(p.minutes), None);
    }
}
