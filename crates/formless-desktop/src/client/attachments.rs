//! Session-scoped attachment staging. Never replays mutations after reconnect.
use super::{Engine, Pending, Update};
use base64::Engine as _;
use formless_protocol::Request;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Default)]
struct ClipboardPng(Cursor<Vec<u8>>);

impl Write for ClipboardPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.position().saturating_add(bytes.len() as u64) > MAX_FILE_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Clipboard PNG is larger than 20 MiB",
            ));
        }
        self.0.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Seek for ClipboardPng {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(position)
    }
}

pub fn clipboard_bytes(bytes: &[u8], mime: &str) -> Result<(Vec<u8>, String), String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("Cannot attach clipboard image: file is larger than 20 MiB".into());
    }
    if matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Ok((bytes.to_vec(), mime.into()));
    }
    let format = image::guess_format(bytes)
        .map_err(|error| format!("Could not convert clipboard image to PNG: {error}"))?;
    let convert = || -> Result<Vec<u8>, image::ImageError> {
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        reader.limits(limits);
        let decoded = reader.decode()?;
        let mut output = ClipboardPng::default();
        decoded.write_to(&mut output, image::ImageFormat::Png)?;
        Ok(output.0.into_inner())
    };
    convert()
        .map(|bytes| (bytes, "image/png".into()))
        .map_err(|error| format!("Could not convert clipboard image to PNG: {error}"))
}

#[derive(Debug)]
pub enum Intake {
    Paths(Vec<PathBuf>),
    Bytes {
        name: String,
        bytes: Vec<u8>,
        mime: String,
    },
    Detach {
        id: String,
    },
    Refresh,
}

#[derive(Debug)]
pub struct Operation {
    pub id: uuid::Uuid,
    pub session_id: String,
    pub intake: Intake,
}

impl Operation {
    /// `mime` is the clipboard image format's MIME type, not a filename guess.
    pub fn clipboard_image(session_id: String, number: u64, bytes: Vec<u8>, mime: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            session_id,
            intake: Intake::Bytes {
                name: format!("pasted-image-{number}.png"),
                bytes,
                mime,
            },
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub operation: uuid::Uuid,
    pub session_id: String,
    pub errors: Vec<String>,
    pub pending: Result<crate::pending_files::Report, String>,
}

#[derive(Debug)]
struct File {
    name: String,
    bytes: Vec<u8>,
    mime: Option<String>,
}

fn read_path(path: &std::path::Path) -> Result<File, String> {
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "Cannot attach {}: select a regular file, not a directory",
            path.display()
        ));
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(format!(
            "Cannot attach {}: file is larger than 20 MiB",
            path.display()
        ));
    }
    let file =
        std::fs::File::open(path).map_err(|e| format!("Cannot attach {}: {e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "Cannot attach {}: select a regular file, not a directory",
            path.display()
        ));
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(format!(
            "Cannot attach {}: file is larger than 20 MiB",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "Cannot attach {}: file grew beyond 20 MiB",
            path.display()
        ));
    }
    Ok(File {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        bytes,
        mime: None,
    })
}

#[derive(Debug)]
enum Source {
    Path(PathBuf),
    Bytes(File),
}
#[derive(Debug)]
enum Phase {
    Begin,
    Chunk,
    Finish,
    Abort,
    Detach,
    Refresh,
}
#[derive(Debug)]
pub(super) struct Work {
    outcome: Outcome,
    sources: VecDeque<Source>,
    file: Option<File>,
    phase: Phase,
    upload: String,
    chunk: usize,
    offset: usize,
    index: u64,
}

