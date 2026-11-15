//! Bounded Session metadata edits using the daemon's existing setters.
//! These are explicit replacements, not compare-and-swap operations.

use formless_protocol::{Request, SessionInfo};

pub const MAX_INPUT: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
}

impl Metadata {
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
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Tags,
    Pin,
    Archive,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Tags => "Tags",
            Self::Pin => "Pin",
            Self::Archive => "Archive",
        }
    }
    pub fn value(self, metadata: &Metadata) -> String {
        match self {
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Name(String),
    Tags(Vec<String>),
    Pin(bool),
    Archive(bool),
}

impl Change {
    pub fn from_input(field: Field, text: &str, metadata: &Metadata) -> Result<Self, &'static str> {
        if text.len() > MAX_INPUT {
            return Err("Use at most 16 KiB; no change was sent.");
        }
        let change = match field {
            Field::Name => Self::Name(text.trim().into()),
            Field::Tags => Self::Tags(text.lines().map(str::to_string).collect()),
            Field::Pin => Self::Pin(!metadata.pinned),
            Field::Archive => Self::Archive(!metadata.archived),
        };
        change.validate()?;
        Ok(change)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
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
    pub fn request(&self, id: &str) -> Request {
        match self {
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
        }
    }
    pub fn matches(&self, metadata: &Metadata) -> bool {
        match self {
            Self::Name(name) => name == &metadata.name,
            Self::Pin(pinned) => pinned == &metadata.pinned,
            Self::Archive(archived) => archived == &metadata.archived,
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
    Rejected(String),
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
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
