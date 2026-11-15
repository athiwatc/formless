//! Session auto-naming: a small LLM embedded in the daemon titles an
//! "Untitled" Session after its first Run. It runs in-process on the CPU
//! (candle, quantized GGUF), so naming never spends provider tokens or
//! needs the network once the model is installed.
//!
//! The weights are too large to ship in the binary; they are fetched once
//! into `<data_dir>/models/` (pinned revision, SHA-256 verified). Until
//! they are present, or when inference fails, a heuristic title taken
//! from the first user message is used instead.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use unicode_segmentation::UnicodeSegmentation;

use crate::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Qwen2,
    Lfm2,
}

/// A pinned, downloadable naming model.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    /// Config name (`naming.model`).
    pub id: &'static str,
    pub arch: Arch,
    pub gguf_url: &'static str,
    pub gguf_sha256: &'static str,
    pub gguf_bytes: u64,
    pub tokenizer_url: &'static str,
    pub tokenizer_sha256: &'static str,
    /// Prepended to the prompt when the chat template starts with BOS.
    pub bos: &'static str,
    pub stop: &'static [&'static str],
}

impl ModelSpec {
    fn file_stem(&self) -> String {
        self.id.replace('.', "_")
    }
    pub fn gguf_path(&self, paths: &Paths) -> PathBuf {
        models_dir(paths).join(format!("{}.gguf", self.file_stem()))
    }
    pub fn tokenizer_path(&self, paths: &Paths) -> PathBuf {
        models_dir(paths).join(format!("{}.tokenizer.json", self.file_stem()))
    }
    /// Both files present, the weights at their pinned size (a truncated
    /// file counts as missing, so it is fetched again).
    pub fn installed(&self, paths: &Paths) -> bool {
        std::fs::metadata(self.gguf_path(paths)).is_ok_and(|m| m.len() == self.gguf_bytes)
            && self.tokenizer_path(paths).is_file()
    }
}

/// Delete a spec's files after they failed to load, once per process, so
/// the next Session downloads them again instead of failing forever.
#[cfg_attr(not(feature = "embedded-llm"), allow(dead_code))]
fn discard_once(paths: &Paths, spec: &ModelSpec) {
    static DISCARDED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut done = DISCARDED.lock().unwrap_or_else(|e| e.into_inner());
    if done.contains(&spec.id) {
        return;
    }
    done.push(spec.id);
    tracing::warn!(
        "session-naming model {} would not load; downloading it again",
        spec.id
    );
    let _ = std::fs::remove_file(spec.gguf_path(paths));
    let _ = std::fs::remove_file(spec.tokenizer_path(paths));
}

/// Default: Qwen2.5-0.5B-Instruct (Apache-2.0). It titles better than
/// smaller models and handles non-English conversations.
pub const QWEN2_5_0_5B: ModelSpec = ModelSpec {
    id: "qwen2.5-0.5b",
    arch: Arch::Qwen2,
    gguf_url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_0.gguf",
    gguf_sha256: "7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed",
    gguf_bytes: 428_730_208,
    tokenizer_url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct/resolve/7ae557604adf67be50417f59c2c2f167def9a775/tokenizer.json",
    tokenizer_sha256: "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539",
    bos: "",
    stop: &["<|im_end|>", "<|endoftext|>"],
};

/// Smaller and faster alternative: LFM2.5-350M (LFM Open License v1.0,
/// which restricts some commercial use).
pub const LFM2_5_350M: ModelSpec = ModelSpec {
    id: "lfm2.5-350m",
    arch: Arch::Lfm2,
    gguf_url: "https://huggingface.co/LiquidAI/LFM2.5-350M-GGUF/resolve/657e078c94084481950a2d555a941481f715536b/LFM2.5-350M-Q4_K_M.gguf",
    gguf_sha256: "7e6f72643caafc9a68256686638c4d7916f2cec76d1df478d4c3ddcd95a6aed4",
    gguf_bytes: 229_312_224,
    tokenizer_url: "https://huggingface.co/LiquidAI/LFM2.5-350M/resolve/9e6c6ccf47cd318696e137d381a7ded8fe4df09f/tokenizer.json",
    tokenizer_sha256: "df1d8d5ec5d091b460562ffd545e4a5e91d17d4a0db7ebe733be34ed374377bd",
    bos: "<|startoftext|>",
    stop: &["<|im_end|>", "<|endoftext|>"],
};

