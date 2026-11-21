//! Bounded preview model measurements; no GUI, OS transport or accessibility API.
//! `cargo run --no-default-features --example preview_profile -- 20`
use formless_desktop::model::BlockKind;
use formless_desktop::timeline::{
    accessible_text_stats, plain_text, Timeline, TimelineBlock, ACCESSIBLE_MAX_BYTES,
    PREVIEW_MAX_BYTES,
};
use serde_json::{json, Value};
use std::{hint::black_box, sync::Arc, time::Instant};

fn summary(mut samples: Vec<u64>) -> Value {
    samples.sort_unstable();
    let n = samples.len();
    json!({"min_ns":samples[0],"median_ns":samples[n/2],"p95_ns":samples[(n-1)*95/100],"samples":n})
}
fn digest(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{hash:016x}")
}
fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_nanos().try_into().unwrap()
}
fn row_case(name: &str, piece: &str, rounds: usize) {
    let mut source = String::new();
    while source.len() < 128 * 1024 {
        source.push_str(piece);
    }
    let mut setup = Vec::new();
    let mut cold = Vec::new();
    let mut hit = Vec::new();
    let mut page = Vec::new();
    let mut sizes = None;
    let mut digests = None;
    let builds = accessible_text_stats().0;
    for _ in 0..rounds {
        let start = Instant::now();
        let mut row = TimelineBlock::new(0, BlockKind::Assistant, source.clone());
        setup.push(elapsed(start));
        assert!(row.preview().len() <= PREVIEW_MAX_BYTES);
        assert_eq!(row.accessible_retained_bytes(), 0);
        let start = Instant::now();
        let text = row.accessible_text();
        cold.push(elapsed(start));
        assert_eq!(&*text, plain_text(row.preview()));
        assert!(text.len() <= ACCESSIBLE_MAX_BYTES);
        assert_eq!(row.accessible_retained_bytes(), text.len());
        let start = Instant::now();
        for _ in 0..1_000 {
            let cached = black_box(row.accessible_text());
            assert!(Arc::ptr_eq(&text, &cached));
        }
        // Mean of a 1,000-hit batch; not a per-hit tail-latency sample.
        hit.push(elapsed(start) / 1_000);
        let initial_sizes = (row.preview().len(), text.len());
        let initial_digest = digest(&text);
        let start = Instant::now();
        row.show_end();
        let end = row.accessible_text();
        page.push(elapsed(start));
        assert!(!Arc::ptr_eq(&text, &end));
        assert_eq!(&*end, plain_text(row.preview()));
        assert!(end.len() <= ACCESSIBLE_MAX_BYTES);
        let observed = (initial_sizes, (row.preview().len(), end.len()));
        let observed_digest = (initial_digest, digest(&end));
        if let Some(expected) = &sizes {
            assert_eq!(expected, &observed);
            assert_eq!(digests.as_ref().unwrap(), &observed_digest);
        } else {
            sizes = Some(observed);
            digests = Some(observed_digest);
        }
    }
    assert_eq!(accessible_text_stats().0 - builds, (rounds * 2) as u64);
    println!(
        "{}",
        json!({"case":name,"rounds":rounds,"source_bytes":source.len(),"preview_and_text_bytes":sizes,"text_digests_fnv1a":digests,"row_setup_including_source_clone":summary(setup),"cold_accessible_conversion":summary(cold),"cache_hit_batch_mean":summary(hit),"move_to_end_and_cold_conversion":summary(page),"accessible_builds":rounds*2,"cache_hits":rounds*1000,"gui":false,"production_streaming_enabled":false})
    );
}
fn history_case(count: usize, rounds: usize) {
    let source = "**History** ไทย 漢字 👩🏽‍💻\n".repeat(100);
    let mut setup = Vec::new();
    let mut visible = Vec::new();
    let mut retained = None;
    for _ in 0..rounds {
        let start = Instant::now();
        let timeline = Timeline::new(
            (0..count)
                .map(|id| TimelineBlock::new(id as u64, BlockKind::Assistant, source.clone()))
                .collect(),
        );
        setup.push(elapsed(start));
        assert_eq!(timeline.accessible_retained_bytes(), 0);
        let builds = accessible_text_stats().0;
        let start = Instant::now();
        for row in &timeline.blocks()[count - 5..] {
            black_box(row.accessible_text());
        }
        visible.push(elapsed(start));
        assert_eq!(accessible_text_stats().0 - builds, 5);
        let bytes = timeline.accessible_retained_bytes();
        assert!(bytes <= 5 * ACCESSIBLE_MAX_BYTES);
        if let Some(expected) = retained {
            assert_eq!(bytes, expected);
        }
        retained = Some(bytes);
    }
    println!(
        "{}",
        json!({"case":"history","rows":count,"rounds":rounds,"source_bytes_per_row":source.len(),"history_setup_including_source_clones":summary(setup),"five_visible_cold_conversions":summary(visible),"retained_accessible_bytes":retained,"visible_rows":5,"gui":false,"native_memory_measured":false})
    );
}
fn main() {
    let rounds = std::env::args()
        .nth(1)
        .map_or(20, |n| n.parse().expect("round count"));
    assert!((1..=50).contains(&rounds), "round count must be 1..=50");
    for (name, piece) in [
        ("words", "Plain ไทย 漢字 👩🏽‍💻 words. "),
        ("markdown", "**ย่อหน้า** with `code` and _emphasis_ 👩🏽‍💻.\n\n"),
        ("structured", "3. **item**\n   1. nested\n\n> [link](https://example.invalid)\n\n| a | b |\n|---|---|\n| ไทย | `x` |\n\n"),
        ("structured_long_line", "**item** [link](https://example.invalid) | ไทย `x` | "),
        ("ordered_list", "999999999. ไทย\n1. 漢字\n1. 👩🏽‍💻\n"),
    ] {
        row_case(name, piece, rounds);
    }
    for count in [100, 1_000] {
        history_case(count, rounds);
    }
}