impl Engine {
    pub(super) fn stage_attachments(&mut self, operation: Operation) {
        let mut work = Box::new(Work {
            outcome: Outcome {
                client: self.instance,
                connection: self.connection,
                operation: operation.id,
                session_id: operation.session_id,
                errors: Vec::new(),
                pending: Err("Pending attachments were not refreshed".into()),
            },
            sources: VecDeque::new(),
            file: None,
            phase: Phase::Refresh,
            upload: String::new(),
            chunk: 0,
            offset: 0,
            index: 0,
        });
        if matches!(operation.intake, Intake::Refresh) && !self.reviewed_send_available {
            work.outcome.pending = Err(
                "Pending-file inventory unavailable. Update the daemon or use Formless TUI.".into(),
            );
            self.updates.push(Update::AttachmentStaged(work.outcome));
            return;
        }
        if !self.ready {
            work.outcome
                .errors
                .push("Not connected; no attachment was staged".into());
            self.updates.push(Update::AttachmentStaged(work.outcome));
            return;
        }
        match operation.intake {
            Intake::Paths(paths) => work.sources.extend(paths.into_iter().map(Source::Path)),
            Intake::Bytes { name, bytes, mime } => work.sources.push_back(Source::Bytes(File {
                name,
                bytes,
                mime: Some(mime),
            })),
            Intake::Detach { id } => {
                work.phase = Phase::Detach;
                self.request(
                    Request::DetachFile {
                        session_id: work.outcome.session_id.clone(),
                        id,
                    },
                    Pending::Attachment(work),
                );
                return;
            }
            Intake::Refresh => {}
        }
        self.next_attachment(work);
    }

    fn next_attachment(&mut self, mut work: Box<Work>) {
        work.file = None;
        work.upload.clear();
        while let Some(source) = work.sources.pop_front() {
            let file = match source {
                Source::Path(path) => read_path(&path),
                Source::Bytes(file) => Ok(file),
            };
            match file {
                Ok(file) if file.bytes.len() <= MAX_FILE_BYTES => {
                    let request = Request::AttachBegin {
                        session_id: work.outcome.session_id.clone(),
                        name: file.name.clone(),
                        bytes: file.bytes.len() as u64,
                        mime: file.mime.clone(),
                    };
                    work.file = Some(file);
                    work.phase = Phase::Begin;
                    work.offset = 0;
                    work.index = 0;
                    self.request(request, Pending::Attachment(work));
                    return;
                }
                Ok(file) => work
                    .outcome
                    .errors
                    .push(format!("{} is larger than 20 MiB", file.name)),
                Err(error) => work.outcome.errors.push(error),
            }
        }
        work.phase = Phase::Refresh;
        self.request(
            Request::ListPendingAttachments {
                session_id: work.outcome.session_id.clone(),
            },
            Pending::Attachment(work),
        );
    }

    fn attachment_piece(&mut self, mut work: Box<Work>) {
        let file = work.file.as_ref().expect("active attachment");
        let request = if work.offset == file.bytes.len() {
            work.phase = Phase::Finish;
            Request::AttachFinish {
                upload_id: work.upload.clone(),
            }
        } else {
            let end = (work.offset + work.chunk).min(file.bytes.len());
            let data_b64 =
                base64::engine::general_purpose::STANDARD.encode(&file.bytes[work.offset..end]);
            work.offset = end;
            work.phase = Phase::Chunk;
            Request::AttachChunk {
                upload_id: work.upload.clone(),
                index: work.index,
                data_b64,
            }
        };
        self.request(request, Pending::Attachment(work));
    }

