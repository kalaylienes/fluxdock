//! Polls the providers, publishes the payload and keeps the tray badge and
//! state file in step.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};

use crate::model::{ProviderStatus, UsagePayload};
use crate::providers::{fired_key, UsageProvider};
use crate::settings::{data_dir, write_atomic};
use crate::state::{AppState, Refresh};
use crate::tray;

/// Minimum spacing between cycles triggered by the file watcher.
const WATCHER_COALESCE: Duration = Duration::from_secs(3);

pub async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Refresh>,
) {
    cycle(&app, &state, true).await;
    let mut last_cycle = std::time::Instant::now();

    loop {
        let interval = {
            let s = state.settings.get();
            Duration::from_secs(s.polling.http_interval_secs.clamp(60, 3600))
        };

        let reason = tokio::select! {
            r = rx.recv() => match r {
                Some(r) => r,
                None => return,
            },
            _ = tokio::time::sleep(interval) => Refresh::Timer,
        };

        if reason == Refresh::Settings {
            let s = state.settings.get();
            state
                .claude
                .lock()
                .await
                .apply_settings(s.providers.claude.clone());
            state
                .codex
                .lock()
                .await
                .apply_settings(s.providers.codex.clone());
            state
                .antigravity
                .lock()
                .await
                .apply_settings(s.providers.antigravity.clone());
            let _ = app.emit("config", state.appearance());
        }

        // The watcher fires several times a second during active use, on top of
        // its own debounce.
        if reason == Refresh::Scheduled && last_cycle.elapsed() < WATCHER_COALESCE {
            continue;
        }

        let force = matches!(reason, Refresh::Manual | Refresh::Settings | Refresh::Timer);
        cycle(&app, &state, force).await;
        last_cycle = std::time::Instant::now();
    }
}

pub async fn cycle(app: &AppHandle, state: &Arc<AppState>, force: bool) {
    let settings = state.settings.get();

    let claude_enabled = settings.providers.claude.enabled;
    let codex_enabled = settings.providers.codex.enabled;
    let antigravity_enabled = settings.providers.antigravity.enabled;

    let (mut claude_snap, weekly_reset) = if claude_enabled {
        let mut guard = state.claude.lock().await;
        guard.set_http_interval(settings.polling.http_interval_secs);
        let snap = guard.poll(force).await;
        (Some(snap), guard.last_weekly_reset())
    } else {
        (None, None)
    };

    let mut codex_snap = if codex_enabled {
        Some(state.codex.lock().await.poll(force).await)
    } else {
        None
    };

    let mut antigravity_snap = if antigravity_enabled {
        Some(state.antigravity.lock().await.poll(force).await)
    } else {
        None
    };

    if let Some(s) = claude_snap.as_mut() {
        s.enabled = true;
    }
    if let Some(s) = codex_snap.as_mut() {
        s.enabled = true;
    }
    if let Some(s) = antigravity_snap.as_mut() {
        s.enabled = true;
    }

    // A weekly reset cannot be recovered locally, so the last one seen is kept.
    if let Some(reset) = weekly_reset {
        if settings.last_seen.claude_seven_day_resets_at != Some(reset) {
            state
                .settings
                .update(|s| s.last_seen.claude_seven_day_resets_at = Some(reset));
        }
    }

    let providers: Vec<_> = [claude_snap, codex_snap, antigravity_snap]
        .into_iter()
        .flatten()
        .collect();

    let onboarding = !providers.is_empty()
        && providers
            .iter()
            .all(|p| p.status == ProviderStatus::CliNotFound);

    let payload = UsagePayload {
        providers,
        generated_at: Utc::now(),
        onboarding,
    };

    check_thresholds(app, state, &payload);
    write_state_file(&payload);

    *state.payload.write() = Some(payload.clone());
    let _ = app.emit("usage", &payload);
    tray::update_badge(app, &payload);
}

/// Turns a row label back into something a notification can read out loud.
/// Anything unfamiliar is quoted as it came, which is better than inventing a
/// name for a window whose length nobody here recognised.
fn window_name(label: &str) -> String {
    match label {
        "5h" => "five hour".into(),
        "1d" => "daily".into(),
        "7d" => "weekly".into(),
        "1M" => "monthly".into(),
        "1y" => "annual".into(),
        // Antigravity names a bucket after the model family that shares it, so
        // its rows say which limit ran low rather than how long the window is.
        "Gem" => "Gemini weekly".into(),
        "3P" => "Claude and GPT weekly".into(),
        other => format!("{other} usage"),
    }
}