pub const MODELS: &[ModelSpec] = &[QWEN2_5_0_5B, LFM2_5_350M];

pub fn spec(id: &str) -> Option<ModelSpec> {
    MODELS
        .iter()
        .copied()
        .find(|m| m.id.eq_ignore_ascii_case(id.trim()))
}

pub fn models_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("models")
}

/// Whether this build can run a naming model at all.
pub const fn embedded_available() -> bool {
    cfg!(feature = "embedded-llm")
}

/// Fetch the model + tokenizer if missing. Each file is streamed to a
/// `.part` file, SHA-256 verified, then renamed into place, so a crash
/// mid-download never leaves a file that looks installed.
pub async fn download(paths: &Paths, spec: &ModelSpec) -> Result<()> {
    std::fs::create_dir_all(models_dir(paths))?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()?;
    fetch_verified(
        &http,
        spec.tokenizer_url,
        spec.tokenizer_sha256,
        None,
        &spec.tokenizer_path(paths),
    )
    .await?;
    fetch_verified(
        &http,
        spec.gguf_url,
        spec.gguf_sha256,
        Some(spec.gguf_bytes),
        &spec.gguf_path(paths),
    )
    .await
}

async fn fetch_verified(
    http: &reqwest::Client,
    url: &str,
    sha256: &str,
    bytes: Option<u64>,
    dest: &std::path::Path,
) -> Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    match std::fs::metadata(dest) {
        Ok(m) if bytes.is_none_or(|b| m.len() == b) => return Ok(()),
        Ok(_) => std::fs::remove_file(dest)?,
        Err(_) => {}
    }
    let part = dest.with_extension("part");
    let resp = http.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = Sha256::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.with_context(|| format!("download {url}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    let got = format!("{:x}", hasher.finalize());
    if got != sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("checksum mismatch for {url}: expected {sha256}, got {got}");
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

const MAX_TITLE_CHARS: usize = 60;
const HEURISTIC_WORDS: usize = 6;
/// Title budget, in grapheme clusters, for text written without spaces
/// between words (Thai, Chinese, Japanese) (#872).
const MAX_UNSPACED_GRAPHEMES: usize = 24;

/// Where a title came from: provisional `Heuristic` titles are replaced
/// once the naming model can do better (#871).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    Heuristic,
    Model,
}

impl TitleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleSource::Heuristic => "heuristic",
            TitleSource::Model => "model",
        }
    }
}

/// Whether `s` is mostly written without spaces: few spaces for its
/// length, as in Thai or CJK.
fn unspaced(s: &str) -> bool {
    let graphemes = s.graphemes(true).count();
    let spaces = s.chars().filter(|c| c.is_whitespace()).count();
    graphemes > 12 && spaces * 12 < graphemes
}

