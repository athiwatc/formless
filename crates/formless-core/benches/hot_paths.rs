use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use formless_core::provider::{parse_sse_data_line, SseDataLine};
use formless_core::storage::{build_context, Store};
use formless_core::tools;
use formless_runtime::Capabilities;

fn session_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    for index in 0..1_000 {
        let session = store
            .create_session(Some(format!("benchmark-session-{index}")))
            .unwrap();
        store
            .append_message(
                &session.id,
                "user",
                &format!("benchmarkneedle conversation for session {index}"),
            )
            .unwrap();
    }
    assert_eq!(store.list_sessions().unwrap().len(), 1_000);
    assert_eq!(
        store.search_sessions("benchmarkneedle", 20).unwrap().len(),
        20
    );
    store
}

fn bench_session_queries(c: &mut Criterion) {
    let store = session_store();
    c.bench_function("sessions/list/1000", |b| {
        b.iter(|| black_box(store.list_sessions().unwrap()))
    });
    c.bench_function("sessions/fts_search/1000", |b| {
        b.iter(|| black_box(store.search_sessions("benchmarkneedle", 20).unwrap()))
    });
}

fn bench_transcript_and_checkpoint(c: &mut Criterion) {
    let store = Store::open_in_memory().unwrap();
    let session = store
        .create_session(Some("benchmark-transcript".into()))
        .unwrap();
    for index in 0..128 {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        store
            .append_message(
                &session.id,
                role,
                &format!("Transcript turn {index}: a representative offline answer and context."),
            )
            .unwrap();
    }
    store
        .save_checkpoint(&session.id, "Summary through turn 64", 64)
        .unwrap();
    assert_eq!(
        store.latest_checkpoint(&session.id).unwrap(),
        Some(("Summary through turn 64".into(), 64))
    );

    c.bench_function("transcript/build_context/128", |b| {
        b.iter(|| {
            black_box(build_context(&store, &session.id, "system prompt", 8_192, 0.8).unwrap())
        })
    });
    c.bench_function("compaction/checkpoint_round_trip", |b| {
        b.iter_batched(
            || {
                let checkpoint_store = Store::open_in_memory().unwrap();
                let checkpoint_session = checkpoint_store
                    .create_session(Some("benchmark-checkpoint".into()))
                    .unwrap();
                checkpoint_store
                    .save_checkpoint(&checkpoint_session.id, "Summary through turn 32", 32)
                    .unwrap();
                (checkpoint_store, checkpoint_session.id)
            },
            |(checkpoint_store, checkpoint_session_id)| {
                let id = checkpoint_store
                    .save_checkpoint(&checkpoint_session_id, "Summary through turn 64", 64)
                    .unwrap();
                black_box((
                    id,
                    checkpoint_store
                        .latest_checkpoint(&checkpoint_session_id)
                        .unwrap(),
                ))
            },
            BatchSize::PerIteration,
        )
    });
}

fn bench_tool_ranking(c: &mut Criterion) {
    let store = Store::open_in_memory().unwrap();
    let capabilities = Capabilities::parse(&Vec::<String>::new()).unwrap();
    for index in 0..200 {
        store
            .create_tool_version(
                &format!("benchmark_invoice_lookup_{index}"),
                &format!("Lookup invoice records in synthetic registry fixture {index}"),
                r#"{"type":"object"}"#,
                "{}",
                "// benchmark fixture",
                &capabilities,
                64,
                "benchmark",
            )
            .unwrap();
    }
    assert_eq!(store.list_tools(false).unwrap().len(), 200);
    assert_eq!(
        tools::search_tools_with(&store, "invoice lookup", 10, "bm25")
            .unwrap()
            .len(),
        10
    );
    c.bench_function("tools/find_tools_ranking/200", |b| {
        b.iter(|| {
            black_box(tools::search_tools_with(&store, "invoice lookup", 10, "bm25").unwrap())
        })
    });
}

fn bench_sse_parsing(c: &mut Criterion) {
    let mut lines = Vec::with_capacity(65);
    for index in 0..32 {
        lines.push(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"token {index}\"}}}}]}}"
        ));
        lines.push(String::new());
    }
    lines.push("data: [DONE]".into());

    c.bench_function("provider/sse_parse/32_events", |b| {
        let mut pending = String::new();
        b.iter(|| {
            let mut parsed = 0;
            for line in &lines {
                if let SseDataLine::Json(value) =
                    parse_sse_data_line(&mut pending, black_box(line)).unwrap()
                {
                    parsed += value["choices"][0]["delta"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .len();
                }
            }
            black_box(parsed)
        })
    });
}

criterion_group!(
    benches,
    bench_session_queries,
    bench_transcript_and_checkpoint,
    bench_tool_ranking,
    bench_sse_parsing
);
criterion_main!(benches);
