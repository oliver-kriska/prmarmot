//! Frame-time measurement for performance work, built only with
//! `cargo build --features perf` (GPUI's profiler). Shipped builds never
//! include it.
//!
//! `PRMARMOT_PERF=/path/run.jsonl` turns it on. A measurement driver writes a
//! phase name into `/path/run.jsonl.phase`; whenever that name changes, the
//! phase that just ended is appended to the log as one JSON line with
//! percentiles in milliseconds: frame draw time, invalidation-to-present time,
//! input-to-frame latency (from GPUI starting to handle the first input to the
//! frame that shows it), how many inputs each such frame coalesced, and the
//! table rebuilds (`sync_table`) it ran.
//! `PRMARMOT_PERF_OVERLAY=1` also paints GPUI's frame-time overlay.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::{Context, Window};
use serde_json::{json, Value};

/// Rebuilds kept per phase; a driver that never changes phase stops adding.
const MAX_REBUILDS: usize = 100_000;

static REBUILDS: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

fn log_path() -> Option<&'static PathBuf> {
    static LOG: OnceLock<Option<PathBuf>> = OnceLock::new();
    LOG.get_or_init(|| std::env::var_os("PRMARMOT_PERF").map(PathBuf::from))
        .as_ref()
}

/// Times one table rebuild; records it when dropped.
pub struct RebuildTimer(Instant);

impl RebuildTimer {
    pub fn start() -> Self {
        Self(Instant::now())
    }
}

impl Drop for RebuildTimer {
    fn drop(&mut self) {
        if log_path().is_none() {
            return;
        }
        if let Ok(mut rebuilds) = REBUILDS.lock() {
            if rebuilds.len() < MAX_REBUILDS {
                rebuilds.push(self.0.elapsed());
            }
        }
    }
}

fn ms(nanos: u64) -> f64 {
    (nanos as f64 / 10_000.0).round() / 100.0
}

fn count(value: u64) -> f64 {
    value as f64
}

/// Percentiles of what a cumulative histogram gained since `$start`, in the
/// unit `$unit` converts to.
macro_rules! gained {
    ($now:expr, $start:expr, $unit:expr) => {{
        let mut gained = $now.clone();
        let _ = gained.subtract(&$start);
        json!({
            "n": gained.len(),
            "p50": $unit(gained.value_at_quantile(0.5)),
            "p90": $unit(gained.value_at_quantile(0.9)),
            "p99": $unit(gained.value_at_quantile(0.99)),
            "max": $unit(gained.max()),
        })
    }};
}

fn rebuild_stats(mut rebuilds: Vec<Duration>) -> Value {
    rebuilds.sort_unstable();
    let at = |q: f64| {
        rebuilds
            .get(((rebuilds.len() as f64 - 1.0) * q).round() as usize)
            .map_or(0.0, |took| ms(took.as_nanos() as u64))
    };
    json!({
        "n": rebuilds.len(),
        "p50": at(0.5),
        "p90": at(0.9),
        "p99": at(0.99),
        "max": at(1.0),
    })
}

pub fn start<V: 'static>(window: &mut Window, cx: &mut Context<V>) {
    let Some(log) = log_path() else {
        return;
    };
    if std::env::var_os("PRMARMOT_PERF_OVERLAY").is_some() {
        window.set_debug_frame_overlay_mode(gpui::DebugFrameOverlayMode::Full);
    }
    let mut phase_file = log.clone().into_os_string();
    phase_file.push(".phase");
    cx.spawn_in(window, async move |_this, cx| {
        let mut phase = String::new();
        let mut since: Option<(gpui::FrameDurationSnapshot, gpui::InputLatencySnapshot)> = None;
        let mut started = Instant::now();
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let next = std::fs::read_to_string(&phase_file).unwrap_or_default();
            let next = next.trim();
            if next == phase {
                continue;
            }
            let Ok(now) = cx.update(|window, _| {
                (
                    window.frame_duration_snapshot(),
                    window.input_latency_snapshot(),
                )
            }) else {
                break;
            };
            let rebuilds = REBUILDS
                .lock()
                .map(|mut rebuilds| std::mem::take(&mut *rebuilds))
                .unwrap_or_default();
            if let (Some((frames, input)), false) = (&since, phase.is_empty()) {
                let (frames_now, input_now) = &now;
                let line = json!({
                    "phase": phase,
                    "secs": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                    "draw_ms": gained!(frames_now.draw_duration_histogram, frames.draw_duration_histogram, ms),
                    "dirty_to_present_ms": gained!(frames_now.dirty_to_present_histogram, frames.dirty_to_present_histogram, ms),
                    "input_to_frame_ms": gained!(input_now.latency_histogram, input.latency_histogram, ms),
                    "inputs_per_frame": gained!(input_now.events_per_frame_histogram, input.events_per_frame_histogram, count),
                    "table_rebuild_ms": rebuild_stats(rebuilds),
                });
                let written = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)
                    .and_then(|mut file| writeln!(file, "{line}"));
                if let Err(error) = written {
                    eprintln!("prmarmot: could not write {}: {error}", log.display());
                }
            }
            since = Some(now);
            phase = next.to_owned();
            started = Instant::now();
        }
    })
    .detach();
}