/// The first `n` grapheme clusters of `s`, so a base letter never loses
/// its tone mark or vowel sign.
fn take_graphemes(s: &str, n: usize) -> &str {
    match s.grapheme_indices(true).nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Title from a user message alone: its first non-empty line, cut to a
/// few words, or to a few characters when the line has no spaces.
pub fn heuristic_title(user: &str) -> Option<String> {
    let line = user.lines().map(str::trim).find(|l| !l.is_empty())?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut title = words
        .iter()
        .take(HEURISTIC_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let mut cut = words.len() > HEURISTIC_WORDS;
    if unspaced(&title) && title.graphemes(true).count() > MAX_UNSPACED_GRAPHEMES {
        title = take_graphemes(&title, MAX_UNSPACED_GRAPHEMES)
            .trim_end()
            .to_string();
        cut = true;
    }
    if cut {
        title.push('…');
    }
    clean_title(&title)
}

/// Recognized small-talk openers only. An uncertain short message may carry
/// the whole task in any script, so length is not evidence of triviality.
fn trivial(message: &str) -> bool {
    let t = message.trim();
    let opener = t.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    opener.is_empty()
        || (!t.contains('\n')
            && ["hi", "hello", "hey", "thanks", "thank you", "ok", "okay"]
                .iter()
                .any(|greeting| opener.eq_ignore_ascii_case(greeting)))
}

/// `text` is already trimmed. A long message keeps the head and the tail
/// (one third for the tail, with the separator included in the budget).
fn shape_naming_text(text: &str) -> String {
    if text.chars().count() <= MAX_PROMPT_CHARS {
        return text.to_string();
    }
    let separator = "\n…\n";
    let tail_chars = MAX_PROMPT_CHARS / 3;
    let head = truncate_chars(
        text,
        MAX_PROMPT_CHARS - tail_chars - separator.chars().count(),
    );
    let skip = text.chars().count() - tail_chars;
    let tail_at = text.char_indices().nth(skip).map_or(text.len(), |(i, _)| i);
    format!("{head}{separator}{}", &text[tail_at..])
}

/// What to name a Session from (#871): its first substantive user message
/// (greetings and other trivial openers are skipped when something follows
/// them), with its head and its tail when it is long, since the task often
/// comes after pasted context.
pub fn naming_text(user_messages: &[String]) -> Option<String> {
    let mut msgs = user_messages
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty());
    let first = msgs.clone().find(|m| !trivial(m));
    let text = first.or_else(|| msgs.next())?;
    Some(shape_naming_text(text))
}

/// Oldest-first user contents. Keeps the first nonempty trimmed row as the
/// fallback and the current row, and stops at the first nontrivial row.
/// Empty and trivial follow [`naming_text`]: Unicode trim, then conservative
/// recognized openers rather than a language-dependent length threshold.
pub(crate) fn try_naming_text_from_contents<I, S, E>(
    contents: I,
) -> std::result::Result<Option<String>, E>
where
    I: IntoIterator<Item = std::result::Result<S, E>>,
    S: AsRef<str>,
{
    let mut fallback: Option<String> = None;
    for content in contents {
        let content = content?;
        let trimmed = content.as_ref().trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trivial(trimmed) {
            return Ok(Some(shape_naming_text(trimmed)));
        }
        if fallback.is_none() {
            fallback = Some(trimmed.to_string());
        }
    }
    Ok(fallback.as_deref().map(shape_naming_text))
}

/// Normalise raw model output into a title: first line, no "Title:"
/// prefix, no wrapping quotes/markdown, no trailing punctuation, capped
/// at a word boundary.
pub fn clean_title(raw: &str) -> Option<String> {
    let mut t = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    for prefix in ["title:", "conversation title:"] {
        // `get`: a non-ASCII title has no char boundary there.
        if t.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
        {
            t = t[prefix.len()..].trim();
        }
    }
    const TRAILING: &[char] = &['.', '!', '?', ':', ';', ',', '。'];
    loop {
        let before = t;
        t = t.trim_end_matches(TRAILING).trim();
        // A heading marker needs separating whitespace: #1234 is text,
        // and a trailing # (C#, F#) is never a wrapper.
        let heading = t.trim_start_matches('#');
        if heading.starts_with(char::is_whitespace) {
            t = heading.trim_start();
        }
        if let Some(inner) = strip_title_wrapper(t) {
            t = inner.trim();
        }
        if t == before {
            break;
        }
    }
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return None;
    }
    // Unspaced scripts get a character budget, cut between graphemes;
    // spaced text is cut at a word boundary (#872).
    if unspaced(&t) {
        let body = t.trim_end_matches('…');
        if body.graphemes(true).count() <= MAX_UNSPACED_GRAPHEMES {
            return Some(t);
        }
        return Some(format!(
            "{}…",
            take_graphemes(body, MAX_UNSPACED_GRAPHEMES).trim_end()
        ));
    }
    if t.graphemes(true).count() <= MAX_TITLE_CHARS {
        return Some(t);
    }
    let cut = take_graphemes(&t, MAX_TITLE_CHARS);
    let cut = match cut.rfind(' ') {
        Some(i) if cut[..i].graphemes(true).count() > MAX_TITLE_CHARS / 2 => &cut[..i],
        _ => cut,
    };
    Some(format!("{}…", cut.trim_end()))
}