fn check_thresholds(app: &AppHandle, state: &Arc<AppState>, payload: &UsagePayload) {
    use tauri_plugin_notification::NotificationExt;

    let settings = state.settings.get();
    let mut newly_fired: Vec<String> = Vec::new();

    for provider in &payload.providers {
        for (slot, window) in [
            ("5h", provider.five_hour.as_ref()),
            ("7d", provider.weekly.as_ref()),
        ] {
            let Some(w) = window else { continue };
            // Codex states how long its windows are and they are not always the
            // pair this loop is named after, so the toast quotes what the
            // provider called the window rather than which field it arrived in.
            // The dedupe key stays on the slot: a label that came and went with
            // a field in the payload would look like a new window and fire the
            // same warning twice. A window that moves between slots, which is
            // what happens when a plan gains or loses its short window, re-fires
            // once for that reason. That is the cheaper side of the trade.
            let name = window_name(w.label.as_deref().unwrap_or(slot));
            if w.stale {
                continue;
            }
            for level in [90u8, 70u8] {
                let enabled = if level == 90 {
                    settings.notifications.at_90
                } else {
                    settings.notifications.at_70
                };
                if !enabled || w.utilization < level as f32 {
                    continue;
                }
                let key = fired_key(provider.id, slot, w.resets_at, level);
                if settings.notifications.fired.contains_key(&key) {
                    break;
                }
                let _ = app
                    .notification()
                    .builder()
                    .title(format!("{} at {}%", provider.label, level))
                    .body(format!(
                        "The {name} window is {:.0}% used",
                        w.utilization
                    ))
                    .show();
                newly_fired.push(key);
                break;
            }
        }
    }

    if !newly_fired.is_empty() {
        state.settings.update(|s| {
            for k in &newly_fired {
                s.notifications.fired.insert(k.clone(), true);
            }
            if s.notifications.fired.len() > 200 {
                s.notifications.fired.clear();
                for k in &newly_fired {
                    s.notifications.fired.insert(k.clone(), true);
                }
            }
        });
    }
}

/// Machine readable status at `state.json` in the data directory,
/// `%APPDATA%\FluxDock` on Windows and `~/.config/FluxDock` on Linux.
/// `generated_at` changes every cycle, so the comparison ignores it.
fn write_state_file(payload: &UsagePayload) {
    static LAST: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

    let Ok(json) = serde_json::to_string_pretty(payload) else {
        return;
    };
    let Ok(providers) = serde_json::to_string(&payload.providers) else {
        return;
    };

    {
        let mut last = LAST.lock();
        if last.as_deref() == Some(providers.as_str()) {
            return;
        }
        *last = Some(providers);
    }

    let _ = std::fs::create_dir_all(data_dir());
    let _ = write_atomic(&data_dir().join("state.json"), json.as_bytes());
}

pub fn spawn_watchers(app: &AppHandle, paths: Vec<std::path::PathBuf>) {
    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    if paths.is_empty() {
        tracing::info!("no paths to watch, falling back to the timer only");
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut debouncer) = new_debouncer(Duration::from_millis(300), None, tx) else {
            return;
        };
        for p in &paths {
            if let Err(e) = debouncer.watch(p, RecursiveMode::Recursive) {
                tracing::warn!("could not watch {p:?}: {e}");
            }
        }
        tracing::info!("watching {} root(s)", paths.len());

        for res in rx {
            if res.is_ok() {
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    state.request(Refresh::Scheduled);
                }
            }
        }
        drop(debouncer);
    });
}

/// Publishes synthetic values so the visual design can be reviewed without
/// waiting for real usage to build up. Providers are never polled in this mode
/// and nothing is written to disk.
pub async fn run_demo(app: AppHandle, state: Arc<AppState>) {
    use crate::model::{ProviderSnapshot, WindowSnapshot};
    use chrono::Duration as ChronoDuration;

    tracing::info!("demo mode, publishing synthetic data");
    let mut step: f32 = 0.0;

    loop {
        step += 0.6;

        // Placement sizes itself from the enabled providers, so the demo has to
        // honour the same setting or the window and its contents disagree.
        let settings = state.settings.get();

        let ramp = |offset: f32, span: f32| -> f32 {
            let raw = (step + offset) % (span * 2.0);
            if raw > span {
                span * 2.0 - raw
            } else {
                raw
            }
        };

        let window = |util: f32, minutes: i64| WindowSnapshot {
            utilization: util,
            label: None,
            resets_at: Some(Utc::now() + ChronoDuration::minutes(minutes)),
            source: crate::model::Source::Official,
            as_of: Utc::now(),
            stale: false,
            eta: None,
            burn_rate: Some(4.2),
        };

        let mut providers = Vec::new();

        if settings.providers.claude.enabled {
            let mut claude = ProviderSnapshot::empty("claude", "Claude Code");
            claude.status = ProviderStatus::Ok;
            claude.plan_type = Some("demo".into());
            claude.five_hour = Some(window(ramp(0.0, 100.0), 137));
            claude.weekly = Some(window(ramp(55.0, 100.0), 4210));
            providers.push(claude);
        }

        if settings.providers.codex.enabled {
            let mut codex = ProviderSnapshot::empty("codex", "Codex CLI");
            codex.status = ProviderStatus::Ok;
            codex.plan_type = Some("demo".into());
            codex.five_hour = Some(window(ramp(28.0, 100.0), 64));
            codex.weekly = Some(window(ramp(80.0, 100.0), 8300));
            providers.push(codex);
        }

        if settings.providers.antigravity.enabled {
            let mut antigravity = ProviderSnapshot::empty("antigravity", "Antigravity");
            antigravity.status = ProviderStatus::Ok;
            antigravity.plan_type = Some("demo".into());
            // Both rows are weekly here, which is what the account actually
            // has, so the labels rather than the slots say what they are.
            let mut gemini = window(ramp(12.0, 100.0), 5900);
            gemini.label = Some("Gem".into());
            let mut third_party = window(ramp(66.0, 100.0), 5900);
            third_party.label = Some("3P".into());
            antigravity.five_hour = Some(gemini);
            antigravity.weekly = Some(third_party);
            providers.push(antigravity);
        }

        let payload = UsagePayload {
            providers,
            generated_at: Utc::now(),
            onboarding: false,
        };

        *state.payload.write() = Some(payload.clone());
        let _ = app.emit("usage", &payload);
        tray::update_badge(&app, &payload);

        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}
