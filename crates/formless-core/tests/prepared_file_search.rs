//! Differential oracle captured before #1491's ranking optimization.
//! The shared fold intentionally remains the current main implementation.
use formless_core::{
    files::{self, FileEntry},
    fuzzy::{self, fold, PreparedQuery},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}
struct CountAlloc;
fn record(bytes: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|count| count.set(count.get() + 1));
        let _ = BYTES.try_with(|count| count.set(count.get() + bytes));
    }
}
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
fn measured<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            TRACK.with(|flag| flag.set(false));
        }
    }
    ALLOCS.with(|n| n.set(0));
    BYTES.with(|n| n.set(0));
    TRACK.with(|flag| flag.set(true));
    let guard = Guard;
    let result = f();
    drop(guard);
    (result, ALLOCS.with(Cell::get), BYTES.with(Cell::get))
}

fn legacy_fuzzy_match(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = fold(query)
        .into_iter()
        .map(|(_, c)| c)
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let folded = fold(candidate);
    let c: Vec<char> = folded.iter().map(|(_, c)| *c).collect();
    let mut best: Option<(i64, Vec<usize>)> = None;
    for start in (0..c.len()).filter(|&i| c[i] == q[0]) {
        let Some(found) = legacy_match_from(&q, &c, start) else {
            // No later start can finish the match either.
            break;
        };
        if best.as_ref().is_none_or(|b| found.0 > b.0) {
            best = Some(found);
        }
    }
    // Prefer shorter ids among equal matches. Positions are the original
    // chars', so highlighting lands on the right glyphs.
    best.map(|(score, pos)| {
        let mut orig: Vec<usize> = pos.into_iter().map(|p| folded[p].0).collect();
        orig.dedup();
        (score - (c.len() / 8) as i64, orig)
    })
}

/// Greedy left-most match of `q` in `c` from `start`.
fn legacy_match_from(q: &[char], c: &[char], start: usize) -> Option<(i64, Vec<usize>)> {
    let mut positions = Vec::with_capacity(q.len());
    let mut score = 0i64;
    let mut prev: Option<usize> = None;
    for (i, ch) in c.iter().enumerate().skip(start) {
        if positions.len() == q.len() {
            break;
        }
        if *ch != q[positions.len()] {
            continue;
        }
        score += 1;
        // Runs of consecutive matches and matches at word starts count most.
        if prev.is_some_and(|p| p + 1 == i) {
            score += 5;
        }
        if i == 0 || !c[i - 1].is_alphanumeric() {
            score += 8;
        }
        if let Some(p) = prev {
            score -= (i - p - 1).min(10) as i64;
        }
        positions.push(i);
        prev = Some(i);
    }
    (positions.len() == q.len()).then_some((score, positions))
}

fn legacy_search<'a>(entries: &'a [FileEntry], query: &str, limit: usize) -> Vec<&'a FileEntry> {
    let query = query.trim();
    if query.is_empty() || query.ends_with('/') {
        let mut kids: Vec<&FileEntry> = entries
            .iter()
            .filter(|e| match e.path.strip_prefix(query) {
                Some(rest) => !rest.is_empty() && !rest.contains('/'),
                None => false,
            })
            .collect();
        kids.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.path.cmp(&b.path)));
        kids.truncate(limit);
        return kids;
    }
    let base_query = query.rsplit('/').next().unwrap_or(query);
    let mut scored: Vec<(i64, &FileEntry)> = entries
        .iter()
        .filter_map(|e| {
            let (mut score, _) = legacy_fuzzy_match(query, &e.path)?;
            let name = e.path.rsplit('/').next().unwrap_or(&e.path);
            if let Some((s, _)) = legacy_fuzzy_match(base_query, name) {
                score += 10 + s;
            }
            Some((score, e))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.path.len().cmp(&b.1.path.len()))
            .then(a.1.path.cmp(&b.1.path))
    });
    scored.into_iter().take(limit).map(|(_, e)| e).collect()
}

