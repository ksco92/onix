//! Per-pass wall-time and peak-RSS profiler for the row diff, compiled only
//! under the `profile` feature and absent from the release wheel (which never
//! enables it). Peak RSS is sampled from `ps` in a background thread, so the
//! module needs no new dependency and no `unsafe`; each sample is attributed to
//! whichever pass is active when it is taken.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// One profiled pass: its label, the wall time between [`enter`] and its guard's
/// drop, and the highest RSS (KiB) any sample saw while it was active.
struct Pass {
    label: &'static str,
    start: Instant,
    wall: Duration,
    peak_rss_kib: u64,
    open: bool,
}

/// An additive sub-cost measured inside a streaming pass (the spill write, which
/// interleaves with the re-read and hash it shares a pass with).
struct Accum {
    label: &'static str,
    total: Duration,
}

struct State {
    passes: Vec<Pass>,
    accums: Vec<Accum>,
    sampler_running: bool,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                passes: Vec::new(),
                accums: Vec::new(),
                sampler_running: false,
            })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The index of the currently active pass for the sampler thread to attribute
/// RSS to; [`NO_PASS`] when no pass is open.
static ACTIVE: AtomicUsize = AtomicUsize::new(NO_PASS);
const NO_PASS: usize = usize::MAX;

/// Clears any prior run's records and starts the background RSS sampler. Call
/// once before the diff being profiled.
pub fn begin() {
    {
        let mut state = state();
        state.passes.clear();
        state.accums.clear();
        if state.sampler_running {
            return;
        }
        state.sampler_running = true;
    }
    ACTIVE.store(NO_PASS, Ordering::Release);
    if std::thread::Builder::new()
        .name("row-diff-profile-sampler".to_string())
        .spawn(sampler_loop)
        .is_err()
    {
        state().sampler_running = false;
    }
}

fn sampler_loop() {
    let pid = std::process::id();
    loop {
        std::thread::sleep(Duration::from_millis(15));
        if !state().sampler_running {
            break;
        }
        let idx = ACTIVE.load(Ordering::Acquire);
        if idx == NO_PASS {
            continue;
        }
        if let Some(rss) = read_rss_kib(pid)
            && let Some(pass) = state().passes.get_mut(idx)
        {
            pass.peak_rss_kib = pass.peak_rss_kib.max(rss);
        }
    }
}

/// The process's resident set size in KiB from `ps`, or `None` if `ps` is
/// unavailable or its output cannot be parsed.
fn read_rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Opens a pass named `label`; drop the returned guard to close it. Passes are
/// sequential, not nested: opening a pass while another is open leaves the
/// previous one's RSS attribution to the newer pass.
#[must_use]
pub fn enter(label: &'static str) -> PassGuard {
    // Read RSS before the timer starts so the boundary sample (which catches a
    // pass shorter than the sampler interval) never counts against the pass wall.
    let boundary = read_rss_kib(std::process::id()).unwrap_or(0);
    let now = Instant::now();
    let idx = {
        let mut state = state();
        let idx = state.passes.len();
        state.passes.push(Pass {
            label,
            start: now,
            wall: Duration::ZERO,
            peak_rss_kib: boundary,
            open: true,
        });
        idx
    };
    ACTIVE.store(idx, Ordering::Release);
    PassGuard { idx }
}

/// Adds `elapsed` to the additive sub-cost named `label` (created on first use).
pub fn accumulate(label: &'static str, elapsed: Duration) {
    let mut state = state();
    if let Some(accum) = state.accums.iter_mut().find(|a| a.label == label) {
        accum.total += elapsed;
    } else {
        state.accums.push(Accum {
            label,
            total: elapsed,
        });
    }
}

/// A live pass; its drop records the pass's wall time and clears the active
/// index so later samples are not attributed to it.
pub struct PassGuard {
    idx: usize,
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        if let Some(pass) = state().passes.get_mut(self.idx)
            && pass.open
        {
            pass.wall = pass.start.elapsed();
            pass.open = false;
        }
        let _ = ACTIVE.compare_exchange(self.idx, NO_PASS, Ordering::AcqRel, Ordering::Acquire);
        // Sample RSS after the wall is recorded, so the `ps` latency is untimed.
        let boundary = read_rss_kib(std::process::id()).unwrap_or(0);
        if let Some(pass) = state().passes.get_mut(self.idx) {
            pass.peak_rss_kib = pass.peak_rss_kib.max(boundary);
        }
    }
}

/// One reported pass: label, wall seconds, and peak RSS in MiB.
pub struct PassReport {
    /// The pass label.
    pub label: &'static str,
    /// Wall time in seconds.
    pub wall_secs: f64,
    /// Peak RSS while the pass was active, in MiB; `None` for an additive
    /// sub-cost, which owns no time window.
    pub peak_rss_mib: Option<f64>,
}

/// Stops the sampler and returns the recorded passes in order, followed by any
/// additive sub-costs.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn finish() -> Vec<PassReport> {
    let mut state = state();
    state.sampler_running = false;
    let mut reports: Vec<PassReport> = state
        .passes
        .iter()
        .map(|pass| PassReport {
            label: pass.label,
            wall_secs: pass.wall.as_secs_f64(),
            peak_rss_mib: Some(pass.peak_rss_kib as f64 / 1024.0),
        })
        .collect();
    reports.extend(state.accums.iter().map(|accum| PassReport {
        label: accum.label,
        wall_secs: accum.total.as_secs_f64(),
        peak_rss_mib: None,
    }));
    reports
}
