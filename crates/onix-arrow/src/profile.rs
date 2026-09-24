//! Per-pass wall-time and peak-RSS profiler for the row diff, compiled only
//! under the `profile` feature (the release wheel never enables it). Peak RSS is
//! sampled by a background thread running `/bin/ps`, so the module needs no new
//! dependency and no `unsafe`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

struct Pass {
    label: &'static str,
    start: Instant,
    wall: Duration,
    peak_rss_kib: u64,
    open: bool,
}

/// An additive sub-cost of the pass active when it was added.
struct Accum {
    label: &'static str,
    pass: usize,
    total: Duration,
}

struct State {
    passes: Vec<Pass>,
    accums: Vec<Accum>,
    /// The thread that called [`begin`]; `None` when not recording.
    owner: Option<ThreadId>,
    sampler_running: bool,
    sampler: Option<JoinHandle<()>>,
}

impl State {
    fn add(&mut self, label: &'static str, pass: usize, elapsed: Duration) {
        if let Some(accum) = self
            .accums
            .iter_mut()
            .find(|a| a.label == label && a.pass == pass)
        {
            accum.total += elapsed;
        } else {
            self.accums.push(Accum {
                label,
                pass,
                total: elapsed,
            });
        }
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                passes: Vec::new(),
                accums: Vec::new(),
                owner: None,
                sampler_running: false,
                sampler: None,
            })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

static ACTIVE: AtomicUsize = AtomicUsize::new(NO_PASS);
const NO_PASS: usize = usize::MAX;

/// The sub-cost the `ps` reads at pass boundaries accumulate under, so a caller
/// can subtract them from a wall timed around the profiled diff.
pub const BOUNDARY_LABEL: &str = "profiler: boundary ps reads";

/// Clears any prior records and records the passes entered on the calling
/// thread until the returned session is finished or dropped.
#[must_use = "dropping the session stops the profiler"]
pub fn begin() -> Session {
    ACTIVE.store(NO_PASS, Ordering::Release);
    let mut state = state();
    state.passes.clear();
    state.accums.clear();
    state.owner = Some(std::thread::current().id());
    if state.sampler.is_none() {
        state.sampler_running = true;
        // Spawned under the lock, so the handle is stored before the sampler's
        // first `state()` returns.
        match std::thread::Builder::new()
            .name("row-diff-profile-sampler".to_string())
            .spawn(sampler_loop)
        {
            Ok(handle) => state.sampler = Some(handle),
            Err(_) => state.sampler_running = false,
        }
    }
    Session { _private: () }
}

fn sampler_loop() {
    #[cfg(test)]
    LIVE_SAMPLERS.fetch_add(1, Ordering::AcqRel);
    let pid = std::process::id();
    loop {
        std::thread::sleep(Duration::from_millis(15));
        if !state().sampler_running {
            break;
        }
        sample_active(|| read_rss_kib(pid));
    }
    #[cfg(test)]
    LIVE_SAMPLERS.fetch_sub(1, Ordering::AcqRel);
}

/// Raises the active pass's peak to `read()`, which is not called when no pass
/// is active.
fn sample_active(read: impl FnOnce() -> Option<u64>) {
    let idx = ACTIVE.load(Ordering::Acquire);
    if idx == NO_PASS {
        return;
    }
    if let Some(rss) = read()
        && let Some(pass) = state().passes.get_mut(idx)
    {
        pass.peak_rss_kib = pass.peak_rss_kib.max(rss);
    }
}

fn read_rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(test)]
static LIVE_SAMPLERS: AtomicUsize = AtomicUsize::new(0);

