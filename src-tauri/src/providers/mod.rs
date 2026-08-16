//! Provider abstraction. Adding a source means implementing this trait and
//! declaring a colour pair; nothing in the core changes.

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
