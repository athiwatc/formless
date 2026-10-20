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
    pub fn installed(&self, paths: &Paths) -> bool {
        self.gguf_path(paths).is_file() && self.tokenizer_path(paths).is_file()
    }
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
        &spec.tokenizer_path(paths),
    )
    .await?;
    fetch_verified(&http, spec.gguf_url, spec.gguf_sha256, &spec.gguf_path(paths)).await
}

async fn fetch_verified(
    http: &reqwest::Client,
    url: &str,
    sha256: &str,
    dest: &std::path::Path,
) -> Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    if dest.is_file() {
        return Ok(());
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

/// Title from the first user message alone: its first non-empty line,
/// cut to a few words.
pub fn heuristic_title(user: &str) -> Option<String> {
    let line = user.lines().map(str::trim).find(|l| !l.is_empty())?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut title = words
        .iter()
        .take(HEURISTIC_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if words.len() > HEURISTIC_WORDS {
        title.push('…');
    }
    clean_title(&title)
}

/// Normalise raw model output into a title: first line, no "Title:"
/// prefix, no wrapping quotes/markdown, no trailing punctuation, capped
/// at a word boundary.
pub fn clean_title(raw: &str) -> Option<String> {
    let mut t = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    for prefix in ["title:", "conversation title:"] {
        if t.len() >= prefix.len() && t[..prefix.len()].eq_ignore_ascii_case(prefix) {
            t = t[prefix.len()..].trim();
        }
    }
    const WRAP: &[char] = &['"', '\'', '`', '*', '#', '“', '”', '‘', '’', '「', '」'];
    const TRAILING: &[char] = &['.', '!', '?', ':', ';', ',', '。'];
    let t = t
        .trim_start_matches(|c: char| WRAP.contains(&c) || c.is_whitespace())
        .trim_end_matches(|c: char| {
            WRAP.contains(&c) || TRAILING.contains(&c) || c.is_whitespace()
        });
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return None;
    }
    if t.chars().count() <= MAX_TITLE_CHARS {
        return Some(t);
    }
    let cut: String = t.chars().take(MAX_TITLE_CHARS).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > MAX_TITLE_CHARS / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    Some(format!("{}…", cut.trim_end()))
}

fn truncate_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

const SYSTEM: &str = "You write short titles for chat conversations. Reply with only the title: 2 to 6 words, no quotes, no trailing punctuation.";
const MAX_PROMPT_CHARS: usize = 1200;
const MAX_NEW_TOKENS: usize = 24;

fn prompt(spec: &ModelSpec, user: &str) -> String {
    format!(
        "{}<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\nConversation:\n{}\n\nTitle:<|im_end|>\n<|im_start|>assistant\n",
        spec.bos,
        truncate_chars(user.trim(), MAX_PROMPT_CHARS)
    )
}

/// Run the model on the first user message. Blocking and CPU-heavy
/// (~0.5-1 s plus model load): call from `spawn_blocking`, never from an
/// async task.
#[cfg(feature = "embedded-llm")]
pub fn llm_title(paths: &Paths, spec: &ModelSpec, user: &str) -> Result<String> {
    use candle_core::{quantized::gguf_file, Device, Tensor};
    use candle_transformers::models::{quantized_lfm2, quantized_qwen2};

    enum Model {
        Qwen2(quantized_qwen2::ModelWeights),
        Lfm2(quantized_lfm2::ModelWeights),
    }
    impl Model {
        fn forward(&mut self, x: &Tensor, pos: usize) -> candle_core::Result<Tensor> {
            match self {
                Model::Qwen2(m) => m.forward(x, pos),
                Model::Lfm2(m) => m.forward(x, pos),
            }
        }
    }

    let dev = Device::Cpu;
    let mut file = std::fs::File::open(spec.gguf_path(paths))?;
    let content = gguf_file::Content::read(&mut file)?;
    let mut model = match spec.arch {
        Arch::Qwen2 => Model::Qwen2(quantized_qwen2::ModelWeights::from_gguf(
            content, &mut file, &dev,
        )?),
        Arch::Lfm2 => Model::Lfm2(quantized_lfm2::ModelWeights::from_gguf(
            content, &mut file, &dev,
        )?),
    };
    let tok = tokenizers::Tokenizer::from_file(spec.tokenizer_path(paths))
        .map_err(anyhow::Error::msg)?;
    let stops: Vec<u32> = spec.stop.iter().filter_map(|s| tok.token_to_id(s)).collect();
    let ids = tok
        .encode(prompt(spec, user), false)
        .map_err(anyhow::Error::msg)?
        .get_ids()
        .to_vec();

    let mut out = Vec::new();
    let mut input = Tensor::new(ids.as_slice(), &dev)?.unsqueeze(0)?;
    let mut pos = 0;
    for _ in 0..MAX_NEW_TOKENS {
        let logits = model.forward(&input, pos)?.squeeze(0)?;
        // Some models return logits for every position, others only the last.
        let logits = if logits.rank() == 2 {
            logits.get(logits.dim(0)? - 1)?
        } else {
            logits
        };
        let next = logits.argmax(0)?.to_scalar::<u32>()?;
        pos += input.dim(1)?;
        if stops.contains(&next) {
            break;
        }
        out.push(next);
        // Titles are one line; stop as soon as the model starts another.
        if tok.decode(&out, true).map_err(anyhow::Error::msg)?.contains('\n') {
            break;
        }
        input = Tensor::new(&[next], &dev)?.unsqueeze(0)?;
    }
    tok.decode(&out, true).map_err(anyhow::Error::msg)
}

#[cfg(not(feature = "embedded-llm"))]
pub fn llm_title(_paths: &Paths, _spec: &ModelSpec, _user: &str) -> Result<String> {
    bail!("this build has no embedded model support (feature `embedded-llm`)")
}

/// Best available title: the embedded model when installed, else the
/// heuristic. Blocking; see [`llm_title`].
pub fn title(paths: &Paths, spec: Option<&ModelSpec>, user: &str) -> Option<String> {
    if let Some(spec) = spec.filter(|s| embedded_available() && s.installed(paths)) {
        match llm_title(paths, spec, user) {
            Ok(raw) => {
                if let Some(t) = clean_title(&raw) {
                    return Some(t);
                }
            }
            Err(e) => tracing::warn!("session naming model failed: {e:#}"),
        }
    }
    heuristic_title(user)
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
    fn clean_title_caps_length_on_char_boundaries() {
        let long = "ทดสอบ ".repeat(30);
        let t = clean_title(&long).unwrap();
        assert!(t.chars().count() <= MAX_TITLE_CHARS + 1);
        assert!(t.ends_with('…'));
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
}
