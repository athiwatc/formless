//! Bounded enumeration of an already pinned Windows directory HANDLE.
//!
//! Never reopen its name: an attribute-only reparse retag may redirect that name.
//! The caller serializes enumeration on this handle (RestartInfo changes its cursor).
//! Unsupported filesystems, malformed replies and resource limits are errors, never
//! evidence that a requested name is absent. This is not a filesystem snapshot.
//!
//! Layout/alignment contract:
//! https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_both_dir_info
use std::os::windows::io::AsRawHandle;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::offset_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
};
use windows_sys::Win32::{
    Foundation::ERROR_NO_MORE_FILES,
    Storage::FileSystem::{
        FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
        FILE_ID_BOTH_DIR_INFO,
    },
};

const BUFFER_BYTES: usize = 64 * 1024;
const MAX_BATCHES: usize = 1024;
const MAX_NAMES: usize = 65_536;
const MAX_NAME_BYTES: usize = 8 * 1024 * 1024;
const NAME_OFFSET: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
const LENGTH_OFFSET: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);

fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid HANDLE directory enumeration",
    )
}
fn limit() -> io::Error {
    io::Error::other("HANDLE directory enumeration resource limit")
}

fn parse_aliases(
    buffer: &[u8],
    names: &mut Vec<OsString>,
    name_bytes: &mut usize,
    aliases: &mut Vec<OsString>,
) -> io::Result<()> {
    let mut offset = 0usize;
    loop {
        let remaining = buffer.get(offset..).ok_or_else(malformed)?;
        if remaining.len() < NAME_OFFSET {
            return Err(malformed());
        }
        let next = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(
            remaining[LENGTH_OFFSET..LENGTH_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let end = NAME_OFFSET.checked_add(length).ok_or_else(malformed)?;
        if length == 0 || length % 2 != 0 || end > remaining.len() {
            return Err(malformed());
        }
        // Every non-final record must end before the next aligned record.
        if next != 0 && (next % 8 != 0 || next < end || next >= remaining.len()) {
            return Err(malformed());
        }
        let wide: Vec<u16> = remaining[NAME_OFFSET..end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        if wide.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92)) {
            return Err(malformed());
        }
        let short_length = remaining[offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength)] as usize;
        if short_length > 24 || short_length % 2 != 0 {
            return Err(malformed());
        }
        let short_offset = offset_of!(FILE_ID_BOTH_DIR_INFO, ShortName);
        let short: Vec<u16> = remaining[short_offset..short_offset + short_length]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        if short.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92)) {
            return Err(malformed());
        }
        if wide != [46] && wide != [46, 46] {
            let total = name_bytes
                .checked_add(length + short_length)
                .ok_or_else(limit)?;
            if names.len() >= MAX_NAMES || total > MAX_NAME_BYTES {
                return Err(limit());
            }
            // Native Windows names may contain unpaired UTF-16 surrogates.
            // Preserve them losslessly rather than replacing or aliasing names.
            names.push(OsString::from_wide(&wide));
            if !short.is_empty() {
                aliases.push(OsString::from_wide(&short));
            }
            *name_bytes = total;
        }
        if next == 0 {
            return Ok(());
        }
        offset = offset.checked_add(next).ok_or_else(malformed)?;
    }
}

fn scan(directory: &File) -> io::Result<(Vec<OsString>, Vec<OsString>)> {
    // u64 storage guarantees the required eight-byte alignment.
    let mut aligned = vec![0u64; BUFFER_BYTES / 8];
    let mut names = Vec::new();
    let mut aliases = Vec::new();
    let mut name_bytes = 0;
    for batch in 0..MAX_BATCHES {
        aligned.fill(0);
        let class = if batch == 0 {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        // SAFETY: live borrowed directory handle and aligned, writable 64 KiB
        // allocation; synchronous API retains neither pointer nor handle.
        if unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                aligned.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok((names, aliases))
            } else {
                Err(error)
            };
        }
        // SAFETY: view only initialized storage; parsing copies each native name
        // before the next kernel call overwrites this allocation.
        let bytes =
            unsafe { std::slice::from_raw_parts(aligned.as_ptr().cast::<u8>(), BUFFER_BYTES) };
        parse_aliases(bytes, &mut names, &mut name_bytes, &mut aliases)?;
    }
    Err(limit())
}

