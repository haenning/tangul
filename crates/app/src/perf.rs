//! Objective numbers on drawing, to see whether a change makes the app faster:
//! how long apart frames are drawn, how long charts take to paint (when they
//! build their shapes and when they reuse them) and how many shapes are built
//! or reused.
//!
//! Recorded only while asked for: `TANGUL_PERF=1` prints a summary to stderr
//! about once a second while frames are drawn; tests call [`record`] and read
//! [`take`]. Otherwise every call returns at once.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

/// Frames further apart than this weren't drawn in a row (nothing changed in
/// between): left out of the intervals.
const IDLE: Duration = Duration::from_millis(250);
/// A frame at 60 Hz.
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);

/// What was recorded since the last [`take`].
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Frames drawn.
    pub frames: usize,
    /// Time between frames drawn in a row.
    pub intervals: Vec<Duration>,
    /// Chart paints that built at least one shape (first draw, new data, new
    /// size, other style).
    pub cold_paints: Vec<Duration>,
    /// Chart paints that reused all their shapes.
    pub warm_paints: Vec<Duration>,
    /// Chart paints of shapes that are never cached (bars).
    pub uncached_paints: Vec<Duration>,
    pub shapes_built: usize,
    pub shapes_reused: usize,
}

thread_local! {
    static RECORDING: Cell<bool> = Cell::new(std::env::var_os("TANGUL_PERF").is_some_and(|v| v == "1"));
    static LOG: Cell<bool> = Cell::new(std::env::var_os("TANGUL_PERF").is_some_and(|v| v == "1"));
    static STATS: RefCell<Stats> = RefCell::default();
    static LAST_FRAME: Cell<Option<Instant>> = const { Cell::new(None) };
    static LAST_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Starts or stops recording (tests); stopping drops what was recorded.
#[cfg(test)]
pub fn record(on: bool) {
    RECORDING.set(on);
    LAST_FRAME.set(None);
    STATS.take();
}

pub fn recording() -> bool {
    RECORDING.get()
}

/// What was recorded since the last call, cleared.
pub fn take() -> Stats {
    STATS.take()
}

/// A frame is being drawn.
pub fn frame() {
    if !recording() {
        return;
    }
    let now = Instant::now();
    let last = LAST_FRAME.replace(Some(now));
    STATS.with_borrow_mut(|s| {
        s.frames += 1;
        if let Some(gap) = last.map(|last| now - last).filter(|gap| *gap < IDLE) {
            s.intervals.push(gap);
        }
    });
    if LOG.get() {
        match LAST_LOG.get() {
            None => LAST_LOG.set(Some(now)),
            Some(at) if now - at >= Duration::from_secs(1) => {
                LAST_LOG.set(Some(now));
                eprintln!("perf: {}", summary(&take()));
            }
            Some(_) => {}
        }
    }
}

/// A chart was painted in `took`; `built` and `reused` count its cached shapes
/// (`None`: shapes that are never cached).
pub fn chart_painted(took: Duration, shapes: Option<(usize, usize)>) {
    if !recording() {
        return;
    }
    STATS.with_borrow_mut(|s| match shapes {
        None => s.uncached_paints.push(took),
        Some((built, reused)) => {
            s.shapes_built += built;
            s.shapes_reused += reused;
            if built > 0 {
                s.cold_paints.push(took)
            } else {
                s.warm_paints.push(took)
            }
        }
    });
}

/// The `q` quantile (0..=1) of `values`, nearest rank.
pub fn quantile(values: &[Duration], q: f64) -> Option<Duration> {
    let mut sorted = values.to_vec();
    sorted.sort();
    let last = sorted.len().checked_sub(1)?;
    Some(sorted[((last as f64) * q).round() as usize])
}

fn ms(d: Option<Duration>) -> String {
    d.map_or("-".into(), |d| format!("{:.1}ms", d.as_secs_f64() * 1000.))
}

fn spread(values: &[Duration]) -> String {
    format!(
        "{} (p50 {}, p95 {}, max {})",
        values.len(),
        ms(quantile(values, 0.5)),
        ms(quantile(values, 0.95)),
        ms(values.iter().max().copied())
    )
}

/// One line: frames and their intervals (how many over a 60 Hz frame), chart
/// paints by kind, shapes built and reused.
pub fn summary(s: &Stats) -> String {
    let slow = s.intervals.iter().filter(|d| **d > FRAME_BUDGET).count();
    format!(
        "frames {}, intervals {} {slow} over 16.7ms | chart paints: building {}, \
         from cache {}, never cached {} | shapes built {}, reused {}",
        s.frames,
        spread(&s.intervals),
        spread(&s.cold_paints),
        spread(&s.warm_paints),
        spread(&s.uncached_paints),
        s.shapes_built,
        s.shapes_reused,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_when_asked_and_sums_up() {
        record(false);
        chart_painted(Duration::from_millis(1), Some((1, 0)));
        assert_eq!(take().shapes_built, 0);
        record(true);
        chart_painted(Duration::from_millis(4), Some((2, 1)));
        chart_painted(Duration::from_millis(1), Some((0, 3)));
        chart_painted(Duration::from_millis(9), None);
        let s = take();
        assert_eq!((s.shapes_built, s.shapes_reused), (2, 4));
        assert_eq!(s.cold_paints, vec![Duration::from_millis(4)]);
        assert_eq!(s.warm_paints, vec![Duration::from_millis(1)]);
        assert_eq!(s.uncached_paints, vec![Duration::from_millis(9)]);
        record(false);
        let d = |ms: u64| Duration::from_millis(ms);
        assert_eq!(quantile(&[d(3), d(1), d(2)], 0.5), Some(d(2)));
        assert_eq!(quantile(&[d(3), d(1), d(2)], 1.), Some(d(3)));
        assert_eq!(quantile(&[], 0.5), None);
    }
}
