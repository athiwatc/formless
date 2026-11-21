use super::*;

pub(super) struct Editor {
    pub topic: String,
    pub pattern: Entity<InputState>,
    pub kind: Entity<InputState>,
    pub acp_agent: Entity<InputState>,
    pub retries: Entity<InputState>,
    pub attempts: Entity<InputState>,
    pub harness: Option<HarnessKind>,
    pub allow: bool,
}

impl Editor {
    pub fn composing(&self, window: &mut Window, cx: &mut Context<Shell>) -> bool {
        [
            &self.pattern,
            &self.kind,
            &self.acp_agent,
            &self.retries,
            &self.attempts,
        ]
        .iter()
        .any(|input| {
            input.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
        })
    }
    pub fn new(
        topic: String,
        retries: u32,
        attempts: u32,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Self {
        Self {
            topic,
            pattern: input("", "Pattern from the permission request", window, cx),
            kind: input("", "Request type (optional)", window, cx),
            acp_agent: input("", "ACP agent (optional; any agent)", window, cx),
            retries: input(&retries.to_string(), "Retries per step", window, cx),
            attempts: input(&attempts.to_string(), "Attempts per topic", window, cx),
            harness: None,
            allow: false,
        }
    }
}

fn input(
    value: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder.to_owned())
            .default_value(value.to_owned())
    })
}

pub(super) fn field(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    busy: bool,
) -> AnyElement {
    v_flex()
        .id(id)
        .test_support()
        .gap_1()
        .min_w_0()
        .child(Label::new(label).text_xs())
        .child(Input::new(input).small().aria_label(label).disabled(busy))
        .into_any_element()
}

pub(super) fn toggle(
    ui: &Ui,
    topic: &formless_protocol::TopicInfo,
    busy: bool,
    cx: &mut Context<Shell>,
) -> Button {
    let id = topic.id.clone();
    Button::new("topic-controls-toggle")
        .small()
        .outline()
        .label("Topic controls")
        .selected(
            ui.policy
                .as_ref()
                .is_some_and(|editor| editor.topic == topic.id),
        )
        .disabled(busy)
        .on_click(cx.listener(move |this, _, window, cx| this.toggle_topic_policy(&id, window, cx)))
}