fn queries() -> Vec<&'static str> {
    vec![
        "",
        " ",
        "\u{301}",
        "a",
        "aa",
        "aaaaab",
        "ab",
        "a b",
        "a\tb",
        "src/lib",
        "src/ café",
        "İ",
        "i",
        "café",
        "cafe\u{301}",
        "ß",
        "ss",
        "STRASSE",
        "ΟΣ",
        "ος",
        "日本",
        "ไทย",
        "nomatch",
        "x/y",
        "/",
        "a/",
    ]
}
fn candidates() -> Vec<String> {
    let mut values = vec![
        "",
        "a",
        "ab",
        "a_b",
        "axb-ab",
        "baab",
        "aaaaaabaaaaab",
        "src/lib.rs",
        "src/café.rs",
        "src/cafe\u{301}.rs",
        "café/\u{301}abc.rs",
        "前文/\u{301}é.rs",
        "ß/\u{301}ß.rs",
        "İstanbul.rs",
        "i\u{307}stanbul.rs",
        "Straße.rs",
        "STRASSE.rs",
        "ΟΣ/ος.rs",
        "日本語/日本.rs",
        "ไทย/ไทย.txt",
        "src/",
        "x//y",
        "/root",
        "no-match",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    // Deterministic mixed strings include repeated starts and score ties.
    let alphabet = [
        'a', 'b', 'A', '/', '_', 'é', '\u{301}', 'ß', 'İ', 'σ', 'ς', '界',
    ];
    let mut state = 1491_u64;
    for n in 0..300 {
        let mut text = String::new();
        for _ in 0..n % 31 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(alphabet[(state >> 32) as usize % alphabet.len()]);
        }
        values.push(text);
    }
    values
}
#[test]
fn prepared_scores_and_public_highlights_match_the_pre_change_oracle() {
    for query in queries() {
        let prepared = PreparedQuery::new(query);
        for candidate in candidates() {
            let expected = legacy_fuzzy_match(query, &candidate);
            assert_eq!(
                prepared.score_folded(&fold(&candidate)),
                expected.as_ref().map(|v| v.0),
                "{query:?} {candidate:?}"
            );
            assert_eq!(
                fuzzy::fuzzy_match(query, &candidate),
                expected,
                "{query:?} {candidate:?}"
            );
        }
    }
}
#[test]
fn bounded_search_preserves_full_order_basename_scores_and_stable_duplicates() {
    let mut entries: Vec<_> = candidates()
        .into_iter()
        .enumerate()
        .map(|(n, path)| FileEntry {
            path,
            dir: n % 5 == 0,
            bytes: n as u64,
        })
        .collect();
    // Identical paths retain input order, including when the cutoff splits a tie.
    for n in 0..15 {
        entries.push(FileEntry {
            path: "a/aaaaab.rs".into(),
            dir: n % 2 == 0,
            bytes: 1000 + n,
        });
    }
    // The ranking oracle predates separator aliases. Supply the normalized
    // spelling explicitly so it still tests rankings without modeling aliases.
    let query_pairs = queries()
        .into_iter()
        .map(|query| (query, if query == "/" { "" } else { query }))
        .chain([
            (r"\", ""),
            (r"a\", "a/"),
            (r"src\lib", "src/lib"),
            (r"x\/", "x//"),
        ]);
    for (query, oracle_query) in query_pairs {
        for limit in [0, 1, 2, 10, 20, entries.len(), usize::MAX] {
            assert_eq!(
                files::search(&entries, query, limit),
                legacy_search(&entries, oracle_query, limit),
                "{query:?} limit {limit}"
            );
        }
    }
    for path in [
        "a/\u{301}é.rs",
        "a/\u{301}ß.rs",
        "x/İ.rs",
        "cafe\u{301}/café.rs",
        "x/\u{301}",
        "x/",
    ] {
        let folded = fold(path);
        let (prefix, name) = path.rsplit_once('/').unwrap();
        let start = prefix.chars().count() + 1;
        let offset = folded.partition_point(|(i, _)| *i < start);
        for query in queries() {
            assert_eq!(
                PreparedQuery::new(query).score_folded(&folded[offset..]),
                legacy_fuzzy_match(query, name).map(|v| v.0),
                "{query:?} {path:?}"
            );
        }
    }
}
#[test]
fn score_only_matching_allocates_nothing_even_with_many_attempts() {
    let query = PreparedQuery::new("aaaaab");
    let candidate = fold(&format!("{}b/{}b", "a".repeat(100), "a".repeat(100)));
    let (score, allocations, bytes) = measured(|| {
        let mut last = None;
        for _ in 0..100 {
            last = std::hint::black_box(query.score_folded(std::hint::black_box(&candidate)));
        }
        last
    });
    assert!(score.is_some());
    assert_eq!((allocations, bytes), (0, 0));
}
#[test]
fn file_search_reduces_measured_allocations_without_reserving_the_requested_limit() {
    let entries: Vec<_> = (0..2048)
        .map(|n| FileEntry {
            path: format!("src/{n:04}/aaaaab-aaaaab.rs"),
            dir: false,
            bytes: n,
        })
        .collect();
    let (old, old_allocs, old_bytes) = measured(|| legacy_search(&entries, "aab", 20));
    let (new, new_allocs, new_bytes) = measured(|| files::search(&entries, "aab", 20));
    assert_eq!(new, old);
    assert!(new_allocs < old_allocs / 2, "{new_allocs} vs {old_allocs}");
    assert!(new_bytes < old_bytes, "{new_bytes} vs {old_bytes}");
    let (all, _, bytes) = measured(|| files::search(&entries[..2], "aab", usize::MAX));
    assert_eq!(all.len(), 2);
    assert!(
        bytes < 16384,
        "small inputs must not reserve usize::MAX entries: {bytes}"
    );
    eprintln!("2048 fixed paths, query aab, limit 20: allocations {old_allocs} -> {new_allocs}; requested allocation bytes {old_bytes} -> {new_bytes}. Score-only attempts allocate zero. No timing benchmark.");
}
