//! Incremental reader for the local Claude Code transcripts.
//!
//! Files are filtered by modification time, a byte offset per file is kept so
//! only appended bytes are parsed, and a cheap substring test runs before any
//! JSON parsing.
//!
//! Offsets alone are not enough. If only offsets survive a restart, the
//! in-memory records cover just the newest run, the window totals collapse and
//! the calibration factor pins to its ceiling. Hourly buckets and the hot
//! window are persisted alongside the offsets for that reason.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::model::TokenTotals;
use crate::settings::{data_dir, strip_bom, write_atomic};

// Price weighted local usage. Only the ratio between these matters, since the
// result is calibrated against the official percentage.
const W_OUTPUT: f64 = 5.0;
const W_CACHE_WRITE: f64 = 1.25;
const W_CACHE_READ: f64 = 0.1;

/// Streaming rewrites land within seconds, so two hours is a generous margin
/// for the deduplicated window.
const HOT_WINDOW_HOURS: i64 = 2;
const RETENTION_DAYS: i64 = 8;
const MIN_SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const INDEX_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Record {
    pub timestamp: DateTime<Utc>,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Record {
    pub fn weighted(&self) -> f64 {
        self.input as f64
            + self.output as f64 * W_OUTPUT
            + self.cache_write as f64 * W_CACHE_WRITE
            + self.cache_read as f64 * W_CACHE_READ
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Bucket {
    weighted: f64,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

impl Bucket {
    fn add(&mut self, r: &Record) {
        self.weighted += r.weighted();
        self.input += r.input;
        self.output += r.output;
        self.cache_write += r.cache_write;
        self.cache_read += r.cache_read;
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct FileMark {
    offset: u64,
    len: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    files: HashMap<String, FileMark>,
    /// Hour start as a unix timestamp mapped to its totals.
    #[serde(default)]
    buckets: BTreeMap<i64, Bucket>,
    /// Offsets have already moved past these records, so they cannot be re-read.
    #[serde(default)]
    hot: HashMap<String, Record>,
}

pub struct JsonlIndex {
    roots: Vec<PathBuf>,
    marks: HashMap<String, FileMark>,
    buckets: BTreeMap<i64, Bucket>,
    /// Keyed by message id and request id, last write wins.
    hot: HashMap<String, Record>,
    dirty: bool,
    last_scan: Option<std::time::Instant>,
}

impl JsonlIndex {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let persisted = std::fs::read_to_string(index_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Persisted>(strip_bom(&s)).ok())
            .filter(|p| p.version == INDEX_VERSION)
            .unwrap_or_default();

        Self {
            roots,
            marks: persisted.files,
            buckets: persisted.buckets,
            hot: persisted.hot,
            dirty: false,
            last_scan: None,
        }
    }

    #[allow(dead_code)]
    fn blank(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            marks: HashMap::new(),
            buckets: BTreeMap::new(),
            hot: HashMap::new(),
            dirty: false,
            last_scan: None,
        }
    }

    pub fn refresh(&mut self) {
        if let Some(last) = self.last_scan {
            if last.elapsed() < MIN_SCAN_INTERVAL {
                return;
            }
        }
        self.last_scan = Some(std::time::Instant::now());

        let cutoff = std::time::SystemTime::from(Utc::now() - Duration::days(7));
        let mut seen: Vec<PathBuf> = Vec::new();

        for root in self.roots.clone() {
            if !root.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if modified < cutoff {
                    continue;
                }
                seen.push(path.to_path_buf());
                self.ingest(path, meta.len());
            }
        }

        self.fold_cold();
        self.prune();

        let keep: std::collections::HashSet<String> =
            seen.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let before = self.marks.len();
        self.marks.retain(|k, _| keep.contains(k));
        if self.marks.len() != before {
            self.dirty = true;
        }

        if self.dirty {
            self.save();
            self.dirty = false;
        }
    }

    fn ingest(&mut self, path: &Path, len: u64) {
        let key = path.to_string_lossy().to_string();
        let mark = self
            .marks
            .get(&key)
            .copied()
            .unwrap_or(FileMark { offset: 0, len: 0 });

        // A shrinking file was rotated or rewritten, so start over.
        let start = if len < mark.len { 0 } else { mark.offset };
        if start >= len {
            return;
        }

        let Ok(mut file) = File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return;
        }

        let mut reader = BufReader::with_capacity(256 * 1024, file);
        let mut consumed = start;
        let mut line = String::new();

        loop {
            line.clear();
            let read = match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            // A partial line is left for the next pass.
            if !line.ends_with('\n') {
                break;
            }
            consumed += read as u64;

            if memchr::memmem::find(line.as_bytes(), b"\"type\":\"assistant\"").is_none() {
                continue;
            }
            if let Some((k, rec)) = parse_assistant_line(&line) {
                self.hot.insert(k, rec);
            }
        }

        self.marks.insert(key, FileMark { offset: consumed, len });
        self.dirty = true;
    }

    fn fold_cold(&mut self) {
        let boundary = Utc::now() - Duration::hours(HOT_WINDOW_HOURS);
        let cold: Vec<String> = self
            .hot
            .iter()
            .filter(|(_, r)| r.timestamp < boundary)
            .map(|(k, _)| k.clone())
            .collect();

        for key in cold {
            if let Some(rec) = self.hot.remove(&key) {
                let hour = floor_hour(rec.timestamp).timestamp();
                self.buckets.entry(hour).or_default().add(&rec);
                self.dirty = true;
            }
        }
    }

    fn prune(&mut self) {
        let cutoff = (Utc::now() - Duration::days(RETENTION_DAYS)).timestamp();
        let before = self.buckets.len();
        self.buckets.retain(|&hour, _| hour >= cutoff);
        if self.buckets.len() != before {
            self.dirty = true;
        }
        let hot_cutoff = Utc::now() - Duration::days(RETENTION_DAYS);
        self.hot.retain(|_, r| r.timestamp > hot_cutoff);
    }

    fn save(&self) {
        let payload = Persisted {
            version: INDEX_VERSION,
            files: self.marks.clone(),
            buckets: self.buckets.clone(),
            hot: self.hot.clone(),
        };
        if let Ok(json) = serde_json::to_vec(&payload) {
            let _ = std::fs::create_dir_all(data_dir());
            let _ = write_atomic(&index_path(), &json);
        }
    }

    /// Weighted usage accumulated since a point in time.
    ///
    /// Buckets are hourly, so only hours starting after `since` are counted.
    /// The hot window produces no buckets, which keeps recent queries exact,
    /// and the five hour block always starts on the hour.
    pub fn weighted_since(&self, since: DateTime<Utc>) -> f64 {
        let from_buckets: f64 = self
            .buckets
            .range(since.timestamp()..)
            .map(|(_, b)| b.weighted)
            .sum();
        let from_hot: f64 = self
            .hot
            .values()
            .filter(|r| r.timestamp >= since)
            .map(|r| r.weighted())
            .sum();
        from_buckets + from_hot
    }

    pub fn totals_since(&self, since: DateTime<Utc>) -> TokenTotals {
        let mut t = TokenTotals::default();
        for (_, b) in self.buckets.range(since.timestamp()..) {
            t.input += b.input;
            t.output += b.output;
            t.cache_write += b.cache_write;
            t.cache_read += b.cache_read;
        }
        for r in self.hot.values().filter(|r| r.timestamp >= since) {
            t.input += r.input;
            t.output += r.output;
            t.cache_write += r.cache_write;
            t.cache_read += r.cache_read;
        }
        t.total = t.input + t.output + t.cache_write + t.cache_read;
        t
    }

    /// Start of the active five hour block: activity is sorted, the block opens
    /// on the hour of the first record and a gap longer than five hours starts a
    /// new one. Matches the convention used by ccusage so the numbers line up.
    pub fn active_block_start(&self) -> Option<DateTime<Utc>> {
        let mut times: Vec<DateTime<Utc>> = self
            .buckets
            .keys()
            .filter_map(|&h| DateTime::from_timestamp(h, 0))
            .collect();
        times.extend(self.hot.values().map(|r| r.timestamp));
        if times.is_empty() {
            return None;
        }
        times.sort_unstable();

        let five = Duration::hours(5);
        let mut block_start = floor_hour(times[0]);
        let mut last = times[0];

        for &t in times.iter().skip(1) {
            if t - block_start > five || t - last > five {
                block_start = floor_hour(t);
            }
            last = t;
        }

        let now = Utc::now();
        if now - last < five && now < block_start + five {
            Some(block_start)
        } else {
            None
        }
    }
}

fn floor_hour(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(t)
}

fn index_path() -> PathBuf {
    data_dir().join("offsets.json")
}

/// Unknown or missing fields drop the line, never the process.
fn parse_assistant_line(line: &str) -> Option<(String, Record)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let timestamp = DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);

    let message = v.get("message")?;
    let msg_id = message.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let request_id = v.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
    if msg_id.is_empty() && request_id.is_empty() {
        return None;
    }

    let usage = message.get("usage")?;
    let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);

    Some((
        format!("{msg_id}:{request_id}"),
        Record {
            timestamp,
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            cache_read: n("cache_read_input_tokens"),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(minutes_ago: i64, output: u64) -> Record {
        Record {
            timestamp: Utc::now() - Duration::minutes(minutes_ago),
            input: 0,
            output,
            cache_write: 0,
            cache_read: 0,
        }
    }

    #[test]
    fn parses_an_assistant_line() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-15T10:00:00.000Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":2,"cache_creation_input_tokens":4,"cache_read_input_tokens":100}}}"#;
        let (key, rec) = parse_assistant_line(line).expect("parses");
        assert_eq!(key, "msg_1:req_1");
        assert_eq!(rec.input, 10);
        assert!((rec.weighted() - 35.0).abs() < 1e-6);
    }

    #[test]
    fn ignores_other_line_types() {
        let line = r#"{"type":"user","timestamp":"2026-08-15T10:00:00.000Z"}"#;
        assert!(parse_assistant_line(line).is_none());
    }

    #[test]
    fn deduplication_keeps_the_last_write() {
        let mut idx = JsonlIndex::blank(vec![]);
        let a = r#"{"type":"assistant","timestamp":"2026-08-15T10:00:00.000Z","requestId":"r","message":{"id":"m","usage":{"output_tokens":1}}}"#;
        let b = r#"{"type":"assistant","timestamp":"2026-08-15T10:00:01.000Z","requestId":"r","message":{"id":"m","usage":{"output_tokens":9}}}"#;
        for l in [a, b] {
            let (k, r) = parse_assistant_line(l).unwrap();
            idx.hot.insert(k, r);
        }
        assert_eq!(idx.hot.len(), 1);
        assert_eq!(idx.hot.values().next().unwrap().output, 9);
    }

    #[test]
    fn folding_into_buckets_preserves_totals() {
        let mut idx = JsonlIndex::blank(vec![]);
        idx.hot.insert("old".into(), rec(240, 100));
        idx.hot.insert("new".into(), rec(5, 10));

        let before = idx.weighted_since(Utc::now() - Duration::hours(5));
        idx.fold_cold();

        assert_eq!(idx.hot.len(), 1);
        assert_eq!(idx.buckets.len(), 1);
        assert!((before - idx.weighted_since(Utc::now() - Duration::hours(5))).abs() < 1e-6);
    }

    #[test]
    fn recent_deltas_stay_exact() {
        let mut idx = JsonlIndex::blank(vec![]);
        idx.hot.insert("old".into(), rec(300, 1000));
        idx.fold_cold();
        idx.hot.insert("fresh".into(), rec(1, 4));

        let delta = idx.weighted_since(Utc::now() - Duration::minutes(2));
        assert!((delta - 20.0).abs() < 1e-6, "expected 20, got {delta}");
    }

    #[test]
    fn totals_survive_a_restart() {
        let mut idx = JsonlIndex::blank(vec![]);
        idx.hot.insert("cold".into(), rec(240, 100));
        idx.hot.insert("warm".into(), rec(30, 20));
        idx.fold_cold();

        let before = idx.weighted_since(Utc::now() - Duration::hours(5));

        let persisted = Persisted {
            version: INDEX_VERSION,
            files: idx.marks.clone(),
            buckets: idx.buckets.clone(),
            hot: idx.hot.clone(),
        };
        let restored: Persisted =
            serde_json::from_str(&serde_json::to_string(&persisted).unwrap()).unwrap();

        let reloaded = JsonlIndex {
            roots: vec![],
            marks: restored.files,
            buckets: restored.buckets,
            hot: restored.hot,
            dirty: false,
            last_scan: None,
        };

        let after = reloaded.weighted_since(Utc::now() - Duration::hours(5));
        assert!((before - after).abs() < 1e-6, "{before} != {after}");
    }

    #[test]
    fn old_buckets_are_pruned() {
        let mut idx = JsonlIndex::blank(vec![]);
        idx.hot.insert("ancient".into(), rec(60 * 24 * 20, 100));
        idx.fold_cold();
        assert_eq!(idx.buckets.len(), 1);
        idx.prune();
        assert!(idx.buckets.is_empty());
    }
}
