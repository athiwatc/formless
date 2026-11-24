use criterion::{black_box, criterion_group, criterion_main, Criterion};
use formless_tui::markdown;

fn bench_long_markdown(c: &mut Criterion) {
    let answer = (0..256)
        .map(|index| {
            format!(
                "## Result {index}\n\nA representative answer paragraph with **formatted text**, a link, and additional context for the reader.\n\n- First item\n- Second item\n\n"
            )
        })
        .collect::<String>();
    assert!(!markdown::render(&answer, false, "", 80).is_empty());

    c.bench_function("tui/markdown_render/long_answer", |b| {
        b.iter(|| black_box(markdown::render(black_box(&answer), false, "", 80)))
    });
}

criterion_group!(benches, bench_long_markdown);
criterion_main!(benches);
