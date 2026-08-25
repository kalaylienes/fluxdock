//! Google Antigravity provider.
//!
//! The Antigravity CLI runs a language server on loopback and answers
//! `RetrieveUserQuotaSummary` with the same numbers its own usage view shows.
//! Nothing is reconstructed here: the server states a remaining fraction and a
//! reset time per bucket, which is exactly the shape the widget draws.
//!
//! The port changes on every start and is written into the CLI's own log, so
//! the log is the only thing read from disk. No credential is read, no secret
//! is taken out of the binary, and asking for the numbers does not spend any of
//! them.
//!
//! The reading only exists while the CLI is running. When it is not, the last
//! one is kept and ages: usage from this machine has stopped, but the same
//! account can be spent from the IDE or another computer.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::model::{ProviderSnapshot, ProviderStatus, Source, WindowSnapshot};
use crate::providers::{blocking, UsageProvider};
use crate::settings::AntigravitySettings;

const INSTALL_URL: &str = "https://antigravity.google/";

const LABEL: &str = "Antigravity";

/// Connect style unary call. A POST with an empty JSON body returns the whole
/// summary; there is no request field to fill in.
const RPC_PATH: &str = "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary";

/// The whole call is a loopback round trip. Anything slower than this is a port
/// that belongs to a process which has gone away.
const CALL_TIMEOUT: StdDuration = StdDuration::from_millis(1500);

/// A log big enough to hold a port line many times over. Reading further back
/// only finds ports of servers that have already exited.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// Log files older than this belong to sessions that cannot still be listening.
const LOG_MAX_AGE_DAYS: i64 = 2;

/// How long a reading is presented as live once the CLI has stopped answering.
/// Local usage has stopped by definition, but the same quota is shared with the
/// IDE and with any other machine signed into the account.
const MAX_FRESH_HOURS: i64 = 6;

/// The last payload the language server handed over, for the diagnostic report.
/// Which buckets an account has varies by tier, and this is the only way to see
/// what a machine on the other side of a bug report actually received.
static LAST_RAW: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

pub fn last_raw_payload() -> Option<String> {
    LAST_RAW.lock().clone()
}

pub struct AntigravityProvider {
    #[allow(dead_code)]
    settings: AntigravitySettings,
    client: reqwest::Client,
    /// The port that answered last time. Tried first, and only re-derived from
    /// the logs once it stops answering.
    port: Option<u16>,
    last: Option<Reading>,
}

/// One successful answer from the language server.
#[derive(Debug, Clone)]
struct Reading {
    at: DateTime<Utc>,
    buckets: Vec<Bucket>,
    raw: String,
}

/// One quota bucket, flattened out of its group. A group is a family of models
/// that share a limit, and today each one carries exactly one bucket.
#[derive(Debug, Clone)]
struct Bucket {
    /// Short row label, two or three characters.
    label: String,
    /// Percentage consumed, 0 to 100.
    used: f32,
    resets: Option<DateTime<Utc>>,
    /// The server's own word for the window length.
    #[allow(dead_code)]
    window: Option<String>,
    /// The group this bucket came from.
    group: Option<String>,
}

impl AntigravityProvider {
    pub fn new(settings: AntigravitySettings) -> Self {
        Self {
            settings,
            // Loopback, so the system proxy must not be consulted: a corporate
            // proxy that swallows 127.0.0.1 would turn a working local server
            // into a timeout on every poll.
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            port: None,
            last: None,
        }
    }

    pub fn apply_settings(&mut self, settings: AntigravitySettings) {
        self.settings = settings;
    }

