//! Bounded Session metadata edits using the daemon's existing setters.
//! These are explicit replacements, not compare-and-swap operations.

use formless_protocol::{Request, SessionInfo};

pub const MAX_INPUT: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instructions {
    Text(String),
    TooLarge(usize),
    Unavailable,
}

impl Instructions {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::TooLarge(_) | Self::Unavailable => None,
        }
    }
}

pub const MAX_MODEL_BYTES: usize = 512;
pub const MAX_EFFORT_BYTES: usize = 64;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}
impl ModelSelection {
    pub fn validate(&self, for_write: bool) -> Result<(), &'static str> {
        for (value, limit) in [
            (&self.model, MAX_MODEL_BYTES),
            (&self.reasoning_effort, MAX_EFFORT_BYTES),
        ] {
            if value.as_ref().is_some_and(|s| s.is_empty() || s.len()>limit || s.trim()!=s || s.chars().any(|c| c.is_control() || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))) {
                return Err("Use a bounded single-line model/Role and effort without control characters; blank inherits defaults.");
            }
        }
        if self
            .model
            .as_deref()
            .is_some_and(|m| m.strip_prefix("role:").is_some_and(|r| r.trim().is_empty()))
        {
            return Err("Enter a Role name after role:.");
        }
        if for_write
            && self
                .model
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("auto"))
        {
            return Err("Choose an explicit model or role:name, or clear the override. Legacy auto may be rewritten by the daemon.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
    pub instructions: Instructions,
    pub selection: Option<ModelSelection>,
}

impl Metadata {
    pub(crate) fn from_session_reply(
        session: &SessionInfo,
        exposes_instructions: bool,
        exposes_selection: bool,
    ) -> Result<Self, &'static str> {
        let mut metadata = Self::from_session(session)?;
        if !exposes_instructions {
            metadata.instructions = Instructions::Unavailable;
        }
        if !exposes_selection {
            metadata.selection = None;
        }
        Ok(metadata)
    }
    pub(crate) fn from_list_reply(id: &str, value: &serde_json::Value) -> Result<Self, String> {
        let mut raw = value
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["id"].as_str() == Some(id));
        let row = raw
            .next()
            .ok_or("This Session is no longer listed. Reload before editing.")?;
        if raw.next().is_some() {
            return Err("Metadata repeats the Session identity; no edit is allowed.".into());
        }
        let instructions = row.get("instructions").is_some();
        let selection = row.get("model").is_some() && row.get("reasoning_effort").is_some();
        let session = crate::replies::session_list(value)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("Session metadata is unavailable")?;
        Self::from_session_reply(&session, instructions, selection).map_err(str::to_string)
    }
    pub fn from_session(session: &SessionInfo) -> Result<Self, &'static str> {
        if session.id.is_empty()
            || session.id.len() > 256
            || session.name.len() > MAX_INPUT
            || session.tags.len() > 128
            || session
                .tags
                .iter()
                .any(|tag| tag.len() > 256 || tag.contains(['\r', '\n']))
            || session.tags.iter().map(|tag| tag.len() + 1).sum::<usize>() > MAX_INPUT
        {
            return Err("Session metadata exceeds this editor's limits; use Formless TUI.");
        }
        Ok(Self {
            id: session.id.clone(),
            name: session.name.clone(),
            tags: session.tags.clone(),
            pinned: session.pinned,
            archived: session.archived,
            selection: if session
                .model
                .as_ref()
                .is_some_and(|m| m.len() > MAX_MODEL_BYTES)
                || session
                    .reasoning_effort
                    .as_ref()
                    .is_some_and(|e| e.len() > MAX_EFFORT_BYTES)
            {
                None
            } else {
                let selection = ModelSelection {
                    model: session.model.clone(),
                    reasoning_effort: session.reasoning_effort.clone(),
                };
                selection.validate(false).is_ok().then_some(selection)
            },
            instructions: match session.instructions.as_deref().unwrap_or_default() {
                text if text.len() <= MAX_INPUT => Instructions::Text(text.into()),
                text => Instructions::TooLarge(text.len()),
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Tags,
    Pin,
    Archive,
    Instructions,
    Model,
    Effort,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Tags => "Tags",
            Self::Pin => "Pin",
            Self::Archive => "Archive",
            Self::Instructions => "Instructions",
            Self::Model => "Model",
            Self::Effort => "Effort",
        }
    }
    pub fn value(self, metadata: &Metadata) -> String {
        match self {
            Self::Model => metadata.selection.as_ref().and_then(|s|s.model.clone()).unwrap_or_default(),
            Self::Effort => metadata.selection.as_ref().and_then(|s|s.reasoning_effort.clone()).unwrap_or_default(),
            Self::Name => metadata.name.clone(),
            Self::Tags => metadata.tags.join("\n"),
            Self::Pin => if metadata.pinned {
                "Pinned"
            } else {
                "Unpinned"
            }
            .into(),
            Self::Archive => if metadata.archived {
                "Archived"
            } else {
                "Unarchived"
            }
            .into(),
            Self::Instructions => metadata
                .instructions
                .text()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "Instructions are unavailable in this editor; use Formless TUI for the complete value.".into()
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Name(String),
    Tags(Vec<String>),
    Pin(bool),
    Archive(bool),
    Instructions(Option<String>),
    Selection {
        expected: ModelSelection,
        desired: ModelSelection,
    },
}

impl Change {
    pub fn from_input(field: Field, text: &str, metadata: &Metadata) -> Result<Self, &'static str> {
        if text.len() > MAX_INPUT {
            return Err("Use at most 16 KiB; no change was sent.");
        }
        let change = match field {
            Field::Model | Field::Effort => {
                let expected=metadata.selection.clone().ok_or("Current model and effort are unavailable; reload or use Formless TUI. No replacement was sent.")?;
                let mut desired = expected.clone();
                let value = text.trim();
                let value = (!value.is_empty()).then(|| value.to_string());
                if field == Field::Model {
                    desired.model = value;
                } else {
                    desired.reasoning_effort = value;
                }
                Self::Selection { expected, desired }
            }
            Field::Name => Self::Name(text.trim().into()),
            Field::Tags => Self::Tags(text.lines().map(str::to_string).collect()),
            Field::Pin => Self::Pin(!metadata.pinned),
            Field::Archive => Self::Archive(!metadata.archived),
            Field::Instructions => {
                if metadata.instructions.text().is_none() {
                    return Err("Current instructions are unavailable in this editor. Use Formless TUI; no replacement was sent.");
                }
                let text = text.trim();
                Self::Instructions((!text.is_empty()).then(|| text.into()))
            }
        };
        change.validate()?;
        Ok(change)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Selection { expected, desired } => {
                expected.validate(false)?;
                desired.validate(true)
            }
            Self::Instructions(Some(text)) if text.len() > MAX_INPUT => {
                Err("Use at most 16 KiB of standing instructions.")
            }
            Self::Name(name)
                if name.is_empty() || name.len() > MAX_INPUT || name.contains(['\r', '\n']) =>
            {
                Err("Use a nonempty, single-line Session name of at most 16 KiB.")
            }
            Self::Tags(tags)
                if tags.len() > 128
                    || tags
                        .iter()
                        .any(|tag| tag.len() > 256 || tag.contains(['\r', '\n']))
                    || tags.iter().map(|tag| tag.len() + 1).sum::<usize>() > MAX_INPUT =>
            {
                Err("Use at most 128 tags, one per line, each at most 256 bytes and 16 KiB total.")
            }
            _ => Ok(()),
        }
    }
    pub fn is_selection(&self) -> bool {
        matches!(self, Self::Selection { .. })
    }
    pub fn request(&self, id: &str) -> Request {
        match self {
            Self::Selection { desired, .. } => Request::SetSessionModel {
                id: id.into(),
                model: desired.model.clone(),
                reasoning_effort: desired.reasoning_effort.clone(),
            },
            Self::Name(name) => Request::RenameSession {
                id: id.into(),
                name: name.clone(),
            },
            Self::Tags(tags) => Request::SetSessionTags {
                id: id.into(),
                tags: tags.clone(),
            },
            Self::Pin(pinned) => Request::SetSessionPinned {
                id: id.into(),
                pinned: *pinned,
            },
            Self::Archive(archived) => Request::SetSessionArchived {
                id: id.into(),
                archived: *archived,
            },
            Self::Instructions(instructions) => Request::SetSessionInstructions {
                id: id.into(),
                instructions: instructions.clone(),
            },
        }
    }
    pub fn matches(&self, metadata: &Metadata) -> bool {
        match self {
            Self::Selection { desired, .. } => metadata.selection.as_ref() == Some(desired),
            Self::Name(name) => name == &metadata.name,
            Self::Pin(pinned) => pinned == &metadata.pinned,
            Self::Archive(archived) => archived == &metadata.archived,
            Self::Instructions(instructions) => metadata
                .instructions
                .text()
                .is_some_and(|text| text == instructions.as_deref().unwrap_or_default().trim()),
            Self::Tags(tags) => {
                // Existing storage setter semantics; the acknowledged values
                // are displayed, never inferred as an optimistic local update.
                let mut normalized: Vec<_> = tags
                    .iter()
                    .map(|tag| tag.trim().trim_start_matches('#').to_lowercase())
                    .filter(|tag| !tag.is_empty())
                    .collect();
                normalized.sort();
                normalized.dedup();
                normalized == metadata.tags
            }
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    Applied(Metadata),
    ObservedSelection(Metadata),
    Rejected(String),
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instructions_use_the_existing_trim_and_clear_contract_without_normalizing_interior_text() {
        let session = crate::fixture::sessions(1).remove(0);
        let mut metadata = Metadata::from_session(&session).unwrap();
        let change = Change::from_input(
            Field::Instructions,
            " \nFirst ไทย\n\tIndented 漢字 😀\n ",
            &metadata,
        )
        .unwrap();
        assert_eq!(
            change,
            Change::Instructions(Some("First ไทย\n\tIndented 漢字 😀".into()))
        );
        assert!(
            matches!(change.request(&metadata.id),Request::SetSessionInstructions{id,instructions:Some(text)} if id==metadata.id && text=="First ไทย\n\tIndented 漢字 😀")
        );
        metadata.instructions = Instructions::Text("First ไทย\n\tIndented 漢字 😀".into());
        assert!(change.matches(&metadata));
        let clear = Change::from_input(Field::Instructions, " \n\t ", &metadata).unwrap();
        assert_eq!(clear, Change::Instructions(None));
        assert!(!clear.matches(&metadata));
        metadata.instructions = Instructions::Text(String::new());
        assert!(clear.matches(&metadata));
        for unsupported in [
            Instructions::Unavailable,
            Instructions::TooLarge(MAX_INPUT + 1),
        ] {
            metadata.instructions = unsupported;
            assert!(!clear.matches(&metadata));
            assert!(
                Change::from_input(Field::Instructions, "small replacement", &metadata).is_err()
            );
        }
    }

    #[test]
    fn unsupported_instructions_never_become_a_truncated_edit_or_block_other_fields() {
        let mut session = crate::fixture::sessions(1).remove(0);
        session.instructions = Some("😀".repeat(MAX_INPUT / 4 + 1));
        let metadata = Metadata::from_session(&session).unwrap();
        assert_eq!(metadata.instructions, Instructions::TooLarge(MAX_INPUT + 4));
        assert!(Change::from_input(Field::Name, "Still editable", &metadata).is_ok());
        assert!(Change::from_input(Field::Instructions, "", &metadata).is_err());
        assert!(Change::Instructions(Some("x".repeat(MAX_INPUT + 1)))
            .validate()
            .is_err());
        let missing = Metadata::from_session_reply(&session, false, true).unwrap();
        assert_eq!(missing.instructions, Instructions::Unavailable);
        assert!(Change::from_input(Field::Tags, "work", &missing).is_ok());
    }
    #[test]
    fn archive_and_restore_are_explicit_boolean_setters() {
        let mut session = crate::fixture::sessions(1).remove(0);
        session.archived = false;
        let mut metadata = Metadata::from_session(&session).unwrap();
        let archive = Change::from_input(Field::Archive, "", &metadata).unwrap();
        assert_eq!(archive, Change::Archive(true));
        assert!(!archive.matches(&metadata));
        assert!(
            matches!(archive.request(&metadata.id),Request::SetSessionArchived {id,archived:true} if id==metadata.id)
        );
        metadata.archived = true;
        assert!(archive.matches(&metadata));
        assert_eq!(
            Change::from_input(Field::Archive, "", &metadata).unwrap(),
            Change::Archive(false)
        );
        assert_eq!(Field::Archive.value(&metadata), "Archived");
    }
    #[test]
    fn edits_are_bounded_and_use_exact_ids_with_existing_tag_normalization() {
        let mut metadata = Metadata {
            id: "original".into(),
            name: "Before".into(),
            tags: vec![],
            pinned: false,
            archived: false,
            instructions: Instructions::Text(String::new()),
            selection: None,
        };
        let change =
            Change::from_input(Field::Tags, "#Work\nwork\n  ไทย 漢字 😀 \n", &metadata).unwrap();
        metadata.tags = vec!["work".into(), "ไทย 漢字 😀".into()];
        assert!(change.matches(&metadata));
        assert!(
            matches!(change.request(&metadata.id), Request::SetSessionTags {id,..} if id=="original")
        );
        assert!(Change::from_input(Field::Name, " \n ", &metadata).is_err());
        assert!(Change::from_input(Field::Name, "two\nlines", &metadata).is_err());
        assert!(Change::from_input(Field::Tags, &"x".repeat(257), &metadata).is_err());
        assert!(Change::from_input(Field::Tags, &"a\n".repeat(129), &metadata).is_err());
        assert!(Change::from_input(Field::Name, &"x".repeat(MAX_INPUT + 1), &metadata).is_err());
        assert_eq!(
            Change::from_input(Field::Name, " After 😀 ", &metadata).unwrap(),
            Change::Name("After 😀".into())
        );
        assert_eq!(
            Change::from_input(Field::Tags, "", &metadata).unwrap(),
            Change::Tags(vec![])
        );
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use serde_json::json;
    fn metadata() -> Metadata {
        let mut s = crate::fixture::sessions(1).remove(0);
        s.id = "s".into();
        s.model = Some("old-model".into());
        s.reasoning_effort = Some("low".into());
        Metadata::from_session(&s).unwrap()
    }
    #[test]
    fn session_model_and_effort_preserve_confirmed_companion_and_clear_to_inheritance() {
        let m = metadata();
        let c = Change::from_input(Field::Model, " role:reviewer ", &m).unwrap();
        assert!(
            matches!(c.request("s"),Request::SetSessionModel{id,model:Some(model),reasoning_effort:Some(e)}if id=="s"&&model=="role:reviewer"&&e=="low")
        );
        let e = Change::from_input(Field::Effort, " high ", &m).unwrap();
        assert!(
            matches!(e.request("s"),Request::SetSessionModel{model:Some(model),reasoning_effort:Some(e),..}if model=="old-model"&&e=="high")
        );
        let clear = Change::from_input(Field::Model, "  ", &m).unwrap();
        assert!(
            matches!(clear.request("s"),Request::SetSessionModel{model:None,reasoning_effort:Some(e),..}if e=="low")
        );
        let mut ack = m.clone();
        ack.selection.as_mut().unwrap().model = None;
        assert!(clear.matches(&ack));
        ack.selection.as_mut().unwrap().reasoning_effort = Some("high".into());
        assert!(!clear.matches(&ack));
    }
    #[test]
    fn session_model_rejects_unknown_partial_oversize_and_ambiguous_auto_without_harming_metadata()
    {
        let mut s = crate::fixture::sessions(1).remove(0);
        s.id = "s".into();
        s.model = Some("old-model".into());
        s.reasoning_effort = Some("low".into());
        for key in ["model", "reasoning_effort"] {
            let mut value = json!([s]);
            value[0].as_object_mut().unwrap().remove(key);
            let m = Metadata::from_list_reply("s", &value).unwrap();
            assert!(m.selection.is_none());
            assert!(Change::from_input(Field::Model, "", &m).is_err());
            assert!(Change::from_input(Field::Name, "Still editable", &m).is_ok());
        }
        let m = metadata();
        for model in [
            "auto".into(),
            "role:".into(),
            "two\nlines".into(),
            "x".repeat(MAX_MODEL_BYTES + 1),
            "a\u{202e}b".into(),
        ] {
            assert!(Change::from_input(Field::Model, &model, &m).is_err());
        }
        assert!(Change::from_input(Field::Effort, &"x".repeat(MAX_EFFORT_BYTES + 1), &m).is_err());
        s.model = Some("x".repeat(MAX_MODEL_BYTES + 1));
        let m = Metadata::from_session(&s).unwrap();
        assert!(m.selection.is_none());
        assert!(Change::from_input(Field::Tags, "work", &m).is_ok());
        s.model = Some("auto".into());
        let m = Metadata::from_session(&s).unwrap();
        assert!(m.selection.is_some());
        assert!(Change::from_input(Field::Effort, "high", &m).is_err());
        assert!(Change::from_input(Field::Model, "role:primary", &m).is_ok());
    }
}
