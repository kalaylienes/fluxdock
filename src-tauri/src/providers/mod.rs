//! Provider abstraction. Adding a source means implementing this trait and
//! declaring a colour pair; nothing in the core changes.

pub mod antigravity;
pub mod claude;
pub mod codex;

use std::path::PathBuf;

use crate::model::ProviderSnapshot;

#[allow(async_fn_in_trait)]
pub trait UsageProvider {
    fn id(&self) -> &'static str;

    /// Roots handed to the filesystem watcher.
    fn watch_paths(&self) -> Vec<PathBuf>;

    /// Never fails. Error conditions travel inside `ProviderSnapshot::status`.
    ///
    /// `force` marks a user requested refresh. Paths that cost a network call
    /// or spawn a process only run on that flag or on their own schedule, so a
    /// busy file watcher cannot turn into a request flood.
    async fn poll(&mut self, force: bool) -> ProviderSnapshot;
}

/// Notification key. The server returns `resets_at` with sub second jitter, so
/// the key is rounded to the minute; otherwise every poll would look like a new
/// window and fire the toast again.
pub fn fired_key(
    provider: &str,
    window: &str,
    resets_at: Option<chrono::DateTime<chrono::Utc>>,
    level: u8,
) -> String {
    let reset = resets_at
        .map(|t| t.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_else(|| "none".into());
    format!("{provider}:{window}:{reset}:{level}")
}

/// Runs blocking work without stalling a tokio worker. Outside a runtime, for
/// example during startup, it just calls through.
pub fn blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Spawns a command without flashing a console window.
pub fn quiet_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs a CLI entry point.
///
/// npm installs both a shell script and a `.cmd` shim under the same name.
/// `CreateProcess` cannot execute either directly, so batch shims go through
/// the command interpreter.
pub fn cli_command(path: &str) -> std::process::Command {
    let lower = path.to_ascii_lowercase();
    if cfg!(windows) && (lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        let mut cmd = quiet_command("cmd");
        cmd.arg("/C").arg(path);
        cmd
    } else {
        quiet_command(path)
    }
}

/// How long a lookup result is trusted. A CLI does not move around often, and a
/// missing one especially does not appear on its own.
#[cfg(windows)]
const PATH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// A lookup result and when it was taken. `None` is a remembered miss.
#[cfg(windows)]
type PathLookup = (Option<String>, std::time::Instant);

/// Remembered lookups, misses included. `where` is itself a process, so calling
/// it on every poll spawns one for a CLI that is not installed at all.
#[cfg(windows)]
static PATH_CACHE: parking_lot::Mutex<Option<std::collections::HashMap<String, PathLookup>>> =
    parking_lot::Mutex::new(None);

/// Picks an entry point Windows can actually launch.
///
/// `where` lists the extensionless shell script first, which fails with
/// "not a valid Win32 application", so executable extensions win.
#[cfg(windows)]
pub fn resolve_on_path(name: &str) -> Option<String> {
    {
        let mut guard = PATH_CACHE.lock();
        let cache = guard.get_or_insert_with(std::collections::HashMap::new);
        if let Some((hit, at)) = cache.get(name) {
            if at.elapsed() < PATH_CACHE_TTL {
                return hit.clone();
            }
        }
    }

    let found = lookup_on_path(name);
    PATH_CACHE
        .lock()
        .get_or_insert_with(std::collections::HashMap::new)
        .insert(name.to_string(), (found.clone(), std::time::Instant::now()));
    found
}

#[cfg(windows)]
fn lookup_on_path(name: &str) -> Option<String> {
    let out = quiet_command("where").arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let candidates: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

    candidates
        .iter()
        .find(|l| {
            let lower = l.to_ascii_lowercase();
            lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat")
        })
        .or(candidates.first())
        .map(|s| s.to_string())
}

#[cfg(not(windows))]
pub fn resolve_on_path(_name: &str) -> Option<String> {
    None
}