    /// The CLI keeps its state under the shared Gemini directory rather than one
    /// of its own.
    fn home(&self) -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".gemini").join("antigravity-cli"))
    }

    fn log_dir(&self) -> Option<PathBuf> {
        self.home().map(|h| h.join("log"))
    }

    fn installed(&self) -> bool {
        self.home().map(|d| d.exists()).unwrap_or(false)
    }

    /// Ports to try, most recently written first.
    fn candidate_ports(&self) -> Vec<u16> {
        let Some(home) = self.home() else {
            return Vec::new();
        };

        let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        if let Some(dir) = self.log_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("log") {
                        continue;
                    }
                    let Ok(meta) = entry.metadata() else { continue };
                    let Ok(modified) = meta.modified() else {
                        continue;
                    };
                    files.push((modified, path));
                }
            }
        }
        // Newest first: the server that is still up wrote its port last.
        files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

        let cutoff = std::time::SystemTime::from(Utc::now() - Duration::days(LOG_MAX_AGE_DAYS));
        let mut ports = Vec::new();
        // The mirror of the current session's log is checked first, because it
        // is the one file whose name does not change.
        let mirror = (std::time::SystemTime::now(), home.join("cli.log"));
        for (modified, path) in std::iter::once(mirror).chain(files) {
            if modified < cutoff {
                continue;
            }
            let Some(text) = read_tail(&path, LOG_TAIL_BYTES) else {
                continue;
            };
            for port in http_ports(&text) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
        ports
    }

    /// Asks one port for the summary. `None` covers both a port nothing is
    /// listening on and a port belonging to something that is not this server.
    async fn ask(&self, port: u16) -> Option<(Vec<Bucket>, String)> {
        let url = format!("http://127.0.0.1:{port}{RPC_PATH}");
        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            return None;
        }
        let raw = response.text().await.ok()?;
        let parsed: QuotaEnvelope = serde_json::from_str(&raw).ok()?;
        let buckets = flatten(&parsed);
        if buckets.is_empty() {
            return None;
        }
        Some((buckets, raw))
    }

    /// Tries the remembered port, then everything the logs know about.
    async fn fetch(&mut self) -> Option<Reading> {
        if let Some(port) = self.port {
            if let Some((buckets, raw)) = self.ask(port).await {
                return Some(Reading {
                    at: Utc::now(),
                    buckets,
                    raw,
                });
            }
        }

        let remembered = self.port.take();
        let candidates = blocking(|| self.candidate_ports());
        for port in candidates {
            if Some(port) == remembered {
                continue;
            }
            if let Some((buckets, raw)) = self.ask(port).await {
                self.port = Some(port);
                return Some(Reading {
                    at: Utc::now(),
                    buckets,
                    raw,
                });
            }
        }

        None
    }
}

impl UsageProvider for AntigravityProvider {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        // Only the log directory. A new file appears there the moment a CLI
        // starts, which is exactly when a new port exists to be found, and it
        // is small enough not to turn every keystroke into a refresh.
        self.log_dir().into_iter().filter(|p| p.exists()).collect()
    }

    async fn poll(&mut self, _force: bool) -> ProviderSnapshot {
        if !self.installed() {
            let mut snap = ProviderSnapshot::empty("antigravity", LABEL);
            snap.status = ProviderStatus::CliNotFound;
            snap.detail = Some("Antigravity not found".into());
            snap.install_url = Some(INSTALL_URL);
            return snap;
        }

        if let Some(reading) = self.fetch().await {
            *LAST_RAW.lock() = Some(reading.raw.clone());
            self.last = Some(reading);
        }

        let Some(reading) = self.last.clone() else {
            let mut snap = ProviderSnapshot::empty("antigravity", LABEL);
            snap.status = ProviderStatus::NoDataYet;
            snap.detail = Some("Antigravity is not running".into());
            return snap;
        };

        let now = Utc::now();
        let age = now - reading.at;
        let stale = age > Duration::hours(MAX_FRESH_HOURS);

        let mut snap = ProviderSnapshot::empty("antigravity", LABEL);

        // Slot by order, label by content, the same rule Codex follows: the
        // field a window arrives in is an ordering, and the label is what the
        // row actually means.
        for (i, bucket) in reading.buckets.iter().take(2).enumerate() {
            let expired = bucket.resets.map(|r| r <= now).unwrap_or(false);
            let window = WindowSnapshot {
                utilization: if expired { 0.0 } else { bucket.used },
                label: Some(bucket.label.clone()),
                resets_at: if expired { None } else { bucket.resets },
                source: Source::Official,
                as_of: reading.at,
                stale,
                eta: None,
                burn_rate: None,
            };
            if i == 0 {
                snap.five_hour = Some(window);
            } else {
                snap.weekly = Some(window);
            }
        }

        let any_full = [snap.five_hour.as_ref(), snap.weekly.as_ref()]
            .into_iter()
            .flatten()
            .any(|w| w.utilization >= 100.0);

        snap.status = if any_full {
            ProviderStatus::LimitReached
        } else if stale {
            ProviderStatus::Stale
        } else {
            ProviderStatus::Ok
        };

        snap.detail = if stale {
            Some(format!(
                "Antigravity not running, last reading {} min ago",
                age.num_minutes().max(0)
            ))
        } else {
            reading.buckets.first().and_then(|b| b.group.clone())
        };

        snap
    }
}