/// Long names only: short aliases must never become independent cleanup targets.
pub(super) fn entries(directory: &File) -> io::Result<Vec<OsString>> {
    scan(directory).map(|(names, _)| names)
}

fn absent_from(names: &[OsString], aliases: &[OsString], target: &OsStr) -> io::Result<bool> {
    let wanted: Vec<u16> = target.encode_wide().collect();
    if wanted.is_empty()
        || wanted == [46]
        || wanted == [46, 46]
        || wanted.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92))
    {
        return Err(malformed());
    }
    if names.iter().chain(aliases).any(|name| name == target) {
        return Ok(false);
    }
    let mut ambiguous = false;
    for name in names.iter().chain(aliases) {
        let wide: Vec<u16> = name.encode_wide().collect();
        // A mismatch between paired ASCII units before either first non-ASCII
        // unit cannot be repaired by any later Unicode case mapping. Do not
        // infer anything from lengths or from units beyond that boundary.
        let ascii_prefix_distinct = wanted
            .iter()
            .zip(&wide)
            .take_while(|(left, right)| **left <= 127 && **right <= 127)
            .any(|(left, right)| {
                (*left as u8).to_ascii_uppercase() != (*right as u8).to_ascii_uppercase()
            });
        if ascii_prefix_distinct {
            continue;
        }
        if wanted.iter().chain(&wide).any(|unit| *unit > 127) {
            // NLS CompareStringOrdinal uses the OS uppercase table, not a
            // documented guarantee of equivalence to this volume's $UpCase.
            // Never promote that uncertainty into authoritative absence.
            ambiguous = true;
        } else if wanted
            .iter()
            .map(|unit| (*unit as u8).to_ascii_uppercase())
            .eq(wide.iter().map(|unit| (*unit as u8).to_ascii_uppercase()))
        {
            return Ok(false);
        }
    }
    if ambiguous {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ambiguous native filename case mapping",
        ))
    } else {
        Ok(true)
    }
}

/// A complete scan includes both long names and custom 8.3 aliases. Errors are
/// unknown, never proof of absence. Empty directories prove absence even for a
/// Unicode target; existing exact Unicode names always prove presence.
pub(super) fn child_missing(directory: &File, target: &OsStr) -> io::Result<bool> {
    let (names, aliases) = scan(directory)?;
    absent_from(&names, &aliases, target)
}

