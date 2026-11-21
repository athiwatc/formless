use super::*;

pub(super) fn secondary(destination: Destination, row: &crate::pages::Row) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(&row.detail).ok()?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let summary = match destination {
        Destination::Mcp => [text("transport"), text("target")]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        Destination::Jobs => format!(
            "{} · Next: {}",
            if text("cron").is_empty() {
                "Schedule not set"
            } else {
                text("cron")
            },
            if text("next_run").is_empty() {
                "not scheduled"
            } else {
                text("next_run")
            }
        ),
        Destination::Memory => format!(
            "Scope: {}",
            if text("scope").is_empty() {
                "global"
            } else {
                text("scope")
            }
        ),
        _ => return None,
    };
    (!summary.is_empty()).then(|| directory::visual_text(&summary))
}

impl Shell {
    pub(super) fn render_resource_fields(
        &self,
        row: Option<&crate::pages::Row>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut grid = v_flex()
            .id("resource-detail")
            .test_support()
            .w_full()
            .min_w_0()
            .gap_2()
            .flex_shrink_0();
        let Some(row) = row else {
            return grid.into_any_element();
        };
        if let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(&row.detail) {
            let mut remaining = crate::pages::PREVIEW_CHARS;
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if remaining == 0 {
                    break;
                }
                let key = key
                    .split('_')
                    .map(|word| if word == "id" { "ID" } else { word })
                    .collect::<Vec<_>>()
                    .join(" ");
                let key: String = key.chars().take(remaining).collect();
                remaining = remaining.saturating_sub(key.chars().count() + 2);
                let value = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                let value: String = value.chars().take(remaining).collect();
                remaining = remaining.saturating_sub(value.chars().count() + 1);
                grid = grid.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_start()
                        .gap_3()
                        .child(
                            readable(("resource-field-key", index), key)
                                .w(self.scaled(100.))
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            readable(("resource-field-value", index), value)
                                .flex_1()
                                .min_w_0(),
                        ),
                );
            }
        } else {
            grid = grid.child(readable("resource-field-text", row.preview.clone()));
        }
        if row.preview_truncated {
            grid = grid.child(
                readable(
                    "resource-detail-truncated",
                    "Preview limited to 500 characters. Copy details for all returned fields.",
                )
                .text_color(cx.theme().muted_foreground),
            );
        }
        grid.into_any_element()
    }

    fn resource_management_target(&self) -> Option<(Destination, u64, Option<usize>)> {
        let page = self.resource_pages.get(&self.destination)?;
        Some((self.destination, page.revision, page.selected))
    }

    fn resource_management_action(&self, slot: usize) -> Option<crate::manage::RowAction> {
        if slot == 0 {
            crate::manage::page_actions(self.destination)
                .into_iter()
                .next()
                .map(|mut action| {
                    action.confirm = true;
                    action
                })
        } else {
            let page = self.resource_pages.get(&self.destination)?;
            page.rows
                .get(page.selected?)?
                .actions
                .get(slot - 1)
                .cloned()
        }
    }

    fn activate_resource_action(
        &mut self,
        target: (Destination, u64, Option<usize>),
        slot: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.client_connected
            || self.background_inert(window)
            || self.resource_management_target() != Some(target)
        {
            return;
        }
        let Some(action) = self.resource_management_action(slot) else {
            return;
        };
        let confirmation = (target.0, target.1, target.2, slot);
        if action.confirm && self.resource_confirm != Some(confirmation) {
            self.resource_confirm = Some(confirmation);
        } else {
            self.resource_confirm = None;
            self.client_request(
                ClientCommand::Manage {
                    destination: target.0,
                    request: action.request(),
                    label: action.label.into(),
                },
                cx,
            );
        }
        cx.notify();
    }

    fn submit_resource(
        &mut self,
        destination: Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.destination != destination
            || !self.client_connected
            || self.background_inert(window)
        {
            return;
        }
        let text = self.resource_input.read(cx).value();
        let schedule = self.resource_schedule.read(cx).value();
        let (request, label) = match destination {
            Destination::Memory => (crate::manage::add_memory(&text), "add memory"),
            Destination::Skills => (crate::manage::add_skill(&text), "add skill"),
            Destination::Jobs => (crate::manage::add_job(&text, &schedule), "add job"),
            _ => return,
        };
        match request {
            Ok(request) => {
                self.resource_error = None;
                self.client_request(
                    ClientCommand::Manage {
                        destination,
                        request,
                        label: label.into(),
                    },
                    cx,
                );
            }
            Err(error) => self.resource_error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn render_resource_management(
        &self,
        page_level: bool,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let destination = self.destination;
        let mut body = v_flex().w_full().min_w_0().gap_2();
        if crate::manage::list_request(destination).is_none() {
            return body.into_any_element();
        }
        if let Some(target) = self.resource_management_target() {
            let slots = if page_level { 0..1 } else { 1..4 };
            let mut bar = h_flex().w_full().min_w_0().gap_2().flex_wrap();
            for slot in slots {
                let Some(action) = self.resource_management_action(slot) else {
                    continue;
                };
                let confirmation = (target.0, target.1, target.2, slot);
                let confirming = self.resource_confirm == Some(confirmation);
                let label = if confirming {
                    format!("Confirm {}", action.label)
                } else {
                    action.label.into()
                };
                bar = bar.child(
                    div().id(("resource-action-anchor", slot))
                        .anchor_scroll(Some(self.resource_anchors[4 + slot].clone()))
                        .child(Button::new(("resource-action", slot))
                        .small()
                        .label(label.clone())
                        .accessibility_label(label)
                        .tooltip(if action.confirm {
                            "Requires a second click to confirm. Cancel keeps the entry unchanged."
                        } else {
                            "Apply this change to the selected daemon entry."
                        })
                        .track_focus(&self.resource_controls[4 + slot])
                        .disabled(!self.client_connected)
                        .when(confirming, |button| button.danger())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_resource_action(target, slot, window, cx)
                        }))),
                );
                if confirming {
                    bar = bar.child(
                        div()
                            .id("resource-action-cancel-anchor")
                            .anchor_scroll(Some(self.resource_anchors[8].clone()))
                            .child(
                                Button::new("resource-action-cancel")
                                    .small()
                                    .label("Cancel")
                                    .accessibility_label("Cancel pending resource change")
                                    .track_focus(&self.resource_controls[8])
                                    .disabled(!self.client_connected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.resource_confirm == Some(confirmation) {
                                            this.resource_confirm = None;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    );
                }
            }
            body = body.child(bar);
        }
        if page_level {
            let form = match destination {
                Destination::Memory => Some(("Memory text", "Add memory")),
                Destination::Skills => Some(("Skill source: folder, git URL or .zip", "Add skill")),
                Destination::Jobs => Some(("Job prompt", "Add job")),
                _ => None,
            };
            if let Some((label, add)) = form {
                body = body.child(readable("resource-input-label", label)).child(
                    div()
                        .id("resource-text-field")
                        .test_support()
                        .anchor_scroll(Some(self.resource_anchors[10].clone()))
                        .w_full()
                        .min_w_0()
                        .child(
                            Input::new(&self.resource_input)
                                .small()
                                .w_full()
                                .min_w_0()
                                .aria_label(label)
                                .disabled(!self.client_connected),
                        ),
                );
                if destination == Destination::Jobs {
                    body = body
                        .child(readable(
                            "resource-schedule-label",
                            "Schedule (cron or plain English)",
                        ))
                        .child(
                            div()
                                .id("resource-schedule-field")
                                .test_support()
                                .anchor_scroll(Some(self.resource_anchors[11].clone()))
                                .w_full()
                                .min_w_0()
                                .child(
                                    Input::new(&self.resource_schedule)
                                        .small()
                                        .w_full()
                                        .min_w_0()
                                        .aria_label("Job schedule, cron or plain English")
                                        .disabled(!self.client_connected),
                                ),
                        );
                }
                if destination == Destination::Skills {
                    body = body.child(readable(
                        "skill-add-help",
                        "New skills install disabled for review.",
                    ));
                }
                body = body.child(
                    div()
                        .id("resource-add-anchor")
                        .w_full()
                        .anchor_scroll(Some(self.resource_anchors[9].clone()))
                        .child(
                            Button::new("resource-add")
                                .w_full()
                                .small()
                                .label(add)
                                .accessibility_label(add)
                                .track_focus(&self.resource_controls[9])
                                .disabled(!self.client_connected)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.submit_resource(destination, window, cx)
                                })),
                        ),
                );
                if let Some(error) = &self.resource_error {
                    body = body.child(readable("resource-validation-error", error.clone()));
                }
            }
        }
        body.into_any_element()
    }
}
