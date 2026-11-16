use super::*;
use formless_desktop::{
    client::{Command as Wire, Update},
    pending_files::Report,
};
use serde_json::{json, Value};
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    step(cx, h, |w, cx| {
        w.click(session_row_id(&sessions[0].id), cx);
        let generation = s.read(cx).client_generation();
        let id = sessions[0].id.clone();
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        &id,
                        &json!({"session":{"id":id},"messages":[],"running":false}),
                    )
                    .unwrap(),
                },
                w,
                cx,
            );
            s.focus_section(PageSection::Composer, w, cx);
        });
        w.input("exact native ไทย😀", cx);
        settle_frames(w, cx);
        s.update(cx, |s, _| s.take_client_commands());
    });
    (h, s)
}
fn present(
    h: WindowHandle<Root>,
    s: &Entity<Shell>,
    cx: &mut TestAppContext,
    prepared: Box<formless_desktop::forecast::Prepared>,
) {
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| {
            s.apply_client_update(Update::Prepared(prepared), w, cx)
        });
        settle_frames(w, cx);
    });
}
#[gpui_kit::test]
fn native_send_even_empty_inventory_and_no_price_threshold_require_explicit_confirmation(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let p = step(cx, h, |_, cx| daemon_quote(&s, cx, false));
    let id = p.id;
    present(h, &s, cx, p);
    step(cx, h, |w, cx| {
        assert!(s.read(cx).send_review_open());
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert_eq!(w.find("forecast-cancel").focused(), Some(true));
        w.press("tab", cx);
        settle_frames(w, cx);
        w.press("tab", cx);
        settle_frames(w, cx);
        assert_eq!(w.find("forecast-confirm").focused(), Some(true));
        w.press("space", cx);
        settle_frames(w, cx);
        let commands = s.update(cx, |s, _| s.take_client_commands());
        assert!(matches!(commands.as_slice(),[Wire::ConfirmPrepared(got)]if *got==id));
        assert_eq!(s.read(cx).draft(cx).as_ref(), "exact native ไทย😀");
    });
}
#[gpui_kit::test]
fn native_send_complete_metadata_and_draft_copy_preserves_all_rows_and_unicode_at_large_scale(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let mut p = step(cx, h, |_, cx| daemon_quote(&s, cx, false));
    let rows=(0..64).map(|n|json!({"id":format!("id-{n}"),"name":format!("name-{n} ไทย😀\u{202e}"),"path":format!("/private/full/{n}"),"mime":"text/plain","bytes":n,"future":{"kept":true}})).collect::<Vec<_>>();
    let raw = json!({"session_id":p.session_id,"attachments":rows});
    p.attachments = Some(Report::decode(&p.session_id, raw.clone()).unwrap());
    let last = p.summary().len() - 1;
    present(h, &s, cx, p);
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        settle_frames(w, cx);
        w.click("forecast-copy", cx);
        let text = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(!text.contains('\u{202e}'));
        let copied: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(copied["pending_files"], raw);
        assert_eq!(copied["draft"], "exact native ไทย😀");
        w.press("end", cx);
        settle_frames(w, cx);
        assert!(w
            .find(format!("forecast-detail-{last}"))
            .value()
            .unwrap()
            .contains("/private/full/63"));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        w.press("escape", cx);
        settle_frames(w, cx);
        assert!(!s.read(cx).send_review_open());
        assert_eq!(s.read(cx).draft(cx).as_ref(), "exact native ไทย😀");
    });
}
#[gpui_kit::test]
fn native_send_foreign_profile_or_inventory_cannot_commit_or_cancel_a_newer_review(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let current = step(cx, h, |_, cx| daemon_quote(&s, cx, false));
    let mut foreign = current.clone();
    foreign.id = uuid::Uuid::new_v4();
    foreign.client = uuid::Uuid::new_v4();
    present(h, &s, cx, current.clone());
    present(h, &s, cx, foreign);
    step(cx, h, |w, cx| {
        assert!(s.read(cx).send_review_open());
        cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        s.update(cx, |s, cx| {
            assert!(!s.copy_send_review_for_test(uuid::Uuid::new_v4(), w, cx));
            s.cancel_send_review_for_test(uuid::Uuid::new_v4(), w, cx);
        });
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "sentinel"
        );
        assert!(s.read(cx).send_review_open());
        w.click("forecast-confirm", cx);
        settle_frames(w, cx);
        assert!(
            matches!(s.update(cx,|s,_|s.take_client_commands()).as_slice(),[Wire::ConfirmPrepared(id)]if *id==current.id)
        );
        w.press("escape", cx);
        settle_frames(w, cx);
    });
    let mut foreign = step(cx, h, |_, cx| daemon_quote(&s, cx, false));
    foreign.attachments =
        Some(Report::decode("other", json!({"session_id":"other","attachments":[]})).unwrap());
    present(h, &s, cx, foreign);
    step(cx, h, |_, cx| {
        assert!(!s.read(cx).send_review_open());
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
