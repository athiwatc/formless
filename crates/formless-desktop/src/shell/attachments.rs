use super::*;
use crate::client::attachments::{Intake, Operation, Outcome};
use crate::pending_files::Report;
use gpui_kit::component::{tooltip::Tooltip, Icon};
use gpui_kit::{assets::IconName, AssetSource as _, ClipboardEntry, PathPromptOptions};
use std::path::PathBuf;

gpui_kit::assets::icon_assets!(AttachmentIcons, [Paperclip, Image, File, ArrowUpRight]);

pub(super) fn icon(name: IconName) -> Icon {
    let data = AttachmentIcons
        .load(&name.path())
        .expect("bundled attachment icon")
        .expect("selected attachment icon");
    Icon::default().data(&data)
}

pub(super) fn transcript_chips(
    block: u64,
    attachments: &[crate::transcript_attachments::Attachment],
    radius: Pixels,
    name_width: Pixels,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .flex_wrap()
        .gap_2()
        .w_full()
        .min_w_0()
        .children(attachments.iter().enumerate().map(|(index, attachment)| {
            let availability = if attachment.expired {
                " · expired"
            } else if attachment.unavailable {
                " · unavailable"
            } else {
                ""
            };
            let tooltip = format!("{} · {}{}", attachment.name, attachment.mime, availability);
            h_flex()
                .id(SharedString::from(format!(
                    "transcript-attachment-{block}-{index}"
                )))
                .test_support()
                .role(Role::Group)
                .aria_label(tooltip.clone())
                .min_w_0()
                .max_w_full()
                .gap_1()
                .px_2()
                .py_1()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(radius)
                .bg(cx.theme().background)
                .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tooltip.clone())).into())
                .child(
                    icon(if attachment.reference {
                        IconName::ArrowUpRight
                    } else if attachment.mime.starts_with("image/") {
                        IconName::Image
                    } else {
                        IconName::File
                    })
                    .size_4()
                    .flex_shrink_0(),
                )
                .child(
                    Label::new(attachment.name.clone())
                        .text_sm()
                        .truncate()
                        .min_w_0()
                        .max_w(name_width),
                )
                .child(
                    Label::new(format_bytes(attachment.bytes as usize))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .flex_shrink_0(),
                )
                .when(!availability.is_empty(), |chip| {
                    chip.child(
                        Label::new(availability)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                })
        }))
}

#[derive(Default)]
pub(super) struct State {
    pub(super) needs_refresh: bool,
    pub(super) pending: Option<Report>,
    pub(super) reference_operation: Option<uuid::Uuid>,
    operations: HashMap<uuid::Uuid, Vec<String>>,
    mutations: HashSet<uuid::Uuid>,
    waiting: Vec<(Intake, u64)>,
    notice: Option<SharedString>,
    notice_revision: u64,
    operation_notices: HashMap<uuid::Uuid, u64>,
}

impl State {
    pub(super) fn busy(&self) -> bool {
        !self.mutations.is_empty()
    }
}

