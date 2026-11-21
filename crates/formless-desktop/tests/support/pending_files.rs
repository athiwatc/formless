use super::*;
use formless_desktop::{
    client::{Command as Wire, Update},
    pending_files::Report,
};
use serde_json::{json, Value};
fn activate(w: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..100 {
        settle_frames(w, cx);
        if w.find(id).focused() == Some(true) {
            assert!(w.find(id).visible());
            w.press("space", cx);
            return;
        }
        w.press("tab", cx);
    }
    panic!("pending-file control unreachable: {id}");
}
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>, String) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    super::session_edit_tests::show(cx, h, &s, &sessions[1]);
    step(cx, h, |_, cx| {
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty())
    });
    (h, s, sessions[1].id.clone())
}
fn value(id: &str, count: usize) -> Value {
    json!({"session_id":id,"attachments":(0..count).map(|n|json!({"id":format!("a{n}"),"name":format!("file-{n:02} ไทย😀"),"mime":"text/plain","path":format!("/metadata/not-read/{n}"),"bytes":n,"future":{"kept":true}})).collect::<Vec<_>>()})
}
fn request(
    cx: &mut TestAppContext,
    h: WindowHandle<Root>,
    s: &Entity<Shell>,
    id: &str,
) -> (uuid::Uuid, uuid::Uuid) {
    step(cx, h, |w, cx| {
        activate(w, cx, "pending-files-read");
        let commands = s.update(cx, |s, _| s.take_client_commands());
        assert_eq!(commands.len(), 1);
        let Wire::ReadPendingAttachments {
            connection: 0,
            operation,
            session_id,
        } = &commands[0]
        else {
            panic!("explicit pending read")
        };
        assert_eq!(session_id, id);
        (s.read(cx).pending_files_instance(), *operation)
    })
}
fn reply(
    cx: &mut TestAppContext,
    h: WindowHandle<Root>,
    s: &Entity<Shell>,
    id: &str,
    owner: (uuid::Uuid, uuid::Uuid),
    raw: Value,
) {
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::PendingAttachments {
                    client: owner.0,
                    connection: 0,
                    operation: owner.1,
                    session_id: id.into(),
                    result: Report::decode(id, raw),
                },
                w,
                cx,
            )
        })
    });
}
#[gpui_kit::test]
fn pending_files_are_explicit_scoped_paginated_and_copy_all_metadata(cx: &mut TestAppContext) {
    let (h, s, id) = setup(cx);
    let owner = request(cx, h, &s, &id);
    let raw = value(&id, 64);
    reply(cx, h, &s, &id, owner, raw.clone());
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        assert_eq!(s.read(cx).pending_files_count(), Some(64));
        for page in 2..=8 {
            activate(w, cx, "pending-files-next");
            assert!(w
                .find("pending-files-count")
                .value()
                .unwrap()
                .contains(&format!("page {page} of 8")));
        }
        assert_eq!(w.find("pending-files-next").disabled(), Some(true));
        activate(w, cx, "pending-files-copy");
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&copied).unwrap(), raw);
        assert!(
            s.update(cx, |s, _| s.take_client_commands()).is_empty(),
            "paging/copy must neither attach/detach nor send"
        );
    });
}
#[gpui_kit::test]
fn pending_files_stale_copy_and_reload_cannot_target_new_metadata(cx: &mut TestAppContext) {
    let (h, s, id) = setup(cx);
    let first = request(cx, h, &s, &id);
    reply(cx, h, &s, &id, first, value(&id, 1));
    let (form, revision) = step(cx, h, |_, cx| {
        (
            s.read(cx).session_edit_revision().unwrap(),
            s.read(cx).pending_files_revision().unwrap(),
        )
    });
    let second = request(cx, h, &s, &id);
    reply(cx, h, &s, &id, first, value(&id, 9));
    assert_eq!(step(cx, h, |_, cx| s.read(cx).pending_files_count()), None);
    reply(cx, h, &s, &id, second, value(&id, 2));
    step(cx, h, |w, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        s.update(cx, |s, cx| {
            s.copy_presented_pending_files(form, revision, cx)
        });
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "sentinel"
        );
        activate(w, cx, "session-edit-reload");
        assert!(s.read(cx).pending_files_revision().is_none());
    });
    reply(cx, h, &s, &id, second, value(&id, 3));
    assert_eq!(step(cx, h, |_, cx| s.read(cx).pending_files_count()), None);
}
#[gpui_kit::test]
fn pending_files_composition_and_profile_replacement_retire_reads(cx: &mut TestAppContext) {
    let (h, s, id) = setup(cx);
    step(cx, h, |w, cx| {
        let form = s.read(cx).session_edit_revision().unwrap();
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| {
            i.replace_and_mark_text_in_range(None, "かな", None, w, cx)
        });
        s.update(cx, |s, cx| s.read_presented_pending_files(form, w, cx));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        input.update(cx, |i, cx| i.unmark_text(w, cx));
    });
    let old = request(cx, h, &s, &id);
    let _ = daemon_view(cx, h, &s);
    reply(cx, h, &s, &id, old, value(&id, 1));
    step(cx, h, |w, cx| {
        assert_eq!(s.read(cx).pending_files_count(), None);
        assert_eq!(w.find("pending-files-read").disabled(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn pending_files_view_rejects_a_foreign_typed_report_even_with_matching_envelope(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    let owner = request(cx, h, &s, &id);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::PendingAttachments {
                    client: owner.0,
                    connection: 0,
                    operation: owner.1,
                    session_id: id.clone(),
                    result: Report::decode("foreign", value("foreign", 1)),
                },
                w,
                cx,
            )
        });
        assert_eq!(s.read(cx).pending_files_count(), None);
        assert_eq!(s.read(cx).pending_files_revision(), Some(owner.1));
        settle_frames(w, cx);
        assert_eq!(w.find("pending-files-copy").disabled(), Some(true));
        assert!(w
            .find("pending-files-notice")
            .value()
            .unwrap()
            .contains("unavailable"));
    });
}