/// Opens a pass named `label`, closed when the guard drops. Passes are
/// sequential, not nested, and record only on the thread that called [`begin`].
#[must_use]
pub fn enter(label: &'static str) -> PassGuard {
    if state().owner != Some(std::thread::current().id()) {
        return PassGuard { idx: NO_PASS };
    }
    let read_start = Instant::now();
    let boundary = read_rss_kib(std::process::id()).unwrap_or(0);
    let now = Instant::now();
    let idx = {
        let mut state = state();
        state.add(BOUNDARY_LABEL, NO_PASS, now - read_start);
        state.passes.push(Pass {
            label,
            start: now,
            wall: Duration::ZERO,
            peak_rss_kib: boundary,
            open: true,
        });
        state.passes.len() - 1
    };
    ACTIVE.store(idx, Ordering::Release);
    PassGuard { idx }
}

/// Adds `elapsed` to the sub-cost `label` of the active pass.
pub fn accumulate(label: &'static str, elapsed: Duration) {
    let mut state = state();
    if state.owner.is_some() {
        state.add(label, ACTIVE.load(Ordering::Acquire), elapsed);
    }
}

/// A live pass.
pub struct PassGuard {
    idx: usize,
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        if self.idx == NO_PASS {
            return;
        }
        if let Some(pass) = state().passes.get_mut(self.idx)
            && pass.open
        {
            pass.wall = pass.start.elapsed();
            pass.open = false;
        }
        let _ = ACTIVE.compare_exchange(self.idx, NO_PASS, Ordering::AcqRel, Ordering::Acquire);
        let read_start = Instant::now();
        let boundary = read_rss_kib(std::process::id()).unwrap_or(0);
        let mut state = state();
        state.add(BOUNDARY_LABEL, NO_PASS, read_start.elapsed());
        if let Some(pass) = state.passes.get_mut(self.idx) {
            pass.peak_rss_kib = pass.peak_rss_kib.max(boundary);
        }
    }
}

/// One reported pass or sub-cost.
pub struct PassReport {
    /// The pass or sub-cost label.
    pub label: &'static str,
    /// Wall time in seconds.
    pub wall_secs: f64,
    /// Peak RSS in MiB for a pass; `None` for a sub-cost.
    pub peak_rss_mib: Option<f64>,
}

/// A recording window opened by [`begin`].
pub struct Session {
    _private: (),
}

impl Session {
    /// Stops recording and returns each pass followed by its sub-costs, then
    /// the sub-costs added outside any pass.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn finish(self) -> Vec<PassReport> {
        stop();
        let state = state();
        let accums_of = |pass: usize| {
            state
                .accums
                .iter()
                .filter(move |a| a.pass == pass)
                .map(|a| PassReport {
                    label: a.label,
                    wall_secs: a.total.as_secs_f64(),
                    peak_rss_mib: None,
                })
        };
        let mut reports = Vec::new();
        for (idx, pass) in state.passes.iter().enumerate() {
            reports.push(PassReport {
                label: pass.label,
                wall_secs: pass.wall.as_secs_f64(),
                peak_rss_mib: Some(pass.peak_rss_kib as f64 / 1024.0),
            });
            reports.extend(accums_of(idx));
        }
        reports.extend(accums_of(NO_PASS));
        reports
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        stop();
    }
}

