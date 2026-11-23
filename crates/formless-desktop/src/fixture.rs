//! Deterministic synthetic data for the spike and its tests.
//!
//! Nothing here comes from a Daemon Core: the fixture exists to exercise
//! rendering, virtualization and text shaping independently of the history
//! and pagination contracts that are still unverified.

use std::collections::HashSet;

use formless_protocol::SessionInfo;

use crate::model::BlockKind;
use crate::timeline::TimelineBlock;

/// Titles that exercise shaping: Thai (no word spaces, stacked marks), CJK,
/// emoji with ZWJ sequences, and mixed-direction text.
pub const TITLES: [&str; 8] = [
    "วางแผนทริปเชียงใหม่ช่วงปีใหม่",
    "四半期レポートの下書き",
    "季度预算复盘与下一步",
    "분기 회의록 정리",
    "Refactor the invoice Tool 🧾",
    "Family 👨‍👩‍👧‍👦 photo captions",
    "ملاحظات الاجتماع for Q3",
    "A very long Session title that has to clip in the sidebar without losing its full value",
];

const BODIES: [&str; 8] = [
    "ช่วยสรุปค่าใช้จ่ายของเดือนนี้ให้หน่อย แล้วบอกด้วยว่าหมวดไหนเกินงบ **มากที่สุด**",
    "了解しました。以下に要点をまとめます。\n\n1. 売上は前期比 **12%** 増\n2. 経費は横ばい\n   - 出張費のみ増加\n3. 来期の見通しは *慎重*",
    "下面是对比表：\n\n| 项目 | 上季度 | 本季度 |\n|---|---:|---:|\n| 收入 | 120 | 134 |\n| 支出 | 80 | 79 |\n\n结论：利润率提升。",
    "```rust\nfn total(items: &[u64]) -> u64 {\n    items.iter().sum() // 合計 / ผลรวม\n}\n```\n\nThe whitespace inside the block must survive a copy.",
    "Mixed direction: the file is named `تقرير.pdf` and the note says «שלום עולם» before the English resumes.",
    "Emoji and graphemes: 👩🏽‍💻 🇹🇭 é (e + combining acute) — each is one caret stop.",
    "> ข้อความอ้างอิงภาษาไทยที่ยาวพอจะตัดบรรทัดได้หลายครั้งเมื่อหน้าต่างแคบลงเพื่อทดสอบการตัดคำ\n\nSee [the capabilities guide](https://formless.athiwat.xyz) for details.",
    "Done. 3 files changed, 2 Tools created, 1 Invocation still pending.",
];

const KINDS: [BlockKind; 4] = [
    BlockKind::User,
    BlockKind::Assistant,
    BlockKind::ToolSummary,
    BlockKind::RunRecap,
];

/// `count` Sessions in daemon order. Every 97th is pinned, every 13th is
/// paused, every 41st is archived, so each sidebar group and the archived
/// filter have members at any realistic size.
pub fn sessions(count: usize) -> Vec<SessionInfo> {
    (0..count)
        .map(|index| SessionInfo {
            id: format!("fixture-session-{index}"),
            forked_from: None,
            fork_point_seq: None,
            name: format!("{} #{index}", TITLES[index % TITLES.len()]),
            model: None,
            reasoning_effort: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            cwd: None,
            message_count: (index % 250) as i64,
            preview: Some(
                BODIES[index % BODIES.len()]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string(),
            ),
            last_run_status: (index % 13 == 5).then(|| "paused".to_string()),
            pinned: index % 97 == 3,
            archived: index % 41 == 40,
            tags: Vec::new(),
            instructions: None,
        })
        .collect()
}

/// The fixture's stand-in for live Run events: every 29th Session is running.
pub fn running(sessions: &[SessionInfo]) -> HashSet<String> {
    sessions
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 29 == 7)
        .map(|(_, session)| session.id.clone())
        .collect()
}

/// `count` mixed timeline blocks with stable, strictly increasing ids.
pub fn timeline(count: usize) -> Vec<TimelineBlock> {
    (0..count)
        .map(|index| {
            TimelineBlock::new(
                index as u64 + 1,
                KINDS[index % KINDS.len()],
                format!("{}\n\n_block {}_", BODIES[index % BODIES.len()], index + 1),
            )
        })
        .collect()
}

/// The text of the `index`th delta of the synthetic stream: a short word in
/// one of three scripts, and a paragraph break every sixtieth delta.
pub fn stream_delta(index: usize) -> &'static str {
    const WORDS: [&str; 8] = [
        "The ",
        "stream ",
        "keeps ",
        "going ",
        "ต่อไป ",
        "続く ",
        "and ",
        "on. ",
    ];
    if index % 60 == 59 {
        "\n\n"
    } else {
        WORDS[index % WORDS.len()]
    }
}

/// The `index`th delta of a synthetic stream of markdown: words with
/// emphasis, inline code and links, paragraph breaks, and every so often an
/// ordered list, a task list, a quote and a fenced code block, so the plain
/// text made for assistive technology has real markdown to work through.
pub fn stream_markdown_delta(index: usize) -> &'static str {
    const PIECES: [&str; 24] = [
        "The ",
        "**stream** ",
        "keeps ",
        "`going` ",
        "ต่อไป ",
        "続く ",
        "and ",
        "[on](https://example.invalid). ",
        "\n\n",
        "1. first\n",
        "2. second\n",
        "\n",
        "- [x] done\n",
        "- [ ] to do\n",
        "\n",
        "> a quoted ",
        "_line_\n",
        "\n",
        "```rust\n",
        "fn main() {}\n",
        "```\n",
        "\n",
        "Emoji 👩🏽‍💻 ",
        "end.\n\n",
    ];
    PIECES[index % PIECES.len()]
}

