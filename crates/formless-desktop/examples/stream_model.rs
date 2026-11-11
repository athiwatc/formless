//! Replays the synthetic stream into the timeline model, with no window and
//! no GPUI, and prints what keeping the streamed row up to date cost.
//!
//! ```sh
//! cargo run --release --no-default-features --locked --example stream_model
//! ```
//!
//! Deltas arrive at a fixed rate on a simulated clock. The view is brought
//! up to date the way the shell does it: one flush, [`FLUSH_INTERVAL_MS`]
//! after the first delta it has not shown yet. The counts are exact and the
//! same on every run. The times are this machine's, for the model's work
//! only: no layout, no painting, no window.

use std::hint::black_box;
use std::time::Instant;

use formless_desktop::fixture;
use formless_desktop::model::BlockKind;
use formless_desktop::timeline::{Timeline, FLUSH_INTERVAL_MS};

const TRIALS: usize = 200;

struct Scenario {
    name: &'static str,
    /// Deltas a second; 0 means all at once.
    rate: u64,
    deltas: usize,
}

const SCENARIOS: [Scenario; 5] = [
    Scenario {
        name: "100-per-s-30s",
        rate: 100,
        deltas: 3_000,
    },
    Scenario {
        name: "100-per-s-90s",
        rate: 100,
        deltas: 9_000,
    },
    Scenario {
        name: "1000-per-s-10s",
        rate: 1_000,
        deltas: 10_000,
    },
    Scenario {
        name: "1000-per-s-60s",
        rate: 1_000,
        deltas: 60_000,
    },
    Scenario {
        name: "burst-100000-at-once",
        rate: 0,
        deltas: 100_000,
    },
];

/// What the view reads when it is brought up to date.
fn show(timeline: &mut Timeline) {
    timeline.settle();
    let row = timeline.blocks().last().expect("a streamed block");
    black_box((row.preview().len(), row.hidden_before(), row.hidden_after()));
}

/// One whole stream. Returns the timeline and how many times the view was
/// brought up to date.
fn replay(scenario: &Scenario) -> (Timeline, u64) {
    let mut timeline = Timeline::default();
    timeline.begin_stream(BlockKind::Assistant);
    let mut flushes = 0;
    // Simulated microseconds at which the pending flush runs.
    let mut flush_due: Option<u64> = None;
    for index in 0..scenario.deltas {
        let now = (index as u64 * 1_000_000)
            .checked_div(scenario.rate)
            .unwrap_or(0);
        if flush_due.is_some_and(|due| due <= now) {
            show(&mut timeline);
            flushes += 1;
            flush_due = None;
        }
        timeline.push_delta(fixture::stream_delta(index));
        flush_due.get_or_insert(now + FLUSH_INTERVAL_MS * 1_000);
    }
    timeline.finish_stream();
    show(&mut timeline);
    (timeline, flushes + 1)
}

/// FNV-1a, to compare what two builds ended up showing.
fn digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn main() {
    for scenario in &SCENARIOS {
        let mut nanos: Vec<u128> = (0..TRIALS)
            .map(|_| {
                let started = Instant::now();
                black_box(replay(scenario));
                started.elapsed().as_nanos()
            })
            .collect();
        nanos.sort_unstable();
        let (timeline, flushes) = replay(scenario);
        let work = timeline.stream_work();
        let row = timeline.blocks().last().expect("a streamed block");
        let per_delta = |total: u128| total as f64 / scenario.deltas as f64;
        println!(
            "model-stream: scenario={} deltas={} flushes={flushes} row_rebuilds={} \
             row_bytes_copied={} text_bytes={} not_shown_before={} not_shown_after={} \
             row_text_digest={:016x} text_digest={:016x} trials={TRIALS} \
             whole_stream_ms min={:.3} median={:.3} ns_per_delta min={:.0} median={:.0}",
            scenario.name,
            work.deltas,
            work.rebuilds,
            work.bytes_copied,
            row.total_bytes(),
            row.hidden_before(),
            row.hidden_after(),
            digest(row.preview()),
            digest(row.markdown()),
            nanos[0] as f64 / 1e6,
            nanos[TRIALS / 2] as f64 / 1e6,
            per_delta(nanos[0]),
            per_delta(nanos[TRIALS / 2]),
        );
    }
}
