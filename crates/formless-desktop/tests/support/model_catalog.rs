use super::*;
use formless_desktop::{
    client::{Command as Wire, Update},
    model_catalog::Catalog,
};
use serde_json::json;
pub(super) fn click(w: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..100 {
        settle_frames(w, cx);
        if w.find(id).focused() == Some(true) {
            assert!(w.find(id).visible());
            w.press("enter", cx);
            return;
        }
        w.press("tab", cx);
    }
    panic!("catalog control not keyboard reachable: {id}");
}
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    super::session_edit_tests::show(cx, h, &s, &sessions[1]);
    step(cx, h, |w, cx| {
        click(w, cx, "session-edit-model");
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
    (h, s)
}
fn read(
    h: WindowHandle<Root>,
    s: &Entity<Shell>,
    cx: &mut TestAppContext,
) -> (uuid::Uuid, uuid::Uuid) {
    step(cx, h, |w, cx| {
        click(w, cx, "session-catalog-read");
        let cmds = s.update(cx, |s, _| s.take_client_commands());
        assert_eq!(cmds.len(), 1);
        let Wire::ReadModelCatalog {
            operation,
            connection: 0,
        } = cmds[0]
        else {
            panic!("explicit catalog read")
        };
        (s.read(cx).session_catalog_instance(), operation)
    })
}
fn reply(
    h: WindowHandle<Root>,
    s: &Entity<Shell>,
    cx: &mut TestAppContext,
    owner: (uuid::Uuid, uuid::Uuid),
    value: Catalog,
) {
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ModelCatalog {
                    instance: owner.0,
                    connection: 0,
                    operation: owner.1,
                    result: Ok(value),
                },
                w,
                cx,
            )
        })
    });
}
fn catalog() -> Catalog {
    Catalog::decode(json!({"models":(0..27).map(|i|format!("model-{i:02}")).collect::<Vec<_>>(),"aliases":{"alias":"model-01"},"roles":[{"name":"reviewer"}],"recent":[],"error":null})).unwrap()
}
#[gpui_kit::test]
fn catalog_reads_only_on_explicit_click_pages_filters_and_fills_without_saving(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let owner = read(h, &s, cx);
    reply(h, &s, cx, owner, catalog());
    step(cx, h, |w, cx| {
        assert!(w
            .find("session-catalog-count")
            .value()
            .unwrap()
            .contains("29 matching choices"));

        click(w, cx, "session-catalog-next");
        assert!(w
            .find("session-catalog-count")
            .value()
            .unwrap()
            .contains("page 2 of 3"));
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| i.set_value("REVIEW", w, cx));
    });
    step(cx, h, |w, cx| {
        assert!(w
            .find("session-catalog-count")
            .value()
            .unwrap()
            .contains("1 matching choices"));
        click(w, cx, "session-catalog-choice-0");
        assert_eq!(
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "role:reviewer"
        );
        assert!(
            s.update(cx, |s, _| s.take_client_commands()).is_empty(),
            "typing, filtering and filling must not request network or save"
        );
        assert!(w.find("session-model-paid-footer").visible());
        click(w, cx, "session-edit-save");
        let cmds = s.update(cx, |s, _| s.take_client_commands());
        assert!(
            matches!(&cmds[..],[Wire::EditSession{change:formless_desktop::session_edit::Change::Selection{desired,..},..}]if desired.model.as_deref()==Some("role:reviewer"))
        );
    });
}
#[gpui_kit::test]
fn catalog_stale_choice_query_field_reload_and_ime_never_overwrite_new_draft(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let owner = read(h, &s, cx);
    reply(h, &s, cx, owner, catalog());
    step(cx, h, |w, cx| {
        let form = s.read(cx).session_edit_revision().unwrap();
        let rev = s.read(cx).session_catalog_revision().unwrap();
        let input = s.read(cx).session_edit_input().unwrap();
        let basis = input.read(cx).value().to_string();
        input.update(cx, |i, cx| i.set_value("new query", w, cx));
        s.update(cx, |s, cx| {
            s.fill_presented_session_catalog(form, rev, &basis, "model-01", w, cx)
        });
        assert_eq!(input.read(cx).value().as_ref(), "new query");
        input.update(cx, |i, cx| {
            i.replace_and_mark_text_in_range(None, "ไทย", None, w, cx)
        });
        let marked = input.read(cx).value().to_string();
        s.update(cx, |s, cx| {
            s.fill_presented_session_catalog(form, rev, &marked, "model-01", w, cx)
        });
        assert_eq!(input.read(cx).value().as_ref(), marked);
        input.update(cx, |i, cx| i.unmark_text(w, cx));
        click(w, cx, "session-edit-effort");
        click(w, cx, "session-edit-model");
        s.update(cx, |s, cx| {
            s.fill_presented_session_catalog(form, rev, &marked, "model-01", w, cx)
        });
        assert_eq!(
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            marked
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        click(w, cx, "session-edit-reload");
        assert!(s.read(cx).session_catalog_revision().is_none());
    });
}
#[gpui_kit::test]
fn catalog_profile_replacement_rejects_old_read_and_requires_reload(cx: &mut TestAppContext) {
    let (h, s) = setup(cx);
    let old = read(h, &s, cx);
    let _ = daemon_view(cx, h, &s);
    reply(h, &s, cx, old, catalog());
    step(cx, h, |w, cx| {
        assert!(s.read(cx).session_catalog_revision().is_none());
        assert_eq!(w.find("session-catalog-read").disabled(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn catalog_keyboard_at_200_percent_preserves_undo_and_refuses_stale_read(cx: &mut TestAppContext) {
    let (h, s) = setup(cx);
    step(cx, h, |w, cx| {
        s.read(cx)
            .session_edit_input()
            .unwrap()
            .update(cx, |i, cx| i.set_value("ไทย😀", w, cx))
    });
    let owner = read(h, &s, cx);
    reply(h, &s, cx, owner, catalog());
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        let input = s.read(cx).session_edit_input().unwrap();
        let before = input.read(cx).value().to_string();
        click(w, cx, "session-catalog-choice-0");
        assert_eq!(input.read(cx).value().as_ref(), "alias");
        w.press(&chord("z"), cx);
        assert_eq!(
            input.read(cx).value().as_ref(),
            before,
            "fill must preserve native undo"
        );
        assert!(w.find("session-model-paid-footer").visible());
        let old = s.read(cx).session_edit_revision().unwrap();
        click(w, cx, "session-edit-effort");
        click(w, cx, "session-edit-model");
        s.update(cx, |s, cx| s.read_presented_session_catalog(old, w, cx));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn catalog_read_can_complete_while_another_field_is_shown_but_not_after_reload(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let owner = read(h, &s, cx);
    step(cx, h, |w, cx| click(w, cx, "session-edit-effort"));
    reply(h, &s, cx, owner, catalog());
    let second = step(cx, h, |w, cx| {
        assert_eq!(s.read(cx).session_catalog_revision(), Some(owner.1));
        click(w, cx, "session-edit-model");
        click(w, cx, "session-catalog-read");
        let commands = s.update(cx, |s, _| s.take_client_commands());
        let [Wire::ReadModelCatalog { operation, .. }] = &commands[..] else {
            panic!("second read")
        };
        let second = (s.read(cx).session_catalog_instance(), *operation);
        click(w, cx, "session-edit-reload");
        assert!(s.read(cx).session_catalog_revision().is_none());
        second
    });
    reply(h, &s, cx, second, catalog());
    step(cx, h, |_, cx| {
        assert!(s.read(cx).session_catalog_revision().is_none())
    });
}

#[gpui_kit::test]
fn catalog_queued_choice_cannot_fill_a_different_displayed_page(cx: &mut TestAppContext) {
    let (h, s) = setup(cx);
    let owner = read(h, &s, cx);
    reply(h, &s, cx, owner, catalog());
    step(cx, h, |w, cx| {
        let ticket = s.read(cx).capture_catalog_choice("alias", cx);
        let input = s.read(cx).session_edit_input().unwrap();
        let before = input.read(cx).value().to_string();
        click(w, cx, "session-catalog-next");
        s.update(cx, |s, cx| s.fill_captured_catalog_choice(ticket, w, cx));
        assert_eq!(
            input.read(cx).value().as_ref(),
            before,
            "a queued choice from the previous catalog page must not overwrite the current draft"
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