/// The envelope the language server wraps its answer in.
#[derive(Debug, Deserialize)]
struct QuotaEnvelope {
    response: Option<QuotaSummary>,
}

#[derive(Debug, Deserialize)]
struct QuotaSummary {
    #[serde(default)]
    groups: Vec<QuotaGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaGroup {
    display_name: Option<String>,
    #[serde(default)]
    buckets: Vec<QuotaBucket>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucket {
    bucket_id: Option<String>,
    display_name: Option<String>,
    window: Option<String>,
    /// Proto3 omits a zero, so an account with nothing left sends no field at
    /// all. Defaulting to zero is therefore the correct reading, not a guess.
    #[serde(default)]
    remaining_fraction: f32,
    reset_time: Option<DateTime<Utc>>,
}

/// Flattens groups into rows in the order the server listed them.
fn flatten(envelope: &QuotaEnvelope) -> Vec<Bucket> {
    let Some(summary) = envelope.response.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for group in &summary.groups {
        for bucket in &group.buckets {
            let id = bucket.bucket_id.as_deref().unwrap_or_default();
            let fallback = group
                .display_name
                .as_deref()
                .or(bucket.display_name.as_deref())
                .unwrap_or("");
            out.push(Bucket {
                label: bucket_label(id, fallback),
                used: ((1.0 - bucket.remaining_fraction) * 100.0).clamp(0.0, 100.0),
                resets: bucket.reset_time,
                window: bucket.window.clone(),
                group: group.display_name.clone(),
            });
        }
    }
    out
}

/// A row label short enough for the strip.
///
/// The bucket id names the model family and the window, `gemini-weekly` and
/// `3p-weekly`, so the part before the dash is the half that distinguishes one
/// row from the other. Short ids are shown as they are, longer ones are cut to
/// three characters, which is the widest label the layout carries.
fn bucket_label(bucket_id: &str, fallback: &str) -> String {
    let stem = bucket_id
        .split('-')
        .next()
        .filter(|s| !s.is_empty())
        .or_else(|| fallback.split_whitespace().next())
        .unwrap_or("?");

    if stem.chars().count() <= 3 {
        return stem.to_uppercase();
    }

    let mut chars = stem.chars();
    let head: String = chars
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tail: String = chars.take(2).collect::<String>().to_lowercase();
    format!("{head}{tail}")
}

/// Every port the log says a plain HTTP listener was opened on, most recent
/// first. The TLS listener is announced by the same sentence one word later, so
/// the protocol is matched exactly rather than by prefix.
fn http_ports(text: &str) -> Vec<u16> {
    const MARK: &str = "listening on random port at ";
    let mut found = Vec::new();
    let mut rest = text;

    while let Some(i) = rest.find(MARK) {
        rest = &rest[i + MARK.len()..];
        let line = match rest.find('\n') {
            Some(end) => &rest[..end],
            None => rest,
        };
        let mut parts = line.split_whitespace();
        if let (Some(number), Some("for"), Some("HTTP")) =
            (parts.next(), parts.next(), parts.next())
        {
            if let Ok(port) = number.parse::<u16>() {
                found.push(port);
            }
        }
    }

    found.reverse();
    found
}

/// The end of a file as text. Logs are rotated per session and stay small, but
/// a long lived one should not be read into memory whole.
fn read_tail(path: &std::path::Path, limit: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > limit {
        file.seek(SeekFrom::Start(len - limit)).ok()?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a running Antigravity CLI 1.1.20 on a Pro account.
    const SAMPLE: &str = r#"{"response":{"groups":[
      {"displayName":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro",
       "buckets":[{"bucketId":"gemini-weekly","displayName":"Weekly Limit Remaining",
       "description":"You have used some of your weekly limit.","window":"weekly",
       "remainingFraction":0.749356,"resetTime":"2026-09-01T23:31:19Z"}]},
      {"displayName":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
       "buckets":[{"bucketId":"3p-weekly","displayName":"Weekly Limit Remaining","window":"weekly",
       "remainingFraction":1,"resetTime":"2026-09-01T23:33:25Z"}]}],
      "description":"Within each group, models share a weekly limit."}}"#;