/// Strip only an enclosing pair, never independent punctuation at either end.
fn strip_title_wrapper(t: &str) -> Option<&str> {
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('“', '”'),
        ('‘', '’'),
        ('「', '」'),
    ] {
        if let Some(inner) = t.strip_prefix(open).and_then(|s| s.strip_suffix(close)) {
            // A prior closer means these are separate quoted phrases,
            // not one enclosing quote around the entire title.
            if !inner.contains(close) {
                return Some(inner);
            }
        }
    }
    for marker in ['*', '`'] {
        let leading = t.chars().take_while(|c| *c == marker).count();
        let trailing = t.chars().rev().take_while(|c| *c == marker).count();
        // Both markers are ASCII; these offsets are UTF-8 boundaries.
        if leading > 0 && leading == trailing {
            if leading * 2 <= t.len() {
                let inner = &t[leading..t.len() - trailing];
                // An equal-width delimiter inside closes the first span
                // before the title ends (`foo` and `bar`, for example).
                if !inner.split(|c| c != marker).any(|run| run.len() == leading) {
                    return Some(inner);
                }
            }
            if leading == t.len() && leading % 2 == 0 {
                return Some("");
            }
        }
    }
    None
}

fn truncate_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

const SYSTEM: &str = "You write short titles for chat conversations. Reply with only the title, a short phrase in the same language as the conversation (a few words; for Thai, Chinese or Japanese a short phrase without spaces), no quotes, no trailing punctuation.";
const MAX_PROMPT_CHARS: usize = 1200;
const MAX_NEW_TOKENS: usize = 24;

fn prompt(spec: &ModelSpec, user: &str) -> String {
    format!(
        "{}<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\nConversation:\n{}\n\nTitle:<|im_end|>\n<|im_start|>assistant\n",
        spec.bos,
        truncate_chars(user.trim(), MAX_PROMPT_CHARS)
    )
}

#[cfg(feature = "embedded-llm")]
mod embedded {
    use super::{prompt, Arch, ModelSpec, MAX_NEW_TOKENS};
    use crate::Paths;
    use anyhow::Result;
    use candle_core::{quantized::gguf_file, Device, Tensor};
    use candle_transformers::models::{quantized_lfm2, quantized_qwen2};

    enum Weights {
        Qwen2(quantized_qwen2::ModelWeights),
        Lfm2(quantized_lfm2::ModelWeights),
    }
    impl Weights {
        fn forward(&mut self, x: &Tensor, pos: usize) -> candle_core::Result<Tensor> {
            match self {
                Weights::Qwen2(m) => m.forward(x, pos),
                Weights::Lfm2(m) => m.forward(x, pos),
            }
        }
    }

    /// Weights and tokenizer, loaded once per spec and reused: opening the
    /// GGUF costs more than titling itself.
    pub(super) struct Loaded {
        id: &'static str,
        model: Weights,
        tok: tokenizers::Tokenizer,
        stops: Vec<u32>,
    }

