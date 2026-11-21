//! Bounded model-only parser/conversion profile. No window or OS accessibility.
use std::hint::black_box;
use std::time::Instant;

fn measure(mut operation: impl FnMut(), rounds: usize) -> serde_json::Value {
    let mut samples = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let start = Instant::now();
        operation();
        samples.push(start.elapsed().as_micros() as u64);
    }
    samples.sort_unstable();
    serde_json::json!({"min_us":samples[0],"median_us":samples[rounds/2],"p95_us":samples[(rounds-1)*95/100]})
}
fn main() {
    let rounds = std::env::args()
        .nth(1)
        .map_or(20, |arg| arg.parse::<usize>().expect("round count"));
    assert!((1..=50).contains(&rounds), "round count must be 1..=50");
    for (name, piece) in [
        ("words", "Plain ไทย 漢字 👩🏽‍💻 words. "),
        ("markdown", "**ย่อหน้า** with `code` and _emphasis_ 👩🏽‍💻.\n\n"),
        ("structured", "3. **item**\n   1. nested\n\n> [link](https://example.invalid)\n\n| a | b |\n|---|---|\n| ไทย | `x` |\n\n"),
    ] {
        let mut source = String::new();
        while source.len() < 32 * 1024 { source.push_str(piece); }
        let options = markdown::ParseOptions::gfm();
        let parser = measure(|| { black_box(markdown::to_mdast(&source, &options).unwrap()); }, rounds);
        let conversion = measure(|| { black_box(formless_desktop::timeline::plain_text(&source)); }, rounds);
        let text = formless_desktop::timeline::plain_text(&source);
        let digest = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
        println!("{}", serde_json::json!({"case":name,"rounds":rounds,"source_bytes":source.len(),"text_bytes":text.len(),"text_digest":format!("{digest:016x}"),"parse_and_ast_drop":parser,"complete_conversion_and_drop":conversion,"gui":false}));
    }
}