    #[test]
    fn a_real_summary_becomes_two_rows() {
        let parsed: QuotaEnvelope = serde_json::from_str(SAMPLE).expect("sample parses");
        let buckets = flatten(&parsed);

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].label, "Gem");
        assert!(
            (buckets[0].used - 25.0644).abs() < 0.01,
            "{}",
            buckets[0].used
        );
        assert_eq!(buckets[0].window.as_deref(), Some("weekly"));
        assert_eq!(buckets[0].group.as_deref(), Some("Gemini Models"));
        assert!(buckets[0].resets.is_some());

        assert_eq!(buckets[1].label, "3P");
        assert_eq!(buckets[1].used, 0.0);
    }

    /// Proto3 leaves a zero out, so an exhausted bucket arrives with no
    /// fraction at all. Reading that as full is the whole point.
    #[test]
    fn a_missing_fraction_means_nothing_left() {
        let json = r#"{"response":{"groups":[{"displayName":"Gemini Models",
          "buckets":[{"bucketId":"gemini-weekly","window":"weekly"}]}]}}"#;
        let parsed: QuotaEnvelope = serde_json::from_str(json).expect("parses");
        let buckets = flatten(&parsed);

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].used, 100.0);
        assert!(buckets[0].resets.is_none());
    }

    #[test]
    fn an_answer_without_a_summary_is_not_one() {
        let parsed: QuotaEnvelope = serde_json::from_str(r#"{"code":"unimplemented"}"#).unwrap();
        assert!(flatten(&parsed).is_empty());
    }

    #[test]
    fn labels_are_short_enough_for_the_strip() {
        assert_eq!(bucket_label("gemini-weekly", "Gemini Models"), "Gem");
        assert_eq!(bucket_label("3p-weekly", "Claude and GPT models"), "3P");
        assert_eq!(bucket_label("", "Gemini Models"), "Gem");
        assert_eq!(bucket_label("", ""), "?");
        assert_eq!(bucket_label("pro-5h", ""), "PRO");
        for id in ["gemini-weekly", "3p-weekly", "", "pro-5h"] {
            assert!(bucket_label(id, "Gemini Models").chars().count() <= 3);
        }
    }

    /// The two listeners are announced by the same sentence, and "HTTPS" starts
    /// with "HTTP".
    #[test]
    fn only_the_plain_listener_is_picked_up() {
        let log = concat!(
            "server.go:597] Language server listening on random port at 60544 for HTTPS (gRPC)\n",
            "server.go:605] Language server listening on random port at 60545 for HTTP\n",
        );
        assert_eq!(http_ports(log), vec![60545]);
    }

    /// A log that saw several servers hands back the newest first, because the
    /// older ones are processes that have already exited.
    #[test]
    fn the_most_recent_port_is_tried_first() {
        let log = concat!(
            "listening on random port at 1111 for HTTP\n",
            "listening on random port at 2222 for HTTPS\n",
            "listening on random port at 3333 for HTTP\n",
        );
        assert_eq!(http_ports(log), vec![3333, 1111]);
    }

    /// Not part of the suite: it needs an Antigravity CLI running on this
    /// machine. Port discovery is the one part of this provider that cannot be
    /// covered by a fixture, so this is how it gets checked by hand:
    /// `cargo test --lib antigravity -- --ignored --nocapture`.
    #[ignore]
    #[tokio::test]
    async fn a_running_cli_answers_for_real() {
        let mut provider = AntigravityProvider::new(AntigravitySettings::default());
        assert!(provider.installed(), "Antigravity is not installed here");

        let ports = provider.candidate_ports();
        println!("candidate ports: {ports:?}");
        assert!(!ports.is_empty(), "no port found in the CLI logs");

        let reading = provider.fetch().await.expect("the language server answered");
        println!("port {:?}", provider.port);
        for bucket in &reading.buckets {
            println!(
                "{:>3}  {:>5.1}% used  resets {:?}  window {:?}",
                bucket.label, bucket.used, bucket.resets, bucket.window
            );
        }
        assert!(!reading.buckets.is_empty());
        assert!(reading.buckets.iter().all(|b| (0.0..=100.0).contains(&b.used)));
    }

    #[test]
    fn a_log_with_nothing_in_it_yields_nothing() {
        assert!(http_ports("").is_empty());
        assert!(http_ports("listening on random port at ").is_empty());
        assert!(http_ports("listening on random port at abc for HTTP").is_empty());
    }
}
