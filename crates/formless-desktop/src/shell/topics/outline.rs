//! An editable outline, never a client-side execution plan.
use super::*;
use formless_protocol::{StepAccess, TopicStepOutline};

pub(super) struct Step {
    pub id: uuid::Uuid,
    pub title: Entity<InputState>,
    pub instructions: Entity<InputState>,
    pub harness: Option<HarnessKind>,
    pub acp_agent: Entity<InputState>,
    pub model: Entity<InputState>,
    pub access: StepAccess,
    pub expanded: bool,
}

impl Step {
    pub fn new(outline: TopicStepOutline, window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let input =
            |value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Shell>| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                })
            };
        Self {
            id: uuid::Uuid::new_v4(),
            title: input(outline.title, "Step title", window, cx),
            instructions: input(outline.instructions, "Instructions (optional)", window, cx),
            harness: outline.harness,
            acp_agent: input(
                outline.acp_agent.unwrap_or_default(),
                "Configured ACP agent",
                window,
                cx,
            ),
            model: input(
                outline.model.unwrap_or_default(),
                "Model (optional; let Formless choose)",
                window,
                cx,
            ),
            access: outline.access,
            expanded: false,
        }
    }

    pub fn value(&self, cx: &App) -> TopicStepOutline {
        let model = self.model.read(cx).value().trim().to_string();
        let acp_agent = self.acp_agent.read(cx).value().trim().to_string();
        TopicStepOutline {
            title: self.title.read(cx).value().trim().to_string(),
            instructions: self.instructions.read(cx).value().trim().to_string(),
            harness: self.harness,
            acp_agent: (self.harness == Some(HarnessKind::Acp) && !acp_agent.is_empty())
                .then_some(acp_agent),
            model: (!model.is_empty()).then_some(model),
            access: self.access,
        }
    }
}

pub(super) fn editor(ui: &Ui, busy: bool, cx: &mut Context<Shell>) -> AnyElement {
    let mut editor = v_flex().id("topic-outline-editor").test_support().gap_2().min_w_0()
        .child(readable("topic-outline-help", "Suggested steps guide the Formless model. It decides the next steps and may adapt the outline.").text_sm())
        .child(h_flex().flex_wrap().gap_2()
            .child(Button::new("topic-outline-auto").small().outline().label("Let Formless plan").selected(ui.outline.is_empty()).disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| { this.topic_ui.outline.clear(); cx.notify(); })))
            .child(Button::new("topic-outline-code").small().outline().label("Code preset").disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.set_topic_outline(topics::Preset::Code, window, cx))))
            .child(Button::new("topic-outline-accounting").small().outline().label("Accounting preset").disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.set_topic_outline(topics::Preset::Accounting, window, cx)))));
    for (index, step) in ui.outline.iter().enumerate() {
        let id = step.id;
        let mut card = v_flex()
            .id(SharedString::from(format!("topic-step-{id}")))
            .test_support()
            .gap_2()
            .min_w_0()
            .border_l_2()
            .border_color(cx.theme().border)
            .pl_2()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Label::new(format!("Step {}", index + 1)).text_xs())
                    .child(
                        Button::new(SharedString::from(format!("topic-step-up-{id}")))
                            .small()
                            .ghost()
                            .label("Earlier")
                            .disabled(busy || index == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(index) =
                                    this.topic_ui.outline.iter().position(|step| step.id == id)
                                {
                                    if index > 0 {
                                        this.topic_ui.outline.swap(index, index - 1);
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topic-step-down-{id}")))
                            .small()
                            .ghost()
                            .label("Later")
                            .disabled(busy || index + 1 == ui.outline.len())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(index) =
                                    this.topic_ui.outline.iter().position(|step| step.id == id)
                                {
                                    if index + 1 < this.topic_ui.outline.len() {
                                        this.topic_ui.outline.swap(index, index + 1);
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topic-step-remove-{id}")))
                            .small()
                            .ghost()
                            .label("Remove")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.topic_ui.outline.retain(|step| step.id != id);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("topic-step-edit-{id}")))
                            .small()
                            .ghost()
                            .label(if step.expanded { "Less" } else { "Edit" })
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(step) =
                                    this.topic_ui.outline.iter_mut().find(|step| step.id == id)
                                {
                                    step.expanded = !step.expanded;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Input::new(&step.title)
                    .small()
                    .aria_label(format!("Step {} title", index + 1)),
            );
        if !step.expanded {
            editor = editor.child(
                card.child(
                    Label::new(format!(
                        "{}{} · {}",
                        topics::chosen_harness(step.harness, step.value(cx).acp_agent.as_deref()),
                        step.value(cx)
                            .model
                            .map(|model| format!(" / {model}"))
                            .unwrap_or_default(),
                        topics::access(step.access)
                    ))
                    .text_xs(),
                ),
            );
            continue;
        }
        card = card.child(
            Input::new(&step.instructions)
                .small()
                .aria_label(format!("Step {} instructions", index + 1)),
        );
        let mut harnesses = h_flex().flex_wrap().gap_2();
        for kind in std::iter::once(None).chain(topics::HARNESSES.into_iter().map(Some)) {
            harnesses = harnesses.child(
                Button::new(SharedString::from(format!(
                    "topic-step-harness-{id}-{kind:?}"
                )))
                .small()
                .outline()
                .label(kind.map(topics::harness).unwrap_or("Formless chooses"))
                .selected(step.harness == kind)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(step) = this.topic_ui.outline.iter_mut().find(|step| step.id == id)
                    {
                        step.harness = kind;
                    }
                    cx.notify();
                })),
            );
        }
        card = card.child(harnesses).child(
            Input::new(&step.model)
                .small()
                .aria_label(format!("Step {} model", index + 1)),
        );
        if step.harness == Some(HarnessKind::Acp) {
            card = card.child(
                Input::new(&step.acp_agent)
                    .small()
                    .aria_label(format!("Step {} ACP agent", index + 1)),
            );
        }
        let mut permissions = h_flex().flex_wrap().gap_2();
        for access in [StepAccess::ReadOnly, StepAccess::Write] {
            permissions = permissions.child(
                Button::new(SharedString::from(format!(
                    "topic-step-access-{id}-{access:?}"
                )))
                .small()
                .outline()
                .label(topics::access(access))
                .selected(step.access == access)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(step) = this.topic_ui.outline.iter_mut().find(|step| step.id == id)
                    {
                        step.access = access;
                    }
                    cx.notify();
                })),
            );
        }
        editor = editor.child(card.child(permissions));
    }
    editor
        .child(
            Button::new("topic-outline-add")
                .small()
                .outline()
                .label("Add suggested step")
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    let title = format!("Step {}", this.topic_ui.outline.len() + 1);
                    let mut step = Step::new(
                        TopicStepOutline {
                            title,
                            instructions: String::new(),
                            harness: None,
                            acp_agent: None,
                            model: None,
                            access: StepAccess::ReadOnly,
                        },
                        window,
                        cx,
                    );
                    step.expanded = true;
                    this.topic_ui.outline.push(step);
                    cx.notify();
                })),
        )
        .into_any_element()
}