/// Stops recording and joins the sampler outside the lock, which the sampler
/// takes on every tick.
fn stop() {
    let handle = {
        let mut state = state();
        state.owner = None;
        state.sampler_running = false;
        state.sampler.take()
    };
    ACTIVE.store(NO_PASS, Ordering::Release);
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

/// Serializes the tests that open a session on the process-global profiler.
#[cfg(test)]
pub(crate) fn serialized() -> MutexGuard<'static, ()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{
        ACTIVE, BOUNDARY_LABEL, LIVE_SAMPLERS, NO_PASS, PassReport, accumulate, begin, enter,
        read_rss_kib, sample_active, serialized, state,
    };
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// The rows with the given labels; a row diff running on another test thread
    /// adds its own sub-costs to an open session.
    fn only<'a>(report: &'a [PassReport], labels: &[&str]) -> Vec<&'a PassReport> {
        report
            .iter()
            .filter(|row| labels.contains(&row.label))
            .collect()
    }

    #[test]
    fn finish_joins_the_sampler_so_cycles_never_accumulate() {
        let _guard = serialized();
        for _ in 0..20 {
            let session = begin();
            drop(enter("pass"));
            let _ = session.finish();
            assert_eq!(LIVE_SAMPLERS.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn a_second_begin_does_not_spawn_a_second_sampler() {
        let _guard = serialized();
        let first = begin();
        let second = begin();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(LIVE_SAMPLERS.load(Ordering::Acquire), 1);
        drop(second);
        drop(first);
        assert_eq!(LIVE_SAMPLERS.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_panic_inside_the_session_stops_recording_and_the_sampler() {
        let _guard = serialized();
        let unwound = std::panic::catch_unwind(|| {
            let _session = begin();
            let _pass = enter("panicking");
            std::thread::sleep(Duration::from_millis(50));
            panic!("unwind through the session");
        });
        assert!(unwound.is_err());
        assert_eq!(LIVE_SAMPLERS.load(Ordering::Acquire), 0);
        assert_eq!(ACTIVE.load(Ordering::Acquire), NO_PASS);
        assert!(state().owner.is_none());
    }

    #[test]
    fn reports_each_pass_followed_by_its_sub_costs() {
        let _guard = serialized();
        let session = begin();
        drop(enter("first"));
        {
            let _open = enter("second");
            accumulate("sub", Duration::from_millis(1));
            accumulate("sub", Duration::from_millis(2));
        }
        accumulate("outside", Duration::from_millis(4));
        let report = session.finish();
        let rows: Vec<_> = only(
            &report,
            &["first", "second", "sub", BOUNDARY_LABEL, "outside"],
        )
        .into_iter()
        .map(|r| (r.label, r.peak_rss_mib.is_some(), r.wall_secs))
        .collect();
        let labels: Vec<_> = rows.iter().map(|&(label, rss, _)| (label, rss)).collect();
        assert_eq!(
            labels,
            [
                ("first", true),
                ("second", true),
                ("sub", false),
                (BOUNDARY_LABEL, false),
                ("outside", false)
            ]
        );
        assert!((rows[2].2 - 0.003).abs() < 1e-9);
        assert!(rows[3].2 > 0.0);
        assert!((rows[4].2 - 0.004).abs() < 1e-9);
    }

    #[test]
    fn a_pass_wall_covers_its_body_and_its_peak_is_a_real_rss_in_mib() {
        let _guard = serialized();
        let session = begin();
        {
            let _pass = enter("sleeps");
            std::thread::sleep(Duration::from_millis(30));
        }
        let report = session.finish();
        let pass = only(&report, &["sleeps"])[0];
        assert!(pass.wall_secs >= 0.03, "{}", pass.wall_secs);
        let rss = pass.peak_rss_mib.expect("a pass carries RSS");
        assert!(rss > 1.0, "{rss}");
    }

    #[test]
    fn a_sample_raises_only_the_active_pass_peak_in_mib() {
        let _guard = serialized();
        let session = begin();
        sample_active(|| panic!("no pass is active, so no reading is taken"));
        {
            let _pass = enter("sampled");
            sample_active(|| Some(3 << 30));
        }
        let report = session.finish();
        assert_eq!(
            only(&report, &["sampled"])[0].peak_rss_mib,
            Some(f64::from(3u32 << 20))
        );
    }

    #[test]
    fn nothing_records_outside_a_session_or_off_its_thread() {
        let _guard = serialized();
        drop(enter("before"));
        accumulate("before", Duration::from_millis(1));
        assert!(state().passes.iter().all(|p| p.label != "before"));
        assert!(state().accums.iter().all(|a| a.label != "before"));
        let session = begin();
        std::thread::spawn(|| drop(enter("foreign")))
            .join()
            .unwrap();
        assert!(only(&session.finish(), &["foreign", BOUNDARY_LABEL]).is_empty());
    }

    #[test]
    fn rss_reads_are_the_process_rss_or_none_for_a_missing_process() {
        assert!(read_rss_kib(std::process::id()).is_some_and(|kib| kib > 1024));
        assert_eq!(read_rss_kib(u32::MAX), None);
    }
}