/// One fenced code block of about `bytes` bytes: the "five-megabyte output"
/// of the epic's stress case. Every line is distinct so a truncated preview
/// can be told from the whole.
pub fn huge_block(id: u64, bytes: usize) -> TimelineBlock {
    let mut markdown = String::with_capacity(bytes + 64);
    markdown.push_str("```text\n");
    let mut line = 0usize;
    while markdown.len() < bytes {
        line += 1;
        markdown.push_str(&format!(
            "{line:08} output line with some payload: 0123456789abcdefghijklmnopqrstuvwxyz\n"
        ));
    }
    markdown.push_str("```\n");
    TimelineBlock::new(id, BlockKind::ToolSummary, markdown)
}

/// Synthetic quote for presentation/component tests, never a daemon reply.
pub fn run_forecast(
    session_id: &str,
    confirmation_required: bool,
) -> formless_protocol::RunForecast {
    use formless_protocol::{ForecastBasis, ForecastRange, RunForecast};
    RunForecast {
        fingerprint: "fixture-fingerprint".into(),
        basis: ForecastBasis {
            provider_key: "fixture-provider".into(),
            model: "fixture-model".into(),
            role: "default".into(),
            reasoning_effort: None,
            session_id: Some(session_id.into()),
            cwd_hash: "fixture-cwd".into(),
            tools_hash: "fixture-tools".into(),
            prompt_tokens: 100,
        },
        comparable_runs: 2,
        comparables: Vec::new(),
        steps: Some(ForecastRange { p50: 1., p90: 3. }),
        tokens: Some(ForecastRange {
            p50: 100.,
            p90: 500.,
        }),
        cost_usd: Some(ForecastRange {
            p50: 0.01,
            p90: 0.05,
        }),
        duration_ms: Some(ForecastRange {
            p50: 2000.,
            p90: 10000.,
        }),
        context_tokens: 8192,
        context_source: "fixture".into(),
        will_compact: false,
        compaction_probability: None,
        cached_fraction: None,
        unpriced: false,
        warnings: Vec::new(),
        confirmation_required,
        confirmation_id: confirmation_required.then(|| "fixture-consent".into()),
        hint: "Synthetic forecast for component tests; not a spending limit.".into(),
    }
}

/// Synthetic structured question for native component/render diagnostics.
pub fn user_question(session: &str) -> formless_protocol::Event {
    use formless_protocol::{Question, QuestionOption};
    formless_protocol::Event::UserQuestion {
        user_only: false,
        topic_id: None,
        attempt_id: None,
        question_id: "native-question".into(),
        session_id: session.into(),
        question: String::new(),
        options: vec![],
        questions: vec![
            Question {
                question: "Choose one ไทย?".into(),
                header: Some("Choice".into()),
                options: vec![
                    QuestionOption {
                        label: "First".into(),
                        description: Some("First choice".into()),
                    },
                    QuestionOption {
                        label: "Second".into(),
                        description: None,
                    },
                ],
                multi_select: false,
            },
            Question {
                question: "Choose any 漢字 😀?".into(),
                header: None,
                options: vec![
                    QuestionOption {
                        label: "Alpha".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Beta".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Gamma".into(),
                        description: None,
                    },
                ],
                multi_select: true,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_deterministic_and_cover_every_sidebar_state() {
        let a = sessions(500);
        assert_eq!(a, sessions(500));
        assert_eq!(a.iter().map(|s| &s.id).collect::<HashSet<_>>().len(), 500);
        assert!(a.iter().any(|s| s.pinned));
        assert!(a.iter().any(|s| s.archived));
        assert!(a
            .iter()
            .any(|s| s.last_run_status.as_deref() == Some("paused")));
        assert!(!running(&a).is_empty());
    }

    #[test]
    fn timeline_ids_are_stable_and_strictly_increasing() {
        let blocks = timeline(1_000);
        assert_eq!(blocks, timeline(1_000));
        assert!(blocks.windows(2).all(|pair| pair[0].id < pair[1].id));
        // A longer transcript keeps the ids of the blocks it already had.
        assert_eq!(timeline(1_200)[..1_000], blocks[..]);
        for kind in KINDS {
            assert!(blocks.iter().any(|block| block.kind == kind));
        }
    }

    #[test]
    fn fixture_text_includes_thai_cjk_emoji_and_right_to_left_scripts() {
        let all: String = TITLES.iter().chain(BODIES.iter()).copied().collect();
        let has = |range: std::ops::RangeInclusive<char>| all.chars().any(|c| range.contains(&c));
        assert!(has('\u{0E00}'..='\u{0E7F}'), "Thai");
        assert!(has('\u{4E00}'..='\u{9FFF}'), "CJK ideographs");
        assert!(has('\u{3040}'..='\u{30FF}'), "Kana");
        assert!(has('\u{AC00}'..='\u{D7AF}'), "Hangul");
        assert!(has('\u{0600}'..='\u{06FF}'), "Arabic");
        assert!(has('\u{0590}'..='\u{05FF}'), "Hebrew");
        assert!(all.contains('\u{200D}'), "ZWJ emoji sequence");
    }
}