    pub(super) fn load(paths: &Paths, spec: &ModelSpec) -> Result<Loaded> {
        let dev = Device::Cpu;
        let mut file = std::fs::File::open(spec.gguf_path(paths))?;
        let content = gguf_file::Content::read(&mut file)?;
        let model = match spec.arch {
            Arch::Qwen2 => Weights::Qwen2(quantized_qwen2::ModelWeights::from_gguf(
                content, &mut file, &dev,
            )?),
            Arch::Lfm2 => Weights::Lfm2(quantized_lfm2::ModelWeights::from_gguf(
                content, &mut file, &dev,
            )?),
        };
        let tok = tokenizers::Tokenizer::from_file(spec.tokenizer_path(paths))
            .map_err(anyhow::Error::msg)?;
        let stops = spec
            .stop
            .iter()
            .filter_map(|s| tok.token_to_id(s))
            .collect();
        Ok(Loaded {
            id: spec.id,
            model,
            tok,
            stops,
        })
    }

    pub(super) static CACHE: std::sync::Mutex<Option<Loaded>> = std::sync::Mutex::new(None);

    pub(super) fn cached_id(cache: &Option<Loaded>) -> Option<&'static str> {
        cache.as_ref().map(|l| l.id)
    }

    pub(super) fn generate(l: &mut Loaded, spec: &ModelSpec, user: &str) -> Result<String> {
        let dev = Device::Cpu;
        let ids = l
            .tok
            .encode(prompt(spec, user), false)
            .map_err(anyhow::Error::msg)?
            .get_ids()
            .to_vec();
        let mut out = Vec::new();
        let mut input = Tensor::new(ids.as_slice(), &dev)?.unsqueeze(0)?;
        // Position 0 resets the model's KV cache from the previous title.
        let mut pos = 0;
        for _ in 0..MAX_NEW_TOKENS {
            let logits = l.model.forward(&input, pos)?.squeeze(0)?;
            // Some models return logits for every position, others only the last.
            let logits = if logits.rank() == 2 {
                logits.get(logits.dim(0)? - 1)?
            } else {
                logits
            };
            let next = logits.argmax(0)?.to_scalar::<u32>()?;
            pos += input.dim(1)?;
            if l.stops.contains(&next) {
                break;
            }
            out.push(next);
            // Titles are one line; stop as soon as the model starts another.
            if l.tok
                .decode(&[next], true)
                .map_err(anyhow::Error::msg)?
                .contains('\n')
            {
                break;
            }
            input = Tensor::new(&[next], &dev)?.unsqueeze(0)?;
        }
        l.tok.decode(&out, true).map_err(anyhow::Error::msg)
    }
}

/// Run the model on the first user message. Blocking and CPU-heavy
/// (~0.5-1 s, plus loading the model the first time): call from
/// `spawn_blocking`, never from an async task.
#[cfg(feature = "embedded-llm")]
pub fn llm_title(paths: &Paths, spec: &ModelSpec, user: &str) -> Result<String> {
    let mut cache = embedded::CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if embedded::cached_id(&cache) != Some(spec.id) {
        *cache = None;
        match embedded::load(paths, spec) {
            Ok(l) => *cache = Some(l),
            Err(e) => {
                discard_once(paths, spec);
                return Err(e.context("load the naming model"));
            }
        }
    }
    let loaded = cache.as_mut().expect("loaded above");
    embedded::generate(loaded, spec, user)
}

#[cfg(not(feature = "embedded-llm"))]
pub fn llm_title(_paths: &Paths, _spec: &ModelSpec, _user: &str) -> Result<String> {
    bail!("this build has no embedded model support (feature `embedded-llm`)")
}

/// Whether the naming model can run now.
pub fn model_ready(paths: &Paths, spec: Option<&ModelSpec>) -> bool {
    spec.is_some_and(|s| embedded_available() && s.installed(paths))
}

