//! Non-destructive availability checks for files referenced by a restored DB.
//! This is not filesystem recovery, content verification, or path relocation.

use crate::Paths;
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReferenceCounts {
    pub total: usize,
    /// Expected file/directory/manifest is absent or has the wrong type.
    pub missing: usize,
    pub unreadable: usize,
    /// Outside the destination's expected directory, or location unresolved.
    /// This can overlap missing/unreadable; foreign file contents are not opened.
    pub foreign: usize,
}

impl ReferenceCounts {
    pub fn has_issues(&self) -> bool {
        self.missing > 0 || self.unreadable > 0 || self.foreign > 0
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub artifacts: ReferenceCounts,
    /// Every recorded version, including old, disabled and retired skills.
    pub skill_versions: ReferenceCounts,
}

impl Report {
    pub fn warnings(&self) -> Vec<String> {
        [("artifact rows", &self.artifacts), ("skill versions", &self.skill_versions)]
            .into_iter()
            .filter(|(_, counts)| counts.has_issues())
            .map(|(label, counts)| format!(
                "restored {label}: {} references; {} missing/invalid, {} unreadable, {} foreign/unresolved (counts may overlap). Files were not restored and rows were preserved",
                counts.total, counts.missing, counts.unreadable, counts.foreign))
            .collect()
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Artifact,
    SkillVersion,
}

/// Open only the restored database read-only. Never reuse artifact GC's
/// reconciliation, which deletes missing rows and would discard recovery clues.
pub(crate) fn inspect(db: &Path, paths: &Paths) -> Result<Report> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("open restored database for reference check")?;
    let scan = |table: &str, root: &Path, kind| -> Result<ReferenceCounts> {
        let mut stmt = conn.prepare(&format!("SELECT path FROM {table}"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut counts = ReferenceCounts::default();
        let root = resolve(root)?;
        for path in rows {
            check(&mut counts, Path::new(&path?), &root, kind, available);
        }
        Ok(counts)
    };
    Ok(Report {
        artifacts: scan(
            "artifacts",
            &paths.data_dir.join("artifacts"),
            Kind::Artifact,
        )?,
        skill_versions: scan(
            "skill_versions",
            &paths.data_dir.join("skills"),
            Kind::SkillVersion,
        )?,
    })
}

fn resolve(path: &Path) -> Result<std::path::PathBuf> {
    let raw = path.to_str().context("reference location is not UTF-8")?;
    formless_runtime::caps::canonical_path(raw).map_err(anyhow::Error::msg)
}

fn check(
    counts: &mut ReferenceCounts,
    path: &Path,
    root: &Path,
    kind: Kind,
    probe: impl FnOnce(&Path, Kind, bool) -> std::io::Result<bool>,
) {
    counts.total += 1;
    // Failed resolution never certifies a reference as local. It still gets
    // its independent missing/unreadable classification from the metadata probe.
    let foreign = !resolve(path).is_ok_and(|resolved| resolved.starts_with(root));
    counts.foreign += usize::from(foreign);
    match probe(path, kind, !foreign) {
        Ok(true) => (),
        Ok(false) => counts.missing += 1,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            counts.missing += 1
        }
        Err(_) => counts.unreadable += 1,
    }
}

fn available(path: &Path, kind: Kind, check_readable: bool) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(path)?;
    match kind {
        Kind::Artifact => {
            if !metadata.is_file() {
                return Ok(false);
            }
            if check_readable {
                drop(std::fs::File::open(path)?);
            }
        }
        Kind::SkillVersion => {
            if !metadata.is_dir() {
                return Ok(false);
            }
            let manifest = path.join("SKILL.md");
            if !std::fs::metadata(&manifest)?.is_file() {
                return Ok(false);
            }
            if check_readable {
                drop(std::fs::read_dir(path)?);
                drop(std::fs::File::open(manifest)?);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn escaping_symlink_is_foreign_and_never_opened_for_readability() {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("fl-reference-{}", uuid::Uuid::new_v4())));
        let root = fixture.0.join("artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let outside = fixture.0.join("outside.txt");
        std::fs::write(&outside, "fixture bytes").unwrap();
        let link = root.join("link.txt");
        std::os::unix::fs::symlink(outside, &link).unwrap();
        let mut counts = ReferenceCounts::default();
        check(
            &mut counts,
            &link,
            &root.canonicalize().unwrap(),
            Kind::Artifact,
            |_, _, check_readable| {
                assert!(
                    !check_readable,
                    "foreign references must not open file contents"
                );
                Ok(true)
            },
        );
        assert_eq!(
            counts,
            ReferenceCounts {
                total: 1,
                foreign: 1,
                ..ReferenceCounts::default()
            }
        );
    }

    #[test]
    fn permission_failure_is_reported_separately_without_mutating_a_reference() {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("fl-reference-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&fixture.0).unwrap();
        let root = fixture.0.canonicalize().unwrap();
        let path = root.join("unreadable");
        let mut counts = ReferenceCounts::default();
        check(&mut counts, &path, &root, Kind::Artifact, |_, _, local| {
            assert!(local);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected unreadable file",
            ))
        });
        assert_eq!(
            counts,
            ReferenceCounts {
                total: 1,
                unreadable: 1,
                ..ReferenceCounts::default()
            }
        );
    }
}
