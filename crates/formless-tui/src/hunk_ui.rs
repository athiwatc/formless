//! Client state for server-owned reviews; no file bytes or inverse patches are
//! constructed here. Tool edits remain local drafts until ordinary Ctrl+S.
use crate::input::InputLine;
use formless_protocol::HunkReview;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    Run {
        session_id: String,
        run_id: Option<String>,
    },
    Tool {
        session_id: String,
        tool_id: String,
        base_version: i64,
        original_hash: String,
        current_hash: String,
        editor_generation: u64,
    },
}

impl Context {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Run { session_id, .. } | Self::Tool { session_id, .. } => session_id,
        }
    }
    pub fn matches(&self, review: &HunkReview, allow_changed_draft: bool) -> bool {
        if review.review_id.is_empty() || review.session_id != self.session_id() {
            return false;
        }
        if !chrono::DateTime::parse_from_rfc3339(&review.expires_at)
            .is_ok_and(|expiry| expiry.with_timezone(&chrono::Utc) > chrono::Utc::now())
        {
            return false;
        }
        let mut ids = std::collections::HashSet::new();
        if review.files.iter().any(|file| {
            file.id.is_empty() || !ids.insert(file.id.as_str()) || {
                let mut hunks = std::collections::HashSet::new();
                file.hunks
                    .iter()
                    .any(|hunk| hunk.id.is_empty() || !hunks.insert(hunk.id.as_str()))
            }
        }) {
            return false;
        }
        match self {
            Self::Run { run_id, .. } => {
                review.tool_id.is_none()
                    && review.run_id.as_ref().is_some_and(|id| {
                        !id.is_empty() && run_id.as_ref().is_none_or(|expected| expected == id)
                    })
                    && review.base_version.is_none()
                    && review.original_candidate_hash.is_none()
                    && review.candidate_hash.is_none()
                    && review.draft_code.is_none()
            }
            Self::Tool {
                tool_id,
                base_version,
                original_hash,
                current_hash,
                ..
            } => {
                review.run_id.is_none()
                    && review.tool_id.as_ref() == Some(tool_id)
                    && review.base_version == Some(*base_version)
                    && review.original_candidate_hash.as_ref() == Some(original_hash)
                    && review.draft_code.as_ref().is_some_and(|draft| {
                        let hash = formless_core::hunk_review::image_hash(Some(draft.as_bytes()));
                        review.candidate_hash.as_ref() == Some(&hash)
                            && (allow_changed_draft || &hash == current_hash)
                            && review.files.len() == 1
                            && review.files[0].after_hash == hash
                    })
            }
        }
    }
}

#[derive(Default)]
pub struct State {
    pub generation: u64,
    pub context: Option<Context>,
    pub document: Option<HunkReview>,
    pub focus: usize,
    pub scroll: usize,
    pub left: usize,
    pub comment: Option<InputLine>,
    pub busy: bool,
    pub error: Option<String>,
}

/// Immutable ownership of one physical request. Reply order, popup visibility
/// and a newer editor cannot turn an old review into a write to another owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub generation: u64,
    pub context: Context,
    pub review_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Inspect,
    Decision,
    Comment,
}
impl State {
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.context = None;
        self.document = None;
        self.focus = 0;
        self.scroll = 0;
        self.left = 0;
        self.comment = None;
        self.busy = false;
        self.error = None;
    }
    pub fn replace(&mut self, document: HunkReview) {
        let prior = self.document.as_ref().and_then(|old| {
            let pos = crate::hunk_view::positions(&old.files)
                .get(self.focus)
                .copied()?;
            Some((
                old.files[pos.file].id.clone(),
                old.files[pos.file].hunks[pos.hunk].id.clone(),
            ))
        });
        let positions = crate::hunk_view::positions(&document.files);
        self.focus = prior
            .and_then(|(file, hunk)| {
                positions.iter().position(|pos| {
                    document.files[pos.file].id == file
                        && document.files[pos.file].hunks[pos.hunk].id == hunk
                })
            })
            .unwrap_or_else(|| self.focus.min(positions.len().saturating_sub(1)));
        self.document = Some(document);
        self.scroll = 0;
        self.left = 0;
        self.comment = None;
        self.busy = false;
        self.error = None;
    }
}
