//! Explicit placement overrides; the daemon resolves the default source context.
use super::*;

pub(super) fn editor(ui: &Ui, busy: bool, cx: &mut Context<Shell>) -> AnyElement {
    let context = ui
        .state
        .for_session(ui.source.as_deref())
        .map(|topic| topic.name.as_str())
        .unwrap_or("top level");
    let selected = ui
        .parent
        .as_deref()
        .map(|id| {
            ui.state
                .topics
                .iter()
                .find(|topic| topic.id == id)
                .map(|topic| topic.name.clone())
                .unwrap_or_else(|| "Unavailable parent".into())
        })
        .unwrap_or_else(|| format!("Use source context · {context}"));
    let mut panel = v_flex().gap_2().min_w_0().child(
        Button::new("topic-parent-open")
            .small()
            .outline()
            .label(format!("Group under: {selected}"))
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| {
                this.topic_ui.choosing_parent = !this.topic_ui.choosing_parent;
                cx.notify();
            })),
    );
    if ui.choosing_parent {
        let mut choices = v_flex()
            .id("topic-parent-choices")
            .test_support()
            .gap_1()
            .min_w_0()
            .max_h(px(160.))
            .overflow_y_scroll()
            .track_scroll(&ui.parent_scroll)
            .child(
                Button::new("topic-parent-source")
                    .small()
                    .ghost()
                    .label(format!("Use source context · {context}"))
                    .selected(ui.parent.is_none())
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.topic_ui.parent = None;
                        this.topic_ui.choosing_parent = false;
                        cx.notify();
                    })),
            );
        for (_, topic) in ui.state.tree() {
            if topic.status == TopicStatus::PendingApproval {
                continue;
            }
            let id = topic.id.clone();
            let path = ui
                .state
                .breadcrumbs(&id)
                .iter()
                .map(|topic| topic.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            choices = choices.child(
                Button::new(SharedString::from(format!("topic-parent-{id}")))
                    .small()
                    .ghost()
                    .label(path)
                    .selected(ui.parent.as_deref() == Some(id.as_str()))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.topic_ui.parent = Some(id.clone());
                        this.topic_ui.choosing_parent = false;
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(choices);
    }
    panel.into_any_element()
}