    pub(super) fn attachment_reply(&mut self, mut work: Box<Work>, result: Result<Value, String>) {
        if matches!(work.phase, Phase::Refresh) {
            work.outcome.pending = result
                .and_then(|v| crate::pending_files::Report::decode(&work.outcome.session_id, v));
            self.updates.push(Update::AttachmentStaged(work.outcome));
            return;
        }
        if matches!(work.phase, Phase::Abort) {
            if let Err(error) = result {
                work.outcome
                    .errors
                    .push(format!("Upload cleanup failed: {error}"));
            }
            self.next_attachment(work);
            return;
        }
        let checked = result.and_then(|value| {
            match work.phase {
                Phase::Begin => {
                    work.upload = value["upload_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("Invalid upload identity")?
                        .into();
                    work.chunk = value["chunk_bytes"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n > 0 && *n <= MAX_FILE_BYTES)
                        .ok_or("Invalid upload chunk size")?;
                    if value["max_bytes"]
                        .as_u64()
                        .is_none_or(|max| work.file.as_ref().unwrap().bytes.len() as u64 > max)
                    {
                        return Err("Attachment exceeds the daemon's advertised size limit".into());
                    }
                }
                Phase::Chunk if value["received"].as_u64() != Some(work.offset as u64) => {
                    return Err("Invalid upload acknowledgement".into())
                }
                Phase::Finish if value["id"].as_str().is_none_or(str::is_empty) => {
                    return Err(
                        "Invalid attachment acknowledgement; refresh pending files before retrying"
                            .into(),
                    )
                }
                Phase::Detach if value["removed"] != true => {
                    return Err("Attachment was not removed; refresh pending files".into())
                }
                _ => {}
            }
            Ok(())
        });
        if let Err(error) = checked {
            let name = work.file.as_ref().map_or("Attachment", |f| f.name.as_str());
            work.outcome.errors.push(format!("{name}: {error}"));
            if !work.upload.is_empty() {
                work.phase = Phase::Abort;
                self.request(
                    Request::AttachAbort {
                        upload_id: work.upload.clone(),
                    },
                    Pending::Attachment(work),
                );
            } else {
                self.next_attachment(work);
            }
            return;
        }
        match work.phase {
            Phase::Begin => self.attachment_piece(work),
            Phase::Chunk => {
                work.index += 1;
                self.attachment_piece(work);
            }
            Phase::Finish | Phase::Detach => self.next_attachment(work),
            Phase::Abort | Phase::Refresh => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_tiff(width: u32, height: u32) -> Vec<u8> {
        // One RGB strip using PackBits repeat runs, with an out-of-line BitsPerSample.
        let raw_bytes = width as u64 * height as u64 * 3;
        assert_eq!(raw_bytes % 128, 0);
        let packed_bytes = u32::try_from(raw_bytes / 128 * 2).unwrap();
        let tags = [
            (256_u16, 4_u16, 1_u32, width),
            (257, 4, 1, height),
            (258, 3, 3, 134),
            (259, 3, 1, 32773),
            (262, 3, 1, 2),
            (273, 4, 1, 140),
            (277, 3, 1, 3),
            (278, 4, 1, height),
            (279, 4, 1, packed_bytes),
            (284, 3, 1, 1),
        ];
        let mut tiff = b"II\x2a\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&(tags.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in tags {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&kind.to_le_bytes());
            tiff.extend_from_slice(&count.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&[0, 0, 0, 0, 8, 0, 8, 0, 8, 0]);
        for _ in 0..raw_bytes / 128 {
            tiff.extend_from_slice(&[129, 0]);
        }
        tiff
    }

    #[test]
    fn compressed_4k_tiff_uses_separate_decode_and_upload_limits() {
        let tiff = solid_tiff(4096, 4096);
        assert!(tiff.len() < MAX_FILE_BYTES);
        let dimensions = image::ImageReader::new(Cursor::new(&tiff))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(u64::from(dimensions.0) * u64::from(dimensions.1) * 3 > MAX_FILE_BYTES as u64);
        let (png, mime) = clipboard_bytes(&tiff, "image/tiff").unwrap();
        assert_eq!(mime, "image/png");
        assert!(png.len() < MAX_FILE_BYTES);
        assert_eq!(
            image::ImageReader::new(Cursor::new(&png))
                .with_guessed_format()
                .unwrap()
                .into_dimensions()
                .unwrap(),
            (4096, 4096)
        );
        assert!(clipboard_bytes(&solid_tiff(MAX_IMAGE_DIMENSION + 128, 1), "image/tiff").is_err());
    }

    #[test]
    fn clipboard_upload_limits_reject_input_and_encoded_output_over_twenty_mib() {
        let oversized = vec![0; MAX_FILE_BYTES + 1];
        for mime in ["image/png", "image/tiff"] {
            assert!(clipboard_bytes(&oversized, mime)
                .unwrap_err()
                .contains("larger than 20 MiB"));
        }
        let mut png = ClipboardPng::default();
        png.seek(SeekFrom::Start(MAX_FILE_BYTES as u64)).unwrap();
        assert!(png
            .write_all(&[0])
            .unwrap_err()
            .to_string()
            .contains("larger than 20 MiB"));
        assert!(png.0.get_ref().is_empty());
    }

    #[test]
    fn clipboard_ico_aliases_are_detected_from_bytes_and_converted_to_png() {
        let original = image::DynamicImage::new_rgba8(2, 2);
        let mut encoded = Cursor::new(Vec::new());
        original
            .write_to(&mut encoded, image::ImageFormat::Ico)
            .unwrap();
        for mime in ["image/ico", "image/x-icon", "image/vnd.microsoft.icon"] {
            let (png, mime) = clipboard_bytes(encoded.get_ref(), mime).unwrap();
            assert_eq!(mime, "image/png");
            assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
            assert_eq!(
                image::load_from_memory(&png).unwrap().to_rgba8(),
                original.to_rgba8()
            );
        }
    }

    #[test]
    fn vision_clipboard_formats_preserve_every_encoded_byte() {
        let frames = b"encoded animation frames and metadata must not be decoded or flattened";
        for mime in ["image/gif", "image/webp"] {
            assert_eq!(
                clipboard_bytes(frames, mime).unwrap(),
                (frames.to_vec(), mime.into())
            );
        }
    }

    #[test]
    fn clipboard_rasters_convert_to_png_and_invalid_images_report_errors() {
        let original = image::DynamicImage::new_rgb8(2, 2);
        for (format, mime) in [
            (image::ImageFormat::Tiff, "image/tiff"),
            (image::ImageFormat::Bmp, "image/bmp"),
        ] {
            let mut encoded = Cursor::new(Vec::new());
            original.write_to(&mut encoded, format).unwrap();
            let (png, mime) = clipboard_bytes(encoded.get_ref(), mime).unwrap();
            assert_eq!(mime, "image/png");
            assert!(png.starts_with(b"\x89PNG"));
            assert_eq!(
                image::load_from_memory(&png).unwrap().to_rgb8(),
                original.to_rgb8()
            );
        }
        for mime in ["image/png", "image/jpeg"] {
            assert_eq!(
                clipboard_bytes(b"encoded", mime).unwrap(),
                (b"encoded".to_vec(), mime.into())
            );
        }
        assert!(clipboard_bytes(b"broken", "image/tiff")
            .unwrap_err()
            .contains("convert clipboard image"));
        assert!(clipboard_bytes(b"", "image/svg+xml").is_err());
    }
    use crate::client::{Command, ServerMessage};
    use serde_json::json;

    fn operation(intake: Intake) -> Operation {
        Operation {
            id: uuid::Uuid::new_v4(),
            session_id: "chosen".into(),
            intake,
        }
    }
    fn engine(intake: Intake) -> Engine {
        let mut engine = Engine {
            ready: true,
            reviewed_send_available: true,
            ..Engine::default()
        };
        engine.command(Command::StageAttachments(operation(intake)));
        engine
    }
    fn reply(engine: &mut Engine, value: Result<Value, &str>) -> Request {
        let envelope = engine.outgoing.pop_front().unwrap();
        let response = match value {
            Ok(value) => ServerMessage::ok(envelope.id, value),
            Err(error) => ServerMessage::err(envelope.id, error.into()),
        };
        engine.receive(response).unwrap();
        envelope.request
    }
    fn begin() -> Value {
        json!({"upload_id":"upload", "chunk_bytes":3, "max_bytes":MAX_FILE_BYTES})
    }
    fn bytes(data: &[u8]) -> Intake {
        Intake::Bytes {
            name: "file.txt".into(),
            bytes: data.to_vec(),
            mime: "text/plain".into(),
        }
    }
    fn refreshed(engine: &mut Engine) -> Outcome {
        assert!(
            matches!(reply(engine, Ok(json!({"session_id":"chosen","attachments":[]}))), Request::ListPendingAttachments{session_id} if session_id=="chosen")
        );
        match engine.updates.pop().unwrap() {
            Update::AttachmentStaged(outcome) => outcome,
            _ => panic!("attachment result"),
        }
    }

    #[test]
    fn chunk_splitting_uses_advertised_size_and_keeps_session_after_switch() {
        let mut e = engine(bytes(b"abcdefg"));
        assert!(
            matches!(reply(&mut e, Ok(begin())), Request::AttachBegin { bytes:7, session_id, .. } if session_id=="chosen")
        );
        e.active = Some("other".into());
        for (index, (data, received)) in [
            (b"abc".as_slice(), 3),
            (b"def".as_slice(), 6),
            (b"g".as_slice(), 7),
        ]
        .into_iter()
        .enumerate()
        {
            let req = reply(&mut e, Ok(json!({"received":received})));
            assert!(
                matches!(req, Request::AttachChunk{index:found,data_b64,..} if found==index as u64 && base64::engine::general_purpose::STANDARD.decode(&data_b64).unwrap()==data)
            );
        }
        assert!(matches!(
            reply(&mut e, Ok(json!({"id":"artifact"}))),
            Request::AttachFinish { .. }
        ));
        let outcome = refreshed(&mut e);
        assert_eq!(outcome.session_id, "chosen");
        assert!(outcome.errors.is_empty());
        assert!(outcome.pending.unwrap().rows.is_empty());
    }

    #[test]
    fn empty_file_finishes_without_chunks_and_empty_batch_refreshes() {
        let mut e = engine(bytes(b""));
        reply(&mut e, Ok(begin()));
        assert!(matches!(
            reply(&mut e, Ok(json!({"id":"empty"}))),
            Request::AttachFinish { .. }
        ));
        assert!(refreshed(&mut e).errors.is_empty());
        assert!(refreshed(&mut engine(Intake::Paths(vec![])))
            .pending
            .is_ok());
    }

    #[test]
    fn upload_errors_abort_before_continuing_and_refresh() {
        for phase in ["begin", "chunk", "finish", "bad chunk", "bad received"] {
            let mut e = engine(bytes(b"abc"));
            if phase == "begin" {
                reply(&mut e, Err("begin rejected"));
            } else if phase == "bad chunk" {
                reply(
                    &mut e,
                    Ok(json!({"upload_id":"upload","chunk_bytes":0,"max_bytes":MAX_FILE_BYTES})),
                );
            } else {
                reply(&mut e, Ok(begin()));
                if phase == "finish" {
                    reply(&mut e, Ok(json!({"received":3})));
                    reply(&mut e, Err("finish rejected"));
                } else if phase == "bad received" {
                    reply(&mut e, Ok(json!({"received":2})));
                } else {
                    reply(&mut e, Err("chunk rejected"));
                }
            }
            if phase != "begin" {
                assert!(
                    matches!(reply(&mut e, Ok(json!({"aborted":true}))), Request::AttachAbort{upload_id} if upload_id=="upload")
                );
            }
            assert_eq!(refreshed(&mut e).errors.len(), 1, "{phase}");
        }
    }

    #[test]
    fn paths_reject_directories_and_oversize_before_reading_and_continue() {
        let root = std::env::temp_dir().join(format!("desktop-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let large = root.join("large");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_FILE_BYTES as u64 + 1)
            .unwrap();
        let good = root.join("good");
        std::fs::write(&good, b"ok").unwrap();
        assert!(read_path(&root).err().unwrap().contains("directory"));
        assert!(read_path(&large).err().unwrap().contains("20 MiB"));
        let mut e = engine(Intake::Paths(vec![
            root.clone(),
            large,
            root.join("missing"),
            good,
        ]));
        reply(&mut e, Ok(begin()));
        reply(&mut e, Ok(json!({"received":2})));
        reply(&mut e, Ok(json!({"id":"good"})));
        assert_eq!(refreshed(&mut e).errors.len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clipboard_name_mime_oversize_detach_and_refresh_errors() {
        let op = Operation::clipboard_image("chosen".into(), 2, vec![1], "image/webp".into());
        let mut e = engine(op.intake);
        assert!(
            matches!(reply(&mut e,Err("no session")), Request::AttachBegin{name,mime:Some(mime),..} if name=="pasted-image-2.png" && mime=="image/webp")
        );
        refreshed(&mut e);
        let mut e = engine(bytes(&vec![0; MAX_FILE_BYTES + 1]));
        assert!(refreshed(&mut e).errors[0].contains("20 MiB"));
        let mut e = engine(Intake::Detach {
            id: "artifact".into(),
        });
        assert!(
            matches!(reply(&mut e,Ok(json!({"removed":true}))), Request::DetachFile{id,..} if id=="artifact")
        );
        reply(&mut e, Err("inventory unavailable"));
        assert!(matches!(
            e.updates.pop(),
            Some(Update::AttachmentStaged(Outcome {
                pending: Err(_),
                ..
            }))
        ));
    }

    #[test]
    fn failed_upload_does_not_drop_later_files() {
        let root = std::env::temp_dir().join(format!("desktop-batch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let mut e = engine(Intake::Paths(vec![first, second]));
        reply(&mut e, Ok(begin()));
        reply(&mut e, Err("piece rejected"));
        assert!(matches!(
            reply(&mut e, Ok(json!({"aborted":true}))),
            Request::AttachAbort { .. }
        ));
        assert!(
            matches!(reply(&mut e,Ok(begin())),Request::AttachBegin{name,..} if name=="second")
        );
        reply(&mut e, Ok(json!({"received":3})));
        reply(&mut e, Ok(json!({"id":"second"})));
        assert_eq!(refreshed(&mut e).errors.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