pub(super) fn editor(
    ui: &Ui,
    topic: &formless_protocol::TopicInfo,
    busy: bool,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let Some(editor) = ui.policy.as_ref().filter(|editor| editor.topic == topic.id) else {
        return div().into_any_element();
    };
    let id = topic.id.clone();
    let allow = !topic.allow_external_context;
    let mut panel = v_flex().id("topic-controls").test_support().gap_2().min_w_0()
        .child(Label::new("Conversation access").font_semibold().text_sm())
        .child(readable("topic-context-scope", if topic.allow_external_context {
            "Wider conversation access is allowed in this profile."
        } else {
            "Conversation access is limited to this topic and its ancestors."
        }).text_sm())
        .child(Button::new("topic-context-toggle").small().outline()
            .label(if allow { "Allow wider conversation access" } else { "Restrict to this branch" })
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| this.topic_action(Action::SetContext { topic: id.clone(), allow }, cx))))
        .child(Label::new("Saved permission rules").font_semibold().text_sm())
        .child(readable("topic-rules-help", "Only you create or remove these rules. They decide matching future requests in this topic, across attempts; reject rules take precedence. Saved rules never decide workspace trust. ‘Allow always’ on a prompt remains attempt-only and never creates a rule.").text_sm())
        .child(readable("topic-pattern-help", "Patterns match the entire permission title or command, case-sensitively. * matches any text; ? matches one character. For example: *cargo test*.").text_xs());
    for rule in &topic.permission_rules {
        let id = topic.id.clone();
        let rule_id = rule.rule_id.clone();
        panel = panel.child(
            v_flex()
                .gap_1()
                .min_w_0()
                .child(
                    readable(
                        SharedString::from(format!("topic-rule-{}", rule.rule_id)),
                        topics::rule_summary(rule),
                    )
                    .text_sm(),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "topic-rule-remove-{}",
                        rule.rule_id
                    )))
                    .small()
                    .outline()
                    .label("Remove rule")
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.topic_action(
                            Action::RemoveRule {
                                topic: id.clone(),
                                rule: rule_id.clone(),
                            },
                            cx,
                        )
                    })),
                ),
        );
    }
    if topic.permission_rules.is_empty() {
        panel = panel.child(
            readable(
                "topic-no-rules",
                "No saved rules. Unmatched permissions wait for you.",
            )
            .text_xs(),
        );
    }
    let mut harnesses = h_flex().flex_wrap().gap_2();
    for harness in std::iter::once(None).chain(topics::HARNESSES.into_iter().map(Some)) {
        harnesses = harnesses.child(
            Button::new(SharedString::from(format!(
                "topic-rule-harness-{harness:?}"
            )))
            .small()
            .outline()
            .label(harness.map(topics::harness).unwrap_or("Any harness"))
            .selected(editor.harness == harness)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(editor) = &mut this.topic_ui.policy {
                    editor.harness = harness;
                }
                cx.notify();
            })),
        );
    }
    panel = panel.child(harnesses);
    if editor.harness == Some(HarnessKind::Acp) {
        panel = panel.child(field(
            "topic-rule-agent",
            "ACP agent (optional)",
            &editor.acp_agent,
            busy,
        ));
    }
    panel = panel
        .child(field("topic-rule-kind", "Permission type (optional)", &editor.kind, busy))
        .child(field("topic-rule-pattern", "Request pattern", &editor.pattern, busy))
        .child(h_flex().gap_2().flex_wrap()
            .child(Button::new("topic-rule-allow").small().outline().label("Allow matching requests")
                .selected(editor.allow).disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(editor) = &mut this.topic_ui.policy { editor.allow = true; } cx.notify();
                })))
            .child(Button::new("topic-rule-reject").small().outline().label("Reject matching requests")
                .selected(!editor.allow).disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(editor) = &mut this.topic_ui.policy { editor.allow = false; } cx.notify();
                }))))
        .child(Button::new("topic-rule-add").small().outline().label("Save rule")
            .disabled(busy || editor.pattern.read(cx).value().trim().is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.save_topic_rule(window, cx))))
        .child(Label::new("Attempt limits").font_semibold().text_sm())
        .child(readable("topic-current-limits", format!("Saved: {} retries per step · {} attempts per topic. Reaching a cap pauses for your decision.", topic.limits.max_retries_per_step, topic.limits.max_attempts_per_topic)).text_sm())
        .child(readable("topic-zero-limits", "0 retries allows the first attempt only; 0 attempts prevents all launches.").text_xs())
        .child(field("topic-limit-retries", "Retries per step", &editor.retries, busy))
        .child(field("topic-limit-attempts", "Attempts per topic", &editor.attempts, busy))
        .child(Button::new("topic-limits-save").small().outline().label("Save limits").disabled(busy)
            .on_click(cx.listener(|this, _, window, cx| this.save_topic_limits(window, cx))));
    panel.into_any_element()
}

pub(super) fn availability(ui: &Ui, busy: bool, cx: &mut Context<Shell>) -> AnyElement {
    let mut panel = v_flex()
        .id("topic-harness-availability")
        .test_support()
        .gap_1()
        .min_w_0()
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(Label::new("Local harness readiness").text_sm())
                .child(
                    Button::new("topic-harness-refresh")
                        .small()
                        .outline()
                        .label("Check harnesses")
                        .disabled(busy)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.topic_action(Action::Harnesses, cx)),
                        ),
                ),
        );
    if ui.state.harnesses.is_empty() {
        panel = panel.child(readable("topic-harness-unchecked", "Readiness has not been loaded. Formless checks before planning and asks you before using a substitute.").text_xs());
    }
    for (index, harness) in ui.state.harnesses.iter().enumerate() {
        panel = panel.child(
            readable(
                SharedString::from(format!("topic-harness-ready-{index}")),
                topics::harness_availability(harness),
            )
            .text_xs(),
        );
    }
    panel.into_any_element()
}