/// Best available title and where it came from: the embedded model when
/// installed, else the heuristic. Blocking; see [`llm_title`].
pub fn title(paths: &Paths, spec: Option<&ModelSpec>, user: &str) -> Option<(String, TitleSource)> {
    if let Some(spec) = spec.filter(|s| model_ready(paths, Some(s))) {
        match llm_title(paths, spec, user) {
            Ok(raw) => {
                if let Some(t) = clean_title(&raw) {
                    return Some((t, TitleSource::Model));
                }
            }
            Err(e) => tracing::warn!("session naming model failed: {e:#}"),
        }
    }
    heuristic_title(user).map(|t| (t, TitleSource::Heuristic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_strips_wrapping_and_prefixes() {
        assert_eq!(
            clean_title("\"Top 5 Hacker News Stories\"").as_deref(),
            Some("Top 5 Hacker News Stories")
        );
        assert_eq!(clean_title("Title: \"Hi\"").as_deref(), Some("Hi"));
        assert_eq!(
            clean_title("**CSV to JSON**.\nmore").as_deref(),
            Some("CSV to JSON")
        );
        assert_eq!(clean_title("  \n \"\" "), None);
    }

    #[test]
    fn title_cleanup_preserves_unpaired_punctuation_in_both_paths() {
        for title in [
            "Port this to C#",
            "Port this to F#",
            "#1234 is flaky again",
            "Explain `useEffect`",
            "**kwargs vs *args",
            "'til the build passes",
            "A trailing apostrophe'",
            "Unmatched quote\"",
            "“Unclosed quote",
            "Wrong quote”",
            "**Unequal stars*",
            "``Unequal backticks`",
            "#No heading",
            "Keep trailing #",
            "`foo` and `bar`",
            "*foo* and *bar*",
            "**foo** and **bar**",
            "\"foo\" and \"bar\"",
            "“foo” and “bar”",
            "*",
            "`",
        ] {
            assert_eq!(
                clean_title(title).as_deref(),
                Some(title),
                "model cleanup: {title}"
            );
            assert_eq!(
                heuristic_title(title).as_deref(),
                Some(title),
                "heuristic cleanup: {title}"
            );
        }
    }

    #[test]
    fn title_cleanup_removes_only_balanced_nested_wrappers() {
        for (raw, expected) in [
            ("\"**C#**\".", "C#"),
            ("## **Title**", "Title"),
            ("# Title #", "Title #"),
            ("###\tTitle", "Title"),
            ("*Title*", "Title"),
            ("**Title**", "Title"),
            ("***Title***", "Title"),
            ("`Title`", "Title"),
            ("``Title``", "Title"),
            ("``Title with `code` ``", "Title with `code`"),
            ("**Title with *italic* text**", "Title with *italic* text"),
            ("'Title'", "Title"),
            ("“Title”", "Title"),
            ("‘Title’", "Title"),
            ("「名前」", "名前"),
            ("Title: \"**Hello**\"!", "Hello"),
            ("Conversation title: **Hello**.\nignored", "Hello"),
        ] {
            assert_eq!(clean_title(raw).as_deref(), Some(expected), "{raw}");
        }
        for empty in ["\"\"", "**", "``", "‘’", "「」", "** **"] {
            assert_eq!(clean_title(empty), None, "{empty}");
        }
    }

    #[test]
    fn clean_title_caps_length_on_char_boundaries() {
        let long = "ทดสอบ ".repeat(30);
        let t = clean_title(&long).unwrap();
        assert!(t.chars().count() <= MAX_TITLE_CHARS + 1);
        assert!(t.ends_with('…'));
    }

    /// No cut leaves a Thai base letter without its tone mark or vowel
    /// sign: the kept text is a prefix of whole grapheme clusters.
    fn whole_graphemes(original: &str, title: &str) -> bool {
        let kept = title.trim_end_matches('…');
        let gs: Vec<&str> = original.graphemes(true).collect();
        let mut acc = String::new();
        for g in gs {
            if acc == kept {
                return true;
            }
            acc.push_str(g);
        }
        acc == kept
    }

    #[test]
    fn unspaced_thai_and_cjk_titles_are_cut_between_graphemes() {
        // Realistic unspaced Thai: tone marks and above/below vowels.
        let thai = "ช่วยเขียนสคริปต์ที่แปลงไฟล์ซีเอสวีเป็นเจสันให้หน่อยได้ไหมครับ แล้วก็อธิบายด้วย";
        let h = heuristic_title(thai).unwrap();
        assert!(h.ends_with('…'), "{h}");
        assert!(
            h.graphemes(true).count() <= MAX_UNSPACED_GRAPHEMES + 1,
            "{h}"
        );
        assert!(whole_graphemes(thai, &h), "{h}");
        assert_ne!(h, thai, "not the whole first line");

        let ja = "このリポジトリのビルドが失敗する原因を調べて修正してください。テストも追加して";
        let h = heuristic_title(ja).unwrap();
        assert!(
            h.graphemes(true).count() <= MAX_UNSPACED_GRAPHEMES + 1,
            "{h}"
        );
        assert!(whole_graphemes(ja, &h), "{h}");

        // Model output in Thai that runs long is cut the same way.
        let c = clean_title("สรุปข่าวเทคโนโลยีประจำสัปดาห์นี้แบบละเอียดมากมายหลายหัวข้อ").unwrap();
        assert!(
            c.graphemes(true).count() <= MAX_UNSPACED_GRAPHEMES + 1,
            "{c}"
        );
        assert!(
            whole_graphemes("สรุปข่าวเทคโนโลยีประจำสัปดาห์นี้แบบละเอียดมากมายหลายหัวข้อ", &c),
            "{c}"
        );
        // A short one is kept whole.
        assert_eq!(clean_title("แปลงไฟล์ CSV").as_deref(), Some("แปลงไฟล์ CSV"));
        // Mixed Thai-English keeps word cutting.
        assert_eq!(
            heuristic_title("ช่วย debug โค้ด Rust ตัวนี้ ที่ compile ไม่ผ่าน").as_deref(),
            Some("ช่วย debug โค้ด Rust ตัวนี้ ที่…")
        );
    }

    #[test]
    fn naming_retains_short_requests_and_skips_only_recognized_openers() {
        let log = format!("{}\nPrivate log tail", "long pasted context\n".repeat(200));
        for request in [
            "Fix the CI",
            "Add tests",
            "修复登录错误",
            "แก้บั๊ก",
            "🦀",
            "你好",
            "hi there, fix CI",
            "No",
            "e\u{301}",
        ] {
            let messages = vec!["".into(), format!("  {request}  "), log.clone()];
            assert_eq!(naming_text(&messages).as_deref(), Some(request));
            let selected =
                try_naming_text_from_contents(messages.iter().map(|s| Ok::<_, ()>(s.as_str())))
                    .unwrap();
            assert_eq!(selected.as_deref(), Some(request));
        }
        for opener in ["hi", "Hello?", "HEY!", "ok", "Thank you.", "!!!", "  ...  "] {
            let messages = vec![opener.into(), "convert this csv to json please".into()];
            assert_eq!(
                naming_text(&messages).as_deref(),
                Some("convert this csv to json please")
            );
        }
        assert_eq!(naming_text(&[]), None);
        assert_eq!(naming_text(&["".into(), " \n ".into()]), None);
        assert_eq!(
            naming_text(&["hi".into(), "okay".into(), "!!!".into()]).as_deref(),
            Some("hi")
        );
        assert_eq!(
            naming_text(&["".into(), "!!!".into(), "hi".into()]).as_deref(),
            Some("!!!")
        );
        let selected = naming_text(&["hi".into(), log.clone()]).unwrap();
        assert_eq!(selected, shape_naming_text(&log));
        assert!(selected.ends_with("Private log tail"));
        assert!(selected.chars().count() <= MAX_PROMPT_CHARS);
    }

    #[test]
    fn shaped_naming_tail_reaches_every_supported_prompt_intact() {
        let ending = "Now: summarise the bug above ไทย 日本語 🦀";
        let long = format!("{}\n{ending}", "context 日本語\n".repeat(200));
        let shaped = naming_text(&[long]).unwrap();
        assert!(shaped.chars().count() <= MAX_PROMPT_CHARS);
        for spec in MODELS {
            assert!(prompt(spec, &shaped).contains(ending), "{}", spec.id);
        }
        for length in [1, MAX_PROMPT_CHARS - 1, MAX_PROMPT_CHARS] {
            let short = "日".repeat(length);
            let shaped = naming_text(std::slice::from_ref(&short)).unwrap();
            assert_eq!(shaped, short);
            assert_eq!(
                prompt(&QWEN2_5_0_5B, &shaped),
                prompt(&QWEN2_5_0_5B, &short)
            );
        }
    }

    #[test]
    fn naming_skips_trivial_openers_and_keeps_the_tail_of_long_messages() {
        let msgs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            naming_text(&msgs(&["hi", "convert this csv to json please"])).as_deref(),
            Some("convert this csv to json please")
        );
        assert_eq!(naming_text(&msgs(&["hi"])).as_deref(), Some("hi"));
        let long = format!(
            "{}\nNow: summarise the bug above",
            "context line\n".repeat(200)
        );
        let t = naming_text(&msgs(&[&long])).unwrap();
        assert!(t.starts_with("context line"));
        assert!(
            t.ends_with("Now: summarise the bug above"),
            "the tail is kept"
        );
        assert!(t.chars().count() <= MAX_PROMPT_CHARS);
    }

    #[test]
    fn heuristic_uses_first_line_and_few_words() {
        assert_eq!(
            heuristic_title("\n  can you write me a tool that converts csv\nsecond").as_deref(),
            Some("can you write me a tool…")
        );
        assert_eq!(heuristic_title("hi").as_deref(), Some("hi"));
        assert_eq!(heuristic_title("   \n"), None);
    }

    #[test]
    fn specs_resolve_by_id() {
        assert_eq!(spec("Qwen2.5-0.5B").unwrap().arch, Arch::Qwen2);
        assert_eq!(spec("lfm2.5-350m").unwrap().arch, Arch::Lfm2);
        assert!(spec("nope").is_none());
    }

    #[tokio::test]
    async fn a_truncated_model_counts_as_missing_and_is_fetched_again() {
        let weights: &'static [u8] = b"full weights";
        let app = axum::Router::new()
            .route("/w", axum::routing::get(move || async move { weights }))
            .route("/t", axum::routing::get(|| async { "{}" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let spec = ModelSpec {
            id: "test-model",
            gguf_url: leak(format!("{base}/w")),
            gguf_sha256: leak(format!("{:x}", Sha256::digest(weights))),
            gguf_bytes: weights.len() as u64,
            tokenizer_url: leak(format!("{base}/t")),
            tokenizer_sha256: leak(format!("{:x}", Sha256::digest(b"{}"))),
            ..QWEN2_5_0_5B
        };
        let dir = std::env::temp_dir().join(format!("fl-namer-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            data_dir: dir.join("d"),
            config_dir: dir.join("c"),
            profile: "t".into(),
        };
        std::fs::create_dir_all(models_dir(&paths)).unwrap();
        std::fs::write(spec.gguf_path(&paths), b"full").unwrap();
        std::fs::write(spec.tokenizer_path(&paths), b"{}").unwrap();
        assert!(!spec.installed(&paths), "a short file is not installed");
        download(&paths, &spec).await.unwrap();
        assert!(spec.installed(&paths));
        assert_eq!(std::fs::read(spec.gguf_path(&paths)).unwrap(), weights);
        let _ = std::fs::remove_dir_all(dir);
    }
}