#[cfg(test)]
fn parse(buffer: &[u8], names: &mut Vec<OsString>, name_bytes: &mut usize) -> io::Result<()> {
    parse_aliases(buffer, names, name_bytes, &mut Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::windows::fs::OpenOptionsExt, path::PathBuf};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    fn record(name: &[u16], next: usize) -> Vec<u8> {
        let size = (NAME_OFFSET + name.len() * 2).max(next);
        let mut bytes = vec![0; size];
        bytes[..4].copy_from_slice(&(next as u32).to_le_bytes());
        bytes[LENGTH_OFFSET..LENGTH_OFFSET + 4]
            .copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        for (pair, unit) in bytes[NAME_OFFSET..NAME_OFFSET + name.len() * 2]
            .chunks_exact_mut(2)
            .zip(name)
        {
            pair.copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parser_copies_native_names_and_validates_record_chain() {
        let next = (NAME_OFFSET + 2 + 7) & !7;
        let mut bytes = record(&[97], next);
        bytes.extend(record(&[0xd800, 98], 0));
        let mut names = Vec::new();
        let mut count = 0;
        parse(&bytes, &mut names, &mut count).unwrap();
        bytes.fill(0);
        assert_eq!(
            names,
            [OsString::from("a"), OsString::from_wide(&[0xd800, 98])]
        );
        assert_eq!(count, 6);
        for invalid in [
            vec![],
            record(&[], 0),
            record(&[0], 0),
            record(&[47], 0),
            record(&[92], 0),
            record(&[58], 0),
            record(&[97], 1),
            record(&[97], next), // next record is missing
        ] {
            assert!(parse(&invalid, &mut Vec::new(), &mut 0).is_err());
        }
        let mut odd = record(&[97], 0);
        odd[LENGTH_OFFSET..LENGTH_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse(&odd, &mut Vec::new(), &mut 0).is_err());
        let mut clipped = record(&[97], 0);
        clipped.pop();
        assert!(parse(&clipped, &mut Vec::new(), &mut 0).is_err());
        assert!(parse(&record(&[97], 0), &mut Vec::new(), &mut MAX_NAME_BYTES).is_err());
    }

    #[test]
    fn absence_includes_custom_short_names_and_case_ambiguity() {
        let mut bytes = record(&[108, 111, 110, 103], 0);
        let short_offset = offset_of!(FILE_ID_BOTH_DIR_INFO, ShortName);
        bytes[offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength)] = 6;
        for (index, unit) in [65u16, 66, 67].into_iter().enumerate() {
            bytes[short_offset + index * 2..short_offset + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        let mut names = Vec::new();
        let mut aliases = Vec::new();
        parse_aliases(&bytes, &mut names, &mut 0, &mut aliases).unwrap();
        assert_eq!(names, [OsString::from("long")]);
        assert_eq!(aliases, [OsString::from("ABC")]);
        assert!(!absent_from(&names, &aliases, OsStr::new("ABC")).unwrap());
        assert!(!absent_from(&names, &aliases, OsStr::new("abc")).unwrap());
        assert!(absent_from(&names, &aliases, OsStr::new("other")).unwrap());
        assert!(absent_from(&[], &[], OsStr::new("雪")).unwrap());
        assert!(!absent_from(&[OsString::from("雪")], &[], OsStr::new("雪")).unwrap());
        assert!(absent_from(&[OsString::from("雪")], &[], OsStr::new("other")).is_err());
        assert!(absent_from(
            &[OsString::from("0123456789abcdef0123456789abcdef-ไทย")],
            &[],
            OsStr::new(".uploads")
        )
        .unwrap());
        assert!(absent_from(&[OsString::from("foo-ไทย")], &[], OsStr::new("bar")).unwrap());
        assert!(absent_from(&[OsString::from("fóo")], &[], OsStr::new("foo")).is_err());
        assert!(absent_from(&[OsString::from("foo-ไทย")], &[], OsStr::new("foo-other")).is_err());
        // Aliases participate in the same proof; a short-name match wins even
        // when the unrelated long Unicode name has a distinct ASCII prefix.
        assert!(!absent_from(
            &[OsString::from("01234567-ไทย")],
            &[OsString::from("UPLOADS")],
            OsStr::new("uploads")
        )
        .unwrap());
        bytes[offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength)] = 25;
        assert!(parse_aliases(&bytes, &mut Vec::new(), &mut 0, &mut Vec::new()).is_err());
        bytes[offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength)] = 3;
        assert!(parse_aliases(&bytes, &mut Vec::new(), &mut 0, &mut Vec::new()).is_err());
    }

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pinned_directory_empty_entries_and_restart_are_exact() {
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "formless-directory-handle-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&fixture.0)
            .unwrap();
        assert!(entries(&directory).unwrap().is_empty());
        assert!(child_missing(&directory, OsStr::new("雪")).unwrap());
        std::fs::write(fixture.0.join("alpha"), b"private").unwrap();
        std::fs::create_dir(fixture.0.join("directory")).unwrap();
        std::fs::write(fixture.0.join("name-\u{96ea}"), b"private").unwrap();
        let mut expected = vec![
            OsString::from("alpha"),
            OsString::from("directory"),
            OsString::from("name-\u{96ea}"),
        ];
        expected.sort();
        for _ in 0..2 {
            let mut found = entries(&directory).unwrap();
            found.sort();
            assert_eq!(found, expected);
        }
    }
}