impl Shell {
    fn attachment_notice(&mut self, notice: impl Into<SharedString>) {
        let notice = notice.into();
        self.attachments.notice_revision = self.attachments.notice_revision.wrapping_add(1);
        self.attachments.notice = Some(notice.clone());
        self.composer_notice = Some(notice);
    }

    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if !paths.is_empty() {
            self.stage_composer_attachments(Intake::Paths(paths), cx);
        }
    }

    pub(super) fn pick_composer_attachments(&mut self, cx: &mut Context<Self>) {
        let owner = (self.client_identity(), self.client_generation);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = picker.await;
            this.update(cx, |this, cx| {
                if owner != (this.client_identity(), this.client_generation) {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => this.attach_paths(paths, cx),
                    Ok(Ok(None)) => (),
                    error => {
                        this.attachment_notice(format!(
                            "Could not open attachment picker: {error:?}"
                        ));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn paste_composer_attachments(
        &mut self,
        item: &ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        let paths: Vec<_> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect();
        if !paths.is_empty() {
            self.attach_paths(paths, cx);
            return true;
        }
        if let Some(image) = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(image),
            _ => None,
        }) {
            if image.bytes().len() > client::attachments::MAX_FILE_BYTES {
                self.attachment_notice("Cannot attach clipboard image: file is larger than 20 MiB");
                cx.notify();
                return true;
            }
            let (bytes, mime) = match client::attachments::clipboard_bytes(
                image.bytes(),
                image.format().mime_type(),
            ) {
                Ok(encoded) => encoded,
                Err(error) => {
                    self.attachment_notice(error);
                    cx.notify();
                    return true;
                }
            };
            self.pasted_image_number = self.pasted_image_number.saturating_add(1);
            self.stage_composer_attachments(
                Intake::Bytes {
                    name: format!("pasted-image-{}.png", self.pasted_image_number),
                    bytes,
                    mime,
                },
                cx,
            );
            return true;
        }
        false
    }

    pub(super) fn refresh_composer_attachments(&mut self, cx: &mut Context<Self>) {
        if self.client_connected && self.sidebar.active_id().is_some() {
            self.stage_composer_attachments(Intake::Refresh, cx);
        }
    }

    pub(super) fn poll_composer_attachments(&mut self, cx: &mut Context<Self>) {
        if self.attachments.needs_refresh
            && self.client_connected
            && self.sidebar.active_id().is_some()
        {
            self.attachments.needs_refresh = false;
            self.refresh_composer_attachments(cx);
        }
    }

    pub(super) fn stage_composer_attachments(&mut self, intake: Intake, cx: &mut Context<Self>) {
        self.stage_attachments_with_notice(intake, self.attachments.notice_revision, cx);
    }

    fn stage_attachments_with_notice(
        &mut self,
        mut intake: Intake,
        notice_revision: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.sidebar.active_id().map(str::to_owned) else {
            return;
        };
        if self.client.is_none() || !self.client_connected {
            self.attachment_notice("Connect to the Daemon Core before attaching files.");
            cx.notify();
            return;
        }
        let count = |intake: &Intake| match intake {
            Intake::Paths(paths) => paths.len(),
            Intake::Bytes { .. } => 1,
            _ => 0,
        };
        let requested = count(&intake);
        if requested > 0 {
            let occupied = self
                .attachments
                .pending
                .as_ref()
                .map_or(0, |report| report.rows.len())
                + self
                    .attachments
                    .operations
                    .values()
                    .map(Vec::len)
                    .sum::<usize>()
                + self
                    .attachments
                    .waiting
                    .iter()
                    .map(|(intake, _)| count(intake))
                    .sum::<usize>();
            let available =
                formless_protocol::PENDING_ATTACHMENT_MAX_ENTRIES.saturating_sub(occupied);
            if requested > available {
                self.attachment_notice(format!("Only 64 pending attachments are supported. {} extra attachment(s) were not staged.", requested - available));
                cx.notify();
                if available == 0 {
                    return;
                }
                if let Intake::Paths(paths) = &mut intake {
                    paths.truncate(available);
                }
            }
            if self.attachments.pending.is_none() {
                self.attachments.waiting.push((intake, notice_revision));
                if self.attachments.operations.is_empty() {
                    self.refresh_composer_attachments(cx);
                }
                return;
            }
        }
        let client = self.client.as_ref().unwrap();
        let names = match &intake {
            Intake::Paths(paths) if paths.is_empty() => return,
            Intake::Paths(paths) => paths
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            Intake::Bytes { name, .. } => vec![name.clone()],
            _ => Vec::new(),
        };
        let mutating = !matches!(intake, Intake::Refresh);
        let operation = Operation {
            id: uuid::Uuid::new_v4(),
            session_id,
            intake,
        };
        let id = operation.id;
        match client.send(ClientCommand::StageAttachments(operation)) {
            Ok(()) => {
                self.attachments.needs_refresh = false;
                self.attachments.operations.insert(id, names);
                self.attachments
                    .operation_notices
                    .insert(id, notice_revision);
                if mutating {
                    self.attachments.mutations.insert(id);
                    self.invalidate_forecast(cx);
                }
            }
            Err(error) => self.attachment_notice(format!("Attachments unavailable: {error}")),
        }
        cx.notify();
    }

    pub(super) fn composer_attachments_received(
        &mut self,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        if self.client_identity() != Some((outcome.client, outcome.connection))
            || !self.client_connected
            || self.sidebar.active_id() != Some(outcome.session_id.as_str())
        {
            return;
        }
        let Some(names) = self.attachments.operations.remove(&outcome.operation) else {
            return;
        };
        let notice_revision = self
            .attachments
            .operation_notices
            .remove(&outcome.operation);
        self.attachments.mutations.remove(&outcome.operation);
        let mut errors = outcome.errors;
        self.attachments.pending = match outcome.pending {
            Ok(report) => Some(report),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if !errors.is_empty() {
            self.attachment_notice(errors.join("\n"));
        } else if !names.is_empty()
            && notice_revision == Some(self.attachments.notice_revision)
            && self.composer_notice == self.attachments.notice
        {
            self.composer_notice = None;
            self.attachments.notice = None;
        }
        let waiting = std::mem::take(&mut self.attachments.waiting);
        if self.attachments.pending.is_some() {
            for (intake, revision) in waiting {
                self.stage_attachments_with_notice(intake, revision, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn has_composer_attachments(&self) -> bool {
        self.attachments
            .pending
            .as_ref()
            .is_some_and(|report| !report.rows.is_empty())
    }

    pub(super) fn composer_attachments_visible(&self) -> bool {
        self.has_composer_attachments()
            || self
                .attachments
                .operations
                .values()
                .any(|names| !names.is_empty())
    }

    fn detach_composer_attachment(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.attachments.busy() {
            self.stage_composer_attachments(Intake::Detach { id }, cx);
        }
    }

    pub(super) fn remove_last_composer_attachment(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(report) = self.attachments.pending.as_ref() else {
            return false;
        };
        let Some(id) = report
            .rows
            .len()
            .checked_sub(1)
            .and_then(|index| report.stored_id(index))
            .map(str::to_owned)
        else {
            return false;
        };
        self.detach_composer_attachment(id, cx);
        true
    }

    pub(super) fn render_composer_attachments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let metadata = self
            .attachments
            .pending
            .as_ref()
            .map(Report::expected)
            .unwrap_or_default();
        h_flex()
            .id("composer-attachments")
            .test_support()
            .role(Role::Group)
            .aria_label("Pending attachments")
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .max_h(self.scaled(128.))
            .overflow_y_scroll()
            .children(self.attachments.pending.iter().flat_map(|report| {
                report.rows.iter().enumerate().map(|(index, row)| {
                    let mime = metadata[index]["mime"].as_str().unwrap_or(row.kind);
                    let name = metadata[index]["full_name"].as_str().unwrap_or(&row.name);
                    let tooltip = format!("{name} · {mime}");
                    let stored_id = report.stored_id(index).map(str::to_owned);
                    h_flex()
                        .id(("composer-attachment", index))
                        .test_support()
                        .role(Role::Group)
                        .aria_label(tooltip.clone())
                        .min_w_0()
                        .max_w_full()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(self.scaled(crate::theme::CONTROL_RADIUS))
                        .bg(cx.theme().background)
                        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tooltip.clone())).into())
                        .child(
                            icon(if mime.starts_with("image/") {
                                IconName::Image
                            } else {
                                IconName::File
                            })
                            .size_4()
                            .flex_shrink_0(),
                        )
                        .child(
                            Label::new(row.name.clone())
                                .text_sm()
                                .truncate()
                                .min_w_0()
                                .max_w(self.scaled(160.)),
                        )
                        .child(
                            Label::new(format_bytes(row.bytes as usize))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .flex_shrink_0(),
                        )
                        .child(
                            Button::new(("composer-attachment-remove", index))
                                .small()
                                .ghost()
                                .label("×")
                                .accessibility_label(format!("Remove {}", row.name))
                                .tooltip(if stored_id.is_some() {
                                    "Remove attachment"
                                } else {
                                    "Remove references using Pending files review"
                                })
                                .disabled(self.attachments.busy() || stored_id.is_none())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(id) = &stored_id {
                                        this.detach_composer_attachment(id.clone(), cx);
                                    }
                                })),
                        )
                })
            }))
            .children(
                self.attachments
                    .operations
                    .values()
                    .flatten()
                    .enumerate()
                    .map(|(index, name)| {
                        readable(
                            ("composer-attachment-uploading", index),
                            format!("Uploading {name}…"),
                        )
                        .text_xs()
                        .min_w_0()
                        .max_w_full()
                        .truncate()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(cx.theme().accent)
                    }),
            )
    }
}
