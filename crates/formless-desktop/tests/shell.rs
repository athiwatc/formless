//! Headless GPUI Kit tests of the Phase 0 shell.
//!
//! These drive the real `Shell` view in the kit's test window: real hit
//! testing, key dispatch, focus and layout. They do not prove pixels, OS IME
//! behaviour, a screen reader, or anything about a Daemon Core.

use std::time::{Duration, Instant};

use formless_desktop::fixture;
use formless_desktop::memory::{limits, IntentKind, IntentRecord};
use formless_desktop::model::{tokens, Destination, TextScale};
use formless_desktop::shell::{self, session_row_id, Shell};
use formless_desktop::timeline::TimelineBlock;
use formless_protocol::SendMode;
use gpui_kit::base::test_support::texts;
use gpui_kit::base::Root;
use gpui_kit::test::{TestInput, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, size, App, AppContext as _, Bounds, ClipboardItem, ElementInputHandler, Entity,
    Pixels, Point, Role, ScrollDelta, Size, TestAppContext, Window, WindowBounds, WindowHandle,
    WindowOptions,
};

const WIDE: Size<Pixels> = Size {
    width: px(1440.),
    height: px(900.),
};

fn open(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
) -> (WindowHandle<Root>, Entity<Shell>) {
    open_with(cx, window_size, sessions, blocks, true)
}

/// `with_running: false` leaves every fixture Session idle, which changes
/// how the sidebar groups and orders them.
fn open_with(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
    with_running: bool,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);
        let sessions = fixture::sessions(sessions);
        let running = if with_running {
            fixture::running(&sessions)
        } else {
            Default::default()
        };
        let blocks = fixture::timeline(blocks);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

#[cfg(target_os = "macos")]
const TOGGLE_SIDEBAR: &str = "cmd-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_SIDEBAR: &str = "ctrl-b";
#[cfg(target_os = "macos")]
const TOGGLE_INSPECTOR: &str = "cmd-alt-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_INSPECTOR: &str = "ctrl-alt-b";

/// One window update. Events a control emits (a composer Enter, for one) are
/// delivered when the update ends, so an assertion about their effect belongs
/// in the next step.
fn step<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    update: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle.into(), |_, window, cx| update(window, cx))
        .expect("shell window is open")
}

#[gpui_kit::test]
fn wide_window_docks_three_panes_at_their_token_widths(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar").bounds();
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(sidebar.size.width, px(tokens::SIDEBAR_DEFAULT));
        assert_eq!(inspector.size.width, px(tokens::INSPECTOR_DEFAULT));
        assert_eq!(sidebar.left(), px(0.));
        assert_eq!(conversation.left(), sidebar.right());
        assert_eq!(inspector.left(), conversation.right());
        assert_eq!(inspector.right(), px(1440.));
        assert!(window.find("header").visible());
        assert!(window.find("status").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn narrowing_hides_the_inspector_until_asked_then_collapses_the_sidebar(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Narrowing alone puts nothing over the conversation or its controls.
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.find("region-sidebar").visible());
        assert_eq!(
            window.find("region-conversation").bounds().right(),
            px(1100.)
        );

        // Asked for, it comes up as an overlay: the conversation keeps its
        // width and the inspector covers its right edge.
        window.click("toggle-inspector", cx);
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(conversation.right(), px(1100.));
        assert_eq!(inspector.right(), px(1100.));
        assert!(inspector.left() < conversation.right());
        // The control that dismisses it stays outside the overlay.
        let toggle = window.find("toggle-inspector").bounds();
        assert!(toggle.bottom() <= inspector.top());
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
    });

    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("composer-primary").visible());
    });

    // Widening restores the docked inspector: that preference never changed.
    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("region-inspector").visible());
        assert!(window.find("region-sidebar").visible());
    });
}

#[gpui_kit::test]
fn composer_takes_thai_cjk_and_emoji_and_keeps_the_draft_after_a_recorded_send(
    cx: &mut TestAppContext,
) {
    let text = "สวัสดีครับ 日本語のテスト 한국어 👩🏽‍💻";
    let draft = format!("{text}\nsecond line");
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The composer has focus from the start, so typing needs no click.
        let composer = window.find_by_label("Message");
        assert_eq!(composer.focused(), Some(true));
        assert_eq!(composer.role(), Some(Role::MultilineTextInput));
        // Nothing to send yet.
        window.press("enter", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).last_intent(), None);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        window.input(text, cx);
        assert_eq!(window.find_by_label("Message").value(), Some(text));
        window.press("shift-enter", cx);
        window.input("second line", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(shell.read(cx).last_intent(), None);
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(
            shell.read(cx).last_intent(),
            // The record has the sizes of what was sent and none of its text.
            Some(&IntentRecord {
                kind: IntentKind::Send(SendMode::Send),
                // The pinned fixture Session sorts first and opens at start.
                session_id: "fixture-session-3".to_string(),
                content_bytes: draft.len(),
                content_chars: draft.chars().count(),
            })
        );
        // Nothing acknowledged the request, so the draft is still there.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
    });
}

#[gpui_kit::test]
fn a_running_session_offers_steer_queue_and_a_separate_stop(cx: &mut TestAppContext) {
    #[cfg(target_os = "macos")]
    const SECONDARY_ENTER: &str = "cmd-enter";
    #[cfg(not(target_os = "macos"))]
    const SECONDARY_ENTER: &str = "ctrl-enter";

    let (handle, shell) = open(cx, WIDE, 200, 40);
    let last_mode = |cx: &App| match shell.read(cx).last_intent().map(|record| record.kind) {
        Some(IntentKind::Send(mode)) => Some(mode),
        _ => None,
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Idle Session: Send only, Stop present but unavailable.
        assert_eq!(window.find("composer-primary").label(), Some("Send"));
        assert!(window.try_find("composer-secondary").is_none());
        let stop = window.find("composer-stop").bounds();
        window.click("composer-stop", cx);
        assert_eq!(shell.read(cx).last_intent(), None);

        // Fixture Session 7 is running.
        window.click(session_row_id("fixture-session-7"), cx);
        assert_eq!(window.find("composer-primary").label(), Some("Steer"));
        assert_eq!(window.find("composer-secondary").label(), Some("Queue"));
        // Stop did not move when the primary action changed.
        assert_eq!(window.find("composer-stop").bounds(), stop);

        window.click("composer-stop", cx);
        assert_eq!(
            shell.read(cx).last_intent(),
            Some(&IntentRecord {
                kind: IntentKind::Stop,
                session_id: "fixture-session-7".to_string(),
                content_bytes: 0,
                content_chars: 0,
            })
        );

        // With no draft the send controls do nothing.
        window.click("composer-primary", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), None);

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.input("แก้ทิศทาง", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.click("composer-primary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Steer));

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        window.press(SECONDARY_ENTER, cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Steer));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "แก้ทิศทาง");
    });
}

#[gpui_kit::test]
fn f6_moves_between_regions_and_skips_a_closed_panel(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let focused = |window: &gpui_kit::Window, id: &'static str| window.find(id).focused();

        // Composer -> Context -> Sessions -> Conversation -> Composer.
        window.press("f6", cx);
        assert_eq!(focused(window, "region-inspector"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));

        // Closing the inspector removes it from the cycle.
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn closing_a_focused_panel_returns_focus_to_the_composer(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn arrows_preview_a_session_and_only_enter_opens_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("unsent draft", cx);
        window.press("f6", cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        window.press("down", cx);
        window.press("down", cx);
        assert_eq!(shell.read(cx).sidebar().selected_index(), Some(2));
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        // Previewing did not touch the draft or steal focus.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        window.press("enter", cx);
        let second = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(second.as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        // Going back restores the first Session's draft.
        window.press("up", cx);
        window.press("up", cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
    })
    .unwrap();
}

#[gpui_kit::test]
fn ten_thousand_sessions_and_blocks_materialize_only_what_is_near_the_viewport(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 10_000, 10_000);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let rows = shell.read(cx).sidebar().rows().len();
        assert!(rows > 9_000, "archived Sessions are filtered: {rows}");

        let observed = window.elements();
        let count = |name: &str| {
            observed
                .iter()
                .filter(|element| {
                    element
                        .path()
                        .last()
                        .is_some_and(|id| format!("{id:?}").contains(name))
                })
                .count()
        };
        let session_rows = count("session-row");
        let blocks = count("block");
        // Rows are identified by Session id, not by position.
        let row_id = |index: usize| session_row_id(&shell.read(cx).sidebar().rows()[index].id);
        assert!(window.find(row_id(0)).visible());
        assert!(window.try_find(row_id(5_000)).is_none());
        assert!(
            (1..=60).contains(&session_rows),
            "session rows: {session_rows}"
        );

        // The timeline follows its tail: the newest block is there, the
        // oldest was never built.
        assert!(window.find(("block", 10_000usize)).visible());
        assert!(window.try_find(("block", 1usize)).is_none());
        assert!((1..=60).contains(&blocks), "timeline blocks: {blocks}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn markdown_paints_thai_cjk_and_right_to_left_text(cx: &mut TestAppContext) {
    // Tall enough for all eight fixture bodies at once.
    let (handle, _) = open(cx, size(px(1440.), px(2400.)), 20, 8);
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let painted: String = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "ช่วยสรุปค่าใช้จ่ายของเดือนนี้ให้หน่อย",
            "了解しました。以下に要点をまとめます。",
            "收入",
            "items.iter().sum() // 合計 / ผลรวม",
            "تقرير.pdf",
            "👩🏽‍💻",
        ] {
            assert!(
                painted.contains(expected),
                "{expected:?} was not painted:\n{painted}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_long_unspaced_thai_paragraph_wraps_inside_a_narrow_conversation(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, size(px(520.), px(2400.)), 20, 8);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let conversation = window.find("region-conversation").bounds();
        assert_eq!(conversation.size.width, px(520.));
        let painted = texts(window);
        let paragraph = painted
            .iter()
            .find(|text| text.text().starts_with("ข้อความอ้างอิงภาษาไทย"))
            .expect("the Thai quote was painted");
        let one_line = painted
            .iter()
            .find(|text| text.text() == "block 8")
            .expect("a short single-line Markdown paragraph was painted")
            .bounds()
            .size
            .height;
        // It breaks onto further lines instead of overflowing or clipping.
        // Where it breaks is not asserted: GPUI 0.3.12 has no Thai
        // word-break model, so the break is not at a word boundary.
        assert!(paragraph.bounds().size.height > one_line * 1.5);
        assert!(paragraph.bounds().left() >= conversation.left());
        assert!(paragraph.bounds().right() <= conversation.right());
        assert!(paragraph.visible());
    });
}

#[gpui_kit::test]
fn a_session_row_keeps_its_element_id_at_a_different_position(cx: &mut TestAppContext) {
    // The same Sessions listed twice: with Session 7 running it sorts into
    // the Active group, idle it stays in Recent. The row is at a different
    // position in each list and is found under the same element id.
    let mut position = |with_running: bool| {
        // Tall enough that the row is materialized in either list.
        let (handle, shell) = open_with(cx, size(px(1440.), px(2400.)), 200, 8, with_running);
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let index = shell
                .read(cx)
                .sidebar()
                .rows()
                .iter()
                .position(|row| row.id == "fixture-session-7")
                .expect("fixture session 7");
            let top = window
                .find(session_row_id("fixture-session-7"))
                .bounds()
                .top();
            (index, top)
        })
    };
    let (running_index, running_top) = position(true);
    let (idle_index, idle_top) = position(false);
    assert_ne!(running_index, idle_index);
    assert_ne!(running_top, idle_top);
}

#[gpui_kit::test]
fn the_inspector_overlay_is_modal_traps_focus_and_restores_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft under the overlay", cx);
        // Put focus somewhere other than the default before opening.
        window.press("shift-f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert!(window.find("overlay-backdrop").visible());

        // F6 and Tab cannot leave the overlay.
        for key in ["f6", "shift-f6", "tab", "shift-tab", "tab"] {
            window.press(key, cx);
            assert_eq!(
                window.find("region-inspector").focused(),
                Some(true),
                "{key} left the overlay"
            );
        }

        // Send sits under the overlay panel: a click there reaches the panel,
        // not the button.
        let send = window.find("composer-primary").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert!(send.right() > inspector.left());
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        assert!(window.find("region-inspector").visible());

        // Escape dismisses it and focus goes back where it was.
        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft under the overlay");

        // With the overlay gone the same click works.
        window.click("composer-primary", cx);
        assert!(shell.read(cx).last_intent().is_some());
    });
}

#[gpui_kit::test]
fn a_click_on_the_backdrop_dismisses_the_overlay_and_nothing_else(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("region-inspector").visible());
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        // The backdrop's top-left corner lies over the first Session row.
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn the_sessions_toggle_opens_a_modal_sidebar_when_too_narrow_to_dock(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        window.input("keep me", cx);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.focused(), Some(true));
        assert_eq!(sidebar.bounds().left(), px(0.));
        // It lies over the conversation, which keeps the full width.
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("overlay-backdrop").visible());

        // Arrows preview inside the overlay; Escape closes without opening.
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);
        window.press("down", cx);
        window.press("escape", cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // Enter opens the selected Session, closes the overlay and returns
        // focus to the composer it came from.
        window.press(TOGGLE_SIDEBAR, cx);
        window.press("down", cx);
        window.press("enter", cx);
        let selected = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(selected.as_str())
        );
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        // The header toggle works too, and closes what it opened.
        window.click("toggle-sidebar", cx);
        assert!(window.find("region-sidebar").visible());
        window.click("toggle-sidebar", cx);
        assert!(window.try_find("region-sidebar").is_none());
    });
}

#[gpui_kit::test]
fn widening_docks_an_open_overlay_and_narrowing_again_does_not_reopen_it(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("overlay-backdrop").visible());
    });

    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Docked now: no backdrop, and focus is back in a visible region.
        assert!(window.try_find("overlay-backdrop").is_none());
        assert!(window.find("region-inspector").visible());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
    });
}

#[gpui_kit::test]
fn other_destinations_show_a_scaffold_notice_instead_of_the_conversation(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("still here", cx);

        window.click("destination-Tools", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Tools);
        // No conversation, no composer: nothing that looks like a Tools page.
        assert!(window.try_find("region-composer").is_none());
        assert!(window.try_find("composer-primary").is_none());
        assert!(window.try_find(("block", 40usize)).is_none());
        assert!(window.find("back-to-chat").visible());
        // Focus left the composer for the region that replaced it.
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.draw(cx).clear(cx);
        let painted: Vec<String> = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect();
        assert!(
            painted.iter().any(|text| text == "Tools: not built"),
            "{painted:?}"
        );
        assert!(painted
            .iter()
            .any(|text| text.contains("This Phase 0 scaffold only has a Chat view")));

        // The region cycle skips the composer that is not there.
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.click("back-to-chat", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(window.find("region-composer").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "still here");
    });
}

/// Press Tab until `id` holds keyboard focus.
fn tab_to(window: &mut Window, id: &'static str, cx: &mut App) {
    for _ in 0..60 {
        if window.find(id).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("Tab never reached {id}");
}

#[gpui_kit::test]
fn panels_expose_landmark_roles_docked_and_named_dialogs_as_overlays(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Navigation));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(sidebar.description(), None);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Complementary));
        assert_eq!(inspector.label(), Some("Context"));
    });

    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Dialog));
        assert_eq!(inspector.label(), Some("Context"));
        assert!(inspector
            .description()
            .is_some_and(|text| text.contains("Escape")));
        // Exactly one dialog is exposed while an overlay is up.
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Dialog));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press("escape", cx);
        assert!(window.find_all_by_role(Role::Dialog).is_empty());
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_header_control_that_opened_it(
    cx: &mut TestAppContext,
) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "toggle-inspector", cx);
        // Activate the focused toggle from the keyboard.
        window.press("enter", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-inspector").focused(), Some(false));

        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        // Back on the very button, not on the composer or a neighbour.
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-sidebar").focused(), Some(false));
        assert_eq!(window.find_by_label("Message").focused(), Some(false));

        // The same button closes it again, and focus stays on that button.
        window.press("enter", cx);
        assert!(window.find("region-inspector").visible());
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_control_inside_a_region(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Tools", cx);
    });
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "back-to-chat", cx);

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("back-to-chat").focused(), Some(false));
        window.press("escape", cx);
        // `focused()` on a region means "contains focus", so the button, a
        // leaf, is what shows that focus came back to the control itself.
        assert_eq!(window.find("back-to-chat").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn focus_falls_back_when_the_originating_control_is_gone(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Opened from the composer, then a destination without a composer is
        // chosen inside the overlay: the composer cannot take focus back.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press(TOGGLE_SIDEBAR, cx);
        window.click("destination-Metrics", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Metrics);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-composer").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
    });
}

// ---- Memory policy and stress -------------------------------------------

#[cfg(target_os = "macos")]
const COMMAND: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const COMMAND: &str = "ctrl";

fn chord(key: &str) -> String {
    format!("{COMMAND}-{key}")
}

fn open_blocks(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    blocks: Vec<TimelineBlock>,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);
        let sessions = fixture::sessions(20);
        let running = fixture::running(&sessions);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

/// Lines of text adding up to exactly `bytes` bytes.
fn text_of(bytes: usize) -> String {
    let line = "the quick brown fox jumps over the lazy dog, again and again and again.\n";
    let mut text = line.repeat(bytes / line.len());
    text.push_str(&"x".repeat(bytes - text.len()));
    text
}

fn painted(window: &Window) -> Vec<String> {
    texts(window)
        .iter()
        .map(|text| text.text().to_string())
        .collect()
}

#[gpui_kit::test]
fn a_five_megabyte_block_shows_a_bounded_preview_and_typing_still_works(cx: &mut TestAppContext) {
    const FIVE_MIB: usize = 5 * 1024 * 1024;
    let typed = "ยังพิมพ์ได้ still typing 12345";

    // The same typing with no large block, to compare against.
    let (baseline, _) = open_blocks(cx, size(px(1440.), px(2400.)), fixture::timeline(9));
    cx.run_until_parked();
    let baseline_typing = step(cx, baseline, |window, cx| {
        window.draw(cx).clear(cx);
        let typing = Instant::now();
        window.input(typed, cx);
        typing.elapsed()
    });

    let mut blocks = fixture::timeline(8);
    blocks.push(fixture::huge_block(9, FIVE_MIB));
    assert!(blocks[8].total_bytes() >= FIVE_MIB);

    let started = Instant::now();
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(2400.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let first_draw = started.elapsed();

        assert!(window.find(("block-truncated", 9usize)).visible());
        let painted = painted(window);
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Preview: the first"))
            .unwrap_or_else(|| panic!("no truncation notice painted"));
        assert!(notice.contains("of 5.0 MiB"), "{notice}");
        // The start of the output was laid out; lines past the preview were
        // never given to the text layout.
        assert!(painted
            .iter()
            .any(|text| text.contains("00000001 output line")));
        assert!(painted
            .iter()
            .any(|text| text.contains("00000390 output line")));
        assert!(!painted
            .iter()
            .any(|text| text.contains("00000450 output line")));
        let laid_out: usize = painted.iter().map(String::len).sum();
        assert!(
            laid_out < 4 * limits::BLOCK_PREVIEW_MAX_BYTES,
            "{laid_out} bytes of text laid out for the whole window"
        );

        // The composer is not held up by the block above it.
        let typing = Instant::now();
        window.input(typed, cx);
        let typing = typing.elapsed();
        assert_eq!(shell.read(cx).draft(cx).as_ref(), typed);

        println!(
            "EVIDENCE five_mib_block: open_and_first_draw_ms={} typing_ms={} \
             baseline_typing_ms={} for {} keystrokes (each a full frame), \
             painted_text_bytes={laid_out}",
            first_draw.as_millis(),
            typing.as_millis(),
            baseline_typing.as_millis(),
            typed.chars().count()
        );
        // Generous: this guards against a freeze, it is not a budget.
        assert!(first_draw < Duration::from_secs(20));
        assert!(typing < Duration::from_secs(20));
    });
}

#[gpui_kit::test]
fn a_five_megabyte_paste_is_refused_whole_and_explained(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("keep", cx);
    });
    cx.write_to_clipboard(ClipboardItem::new_string(text_of(5 * 1024 * 1024)));
    let pasting = Instant::now();
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        println!(
            "EVIDENCE five_mib_paste: refused_in_ms={}",
            pasting.elapsed().as_millis()
        );
        // Nothing of the clipboard went in.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep");
        let notice = shell.read(cx).composer_notice().expect("a reason is shown");
        assert!(notice.starts_with("Paste not added"), "{notice}");
        assert!(
            notice.contains("5.0 MiB") && notice.contains("1.0 MiB"),
            "{notice}"
        );
        assert!(window.find("composer-notice").visible());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // A paste that fits goes in as usual and clears the notice.
        cx.write_to_clipboard(ClipboardItem::new_string(" and this".to_string()));
        window.press(&chord("v"), cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep and this");
        assert_eq!(shell.read(cx).composer_notice(), None);
        assert!(window.try_find("composer-notice").is_none());
    });
}

#[gpui_kit::test]
fn the_composer_never_holds_more_than_its_limit(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let full = text_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(&chord("v"), cx);
    });
    step(cx, handle, |window, cx| {
        // Exactly at the limit is allowed.
        assert_eq!(shell.read(cx).draft(cx).len(), limits::COMPOSER_MAX_BYTES);
        assert_eq!(shell.read(cx).composer_notice(), None);
        // One more character, typed rather than pasted, goes over.
        window.input("ก", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), limits::COMPOSER_MAX_BYTES);
        assert_eq!(draft.as_ref(), full);
        let notice = shell.read(cx).composer_notice().expect("a reason is shown");
        assert!(notice.contains("limit"), "{notice}");
        // The composer was replaced and still has focus.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn undo_cannot_bring_back_another_sessions_draft(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let rows = |cx: &App| -> Vec<String> {
        shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .take(2)
            .map(|row| row.id.clone())
            .collect()
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("for the first session only", cx);
        // Undo works within one Session's composer.
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |window, cx| {
        assert_ne!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
        window.press(&chord("shift-z"), cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
        let second = rows(cx)[1].clone();
        window.click(session_row_id(&second), cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        // Clicking a row took focus out of the composer; put it back.
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // Undo in the second Session has nothing of the first to restore.
        window.press(&chord("z"), cx);
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        let first = rows(cx)[0].clone();
        window.click(session_row_id(&first), cx);
    });
    step(cx, handle, |_, cx| {
        // The draft itself was parked and comes back.
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
    });
}

#[gpui_kit::test]
fn large_edits_drop_the_undo_history_once_the_budget_is_spent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let chunk = text_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(chunk.clone()));
    step(cx, handle, |window, cx| window.render_frame(cx));
    // Each round inserts and then removes one composer's worth of text; both
    // count against the budget.
    let mut cleared_after = None;
    for round in 0..6 {
        step(cx, handle, |window, cx| window.press(&chord("v"), cx));
        cx.run_until_parked();
        let cleared = step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).draft(cx).as_ref(), chunk, "round {round}");
            shell
                .read(cx)
                .composer_notice()
                .is_some_and(|notice| notice.starts_with("Undo history was cleared"))
        });
        if cleared {
            cleared_after = Some(round);
            break;
        }
        step(cx, handle, |window, cx| {
            window.press(&chord("a"), cx);
            window.press("backspace", cx);
        });
        cx.run_until_parked();
    }
    // Insert 1 MiB, remove 1 MiB, insert 1 MiB, remove 1 MiB, insert 1 MiB:
    // 5 MiB of edits passes the 4 MiB budget on the third paste.
    assert_eq!(cleared_after, Some(2));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // The history is gone: undo changes nothing, and the text is whole.
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), chunk);
    });
}

// ---- Over-limit edits: every pathway, whole draft kept ---------------------

#[cfg(target_os = "macos")]
const TO_START: &str = "cmd-up";
#[cfg(not(target_os = "macos"))]
const TO_START: &str = "ctrl-home";

/// A draft of exactly `bytes` bytes that ends in a multi-code-point emoji, so
/// a cut tail or a split character would show.
const TAIL: &str = " the tail must survive 👩🏽‍💻";

fn draft_of(bytes: usize) -> String {
    text_of(bytes - TAIL.len()) + TAIL
}

/// Open the shell and paste `draft` into the composer.
fn open_with_draft(cx: &mut TestAppContext, draft: &str) -> (WindowHandle<Root>, Entity<Shell>) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.write_to_clipboard(ClipboardItem::new_string(draft.to_string()));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });
    (handle, shell)
}

/// After a rejected edit: the whole previous draft, a reason on screen, and
/// focus still in the composer.
fn assert_rejected(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    expected: &str,
    pathway: &str,
) {
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), expected.len(), "{pathway}: length");
        assert!(draft.as_ref() == expected, "{pathway}: the draft changed");
        let notice = shell
            .read(cx)
            .composer_notice()
            .unwrap_or_else(|| panic!("{pathway}: no reason shown"))
            .to_string();
        assert!(notice.starts_with("Not added"), "{pathway}: {notice}");
        assert_eq!(
            window.find_by_label("Message").focused(),
            Some(true),
            "{pathway}: focus"
        );
    });
}

#[gpui_kit::test]
fn an_over_limit_insertion_in_the_middle_keeps_the_whole_draft_and_the_caret(
    cx: &mut TestAppContext,
) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);

    // Typed in the middle, one byte over.
    step(cx, handle, |window, cx| {
        window.press(TO_START, cx);
        for _ in 0..5 {
            window.press("right", cx);
        }
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
        window.input("Z", cx);
    });
    assert_rejected(cx, handle, &shell, &full, "typing in the middle");
    step(cx, handle, |_, cx| {
        // The caret is where the insertion was attempted.
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
    });

    // Pasted in the middle.
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "pasted in the middle".to_string(),
        ));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &full, "paste in the middle");
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
    });

    // Typed at the very end, after the emoji.
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        window.press("right", cx);
        assert_eq!(
            shell.read(cx).composer_selection(cx),
            full.len()..full.len()
        );
        window.input("!", cx);
    });
    assert_rejected(cx, handle, &shell, &full, "typing at the end");
}

#[gpui_kit::test]
fn a_paste_is_judged_on_what_it_replaces(cx: &mut TestAppContext) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);
    let select_first_ten = |window: &mut Window, cx: &mut App| {
        window.press(TO_START, cx);
        for _ in 0..10 {
            window.press("shift-right", cx);
        }
    };

    // Ten bytes over ten selected bytes: the draft stays at the limit.
    step(cx, handle, |window, cx| {
        select_first_ten(window, cx);
        assert_eq!(shell.read(cx).composer_selection(cx), 0..10);
        cx.write_to_clipboard(ClipboardItem::new_string("0123456789".to_string()));
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    let replaced = format!("0123456789{}", &full[10..]);
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == replaced);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });

    // Eleven bytes over ten selected bytes: one over, refused whole, and the
    // selection it would have replaced is selected again.
    step(cx, handle, |window, cx| {
        select_first_ten(window, cx);
        cx.write_to_clipboard(ClipboardItem::new_string("0123456789X".to_string()));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &replaced, "paste over a selection");
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer_selection(cx), 0..10);
    });

    // A clipboard the size of the limit replaces a fully selected draft.
    let other = draft_of(limits::COMPOSER_MAX_BYTES).replace("fox", "cat");
    assert_eq!(other.len(), limits::COMPOSER_MAX_BYTES);
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        cx.write_to_clipboard(ClipboardItem::new_string(other.clone()));
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == other);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });
}

#[gpui_kit::test]
fn a_multi_code_point_character_is_never_inserted_in_part(cx: &mut TestAppContext) {
    // Four bytes of room; the emoji is one character of fifteen bytes.
    let emoji = "👩🏽‍💻";
    assert_eq!(emoji.len(), 15);
    let nearly = draft_of(limits::COMPOSER_MAX_BYTES - 4);
    let (handle, shell) = open_with_draft(cx, &nearly);

    // Committed in one edit, as an input method or the emoji picker does.
    step(cx, handle, |window, cx| {
        let bounds = window.find_by_label("Message").bounds();
        let composer = shell.read(cx).composer().clone();
        TestInput::new(ElementInputHandler::new(bounds, composer), window).commit_text(emoji, cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "input method commit");

    // Pasted.
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(emoji.to_string()));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "emoji paste");

    // A composition in progress (marked text) is left alone while the input
    // method owns it: twelve bytes where three fit, and no complaint yet.
    let ime = |cx: &mut TestAppContext, act: &dyn Fn(&mut TestInput, &mut App)| {
        step(cx, handle, |window, cx| {
            let bounds = window.find_by_label("Message").bounds();
            let composer = shell.read(cx).composer().clone();
            act(
                &mut TestInput::new(ElementInputHandler::new(bounds, composer), window),
                cx,
            );
        });
        cx.run_until_parked();
    };
    step(cx, handle, |window, cx| window.input("a", cx));
    cx.run_until_parked();
    let nearly = format!("{nearly}a");
    ime(cx, &|input, cx| {
        input.compose_text(None, "にほんご", None, cx)
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).len(), nearly.len() + 12);
        assert_eq!(shell.read(cx).composer_notice(), None);
        // Over-limit text is not handed on even in this state.
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        assert!(shell
            .read(cx)
            .composer_notice()
            .is_some_and(|notice| notice.starts_with("Not sent")));
    });
    // Ending the composition in place makes its text part of the draft, and
    // the kit gives no signal at all when that happens (no event, no
    // notification). Until the composer next changes, the excess is still
    // there; it can be neither sent nor parked.
    ime(cx, &|input, cx| input.unmark_text(cx));
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).len(), nearly.len() + 12);
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        // The next change of any kind, here a caret move, is when it is
        // judged, and it is taken back whole.
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.press("left", cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "composition ended in place");

    // Composed again and committed with different text: same outcome.
    ime(cx, &|input, cx| input.compose_text(None, "かな", None, cx));
    ime(cx, &|input, cx| input.commit_text("仮名", cx));
    assert_rejected(cx, handle, &shell, &nearly, "composition committed");

    // What fits still goes in: three more ASCII bytes fill the draft exactly.
    step(cx, handle, |window, cx| window.input("bcd", cx));
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), limits::COMPOSER_MAX_BYTES);
        assert!(draft.ends_with(&format!("{emoji}abcd")));
    });
}

#[gpui_kit::test]
fn a_rejected_edit_keeps_the_earlier_undo_history_and_cannot_be_redone(cx: &mut TestAppContext) {
    let base = draft_of(limits::COMPOSER_MAX_BYTES - 20);
    let (handle, shell) = open_with_draft(cx, &base);
    // An accepted edit to undo later.
    step(cx, handle, |window, cx| window.input("abc", cx));
    cx.run_until_parked();
    let accepted = format!("{base}abc");
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == accepted);
        // Thirty bytes where seventeen fit.
        cx.write_to_clipboard(ClipboardItem::new_string("x".repeat(30)));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &accepted, "paste at the end");
    step(cx, handle, |window, cx| {
        // The rejection went through the kit's own undo, so nothing else was
        // lost: the notice does not report a cleared history.
        let notice = shell.read(cx).composer_notice().unwrap_or_default();
        assert!(!notice.contains("history was cleared"), "{notice}");
        // Undo now takes back the accepted typing, not nothing.
        window.press(&chord("z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let draft = shell.read(cx).draft(cx);
        assert!(draft.len() < accepted.len(), "undo had nothing to undo");
        assert!(accepted.starts_with(draft.as_ref()));
        // Redo brings the typing back; a second redo would re-apply the
        // rejected paste, and is rejected again.
        window.press(&chord("shift-z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == accepted);
        window.press(&chord("shift-z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        let draft = shell.read(cx).draft(cx);
        assert!(draft.len() <= limits::COMPOSER_MAX_BYTES);
        assert!(
            draft.as_ref() == accepted,
            "redo of the rejected paste got through"
        );
    });
}

#[gpui_kit::test]
fn discarding_stored_drafts_is_reported_not_silent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let ids: Vec<String> = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .take(6)
            .map(|row| row.id.clone())
            .collect()
    });
    let draft = draft_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(draft.clone()));
    let stored = limits::DRAFT_MAX_BYTES / limits::COMPOSER_MAX_BYTES;
    assert_eq!(stored, 4);

    // Leave a full-size draft in each of the first five Sessions.
    for (index, id) in ids.iter().enumerate().take(stored + 1) {
        step(cx, handle, |window, cx| {
            window.click(session_row_id(id), cx);
            window.click_at("region-composer", point(px(40.), px(20.)), cx);
            window.press(&chord("v"), cx);
        });
        cx.run_until_parked();
        step(cx, handle, |_, cx| {
            assert_eq!(
                shell.read(cx).draft(cx).len(),
                draft.len(),
                "session {index}"
            );
            // Four parked drafts fit the budget exactly: nothing discarded
            // and nothing reported so far.
            assert_eq!(shell.read(cx).composer_notice(), None, "session {index}");
        });
    }

    // Leaving the fifth parks a fifth megabyte: the oldest draft has to go.
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&ids[stored + 1]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let notice = shell
            .read(cx)
            .composer_notice()
            .expect("the discard is reported");
        assert!(
            notice.starts_with("Unsent drafts for 1 older Session(s) were discarded"),
            "{notice}"
        );
        assert!(
            notice.contains("64 Sessions") && notice.contains("4.0 MiB"),
            "{notice}"
        );
        assert!(window.find("composer-notice").visible());
        assert!(!shell.read(cx).has_stored_draft(&ids[0]));
        for id in &ids[1..=stored] {
            assert!(shell.read(cx).has_stored_draft(id), "{id}");
        }
        // The discarded draft really is gone, and a kept one is intact.
        window.click(session_row_id(&ids[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        window.click(session_row_id(&ids[1]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == draft);
    });
}

// ---- Streaming -----------------------------------------------------------

#[gpui_kit::test]
fn a_burst_of_stream_deltas_reaches_the_view_in_one_update(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find(("block", 40usize)).visible());
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..1_000 {
                shell.push_stream_delta(&format!("คำที่ {index} "), cx);
            }
        });
        // The data is already complete; the view has not been told yet.
        assert_eq!(shell.read(cx).block_count(), 41);
        assert_eq!(shell.read(cx).stream_stats(), (1_001, 0));
    });
    cx.executor().advance_clock(Duration::from_millis(
        2 * formless_desktop::timeline::FLUSH_INTERVAL_MS,
    ));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(shell.read(cx).stream_stats(), (1_001, 1));
        // Following the tail: the streamed block is on screen with its text.
        assert!(window.find(("block", 41usize)).visible());
        assert!(painted(window)
            .iter()
            .any(|text| text.contains("คำที่ 0 ") && text.contains("คำที่ 999")));

        // Finishing shows the final state at once, with no timer involved.
        shell.update(cx, |shell, cx| {
            shell.push_stream_delta("จบ", cx);
            shell.finish_stream(cx);
        });
        assert_eq!(shell.read(cx).stream_stats().1, 2);
    });
    // The kit parses Markdown off the main thread, so the new text is painted
    // a frame after the update, not within it. The timer armed by the last
    // delta also fires here and finds nothing left to do.
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert!(painted(window).iter().any(|text| text.ends_with("จบ")));
        assert_eq!(shell.read(cx).stream_stats().1, 2);
    });
}

#[gpui_kit::test]
fn streaming_does_not_move_older_content_that_is_being_read(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 200);
    cx.run_until_parked();
    let anchor = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Scroll well away from the tail.
        window.scroll(
            "region-conversation",
            ScrollDelta::Pixels(point(px(0.), px(4000.))),
            cx,
        );
        window.render_frame(cx);
        assert!(window.try_find(("block", 200usize)).is_none());
        let id = (1..=200usize)
            .rev()
            .find(|id| {
                window
                    .try_find(("block", *id))
                    .is_some_and(|block| block.visible() && block.bounds().top() > px(100.))
            })
            .expect("an older block is on screen");
        (id, window.find(("block", id)).bounds())
    });

    step(cx, handle, |_, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..300 {
                shell.push_stream_delta(&format!("streamed paragraph {index}\n\n"), cx);
            }
        });
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).stream_stats(), (301, 1));
        assert_eq!(shell.read(cx).block_count(), 201);
        // The block being read is exactly where it was.
        assert_eq!(window.find(("block", anchor.0)).bounds(), anchor.1);
        // The streamed tail is off screen and was not built.
        assert!(window.try_find(("block", 201usize)).is_none());
    });
}

// ---- Text size -----------------------------------------------------------
//
// These set the shell's own text-size setting and the display scale factor in
// the test window. They are not observations of a macOS accessibility
// setting, and nobody looked at the result.

fn set_text_scale(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    percent: u16,
) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(percent), window, cx)
        });
        window.render_frame(cx);
    });
}

const SHELL_PARTS: [&str; 5] = [
    "header",
    "region-sidebar",
    "region-conversation",
    "region-inspector",
    "status",
];

#[gpui_kit::test]
fn at_200_percent_a_window_twice_as_wide_has_the_same_layout_at_twice_the_size(
    cx: &mut TestAppContext,
) {
    let (normal, normal_shell) = open(cx, WIDE, 200, 40);
    let first_row = step(cx, normal, |_, cx| {
        session_row_id(&normal_shell.read(cx).sidebar().rows()[0].id)
    });
    let measure = |cx: &mut TestAppContext, handle: WindowHandle<Root>| {
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let mut bounds: Vec<Bounds<Pixels>> = SHELL_PARTS
                .iter()
                .map(|id| window.find(*id).bounds())
                .collect();
            bounds.push(window.find(first_row.clone()).bounds());
            let controls =
                ["composer-primary", "toggle-inspector"].map(|id| window.find(id).bounds());
            (bounds, controls)
        })
    };
    let (at_100, controls_100) = measure(cx, normal);

    let (large, large_shell) = open(cx, size(px(2880.), px(1800.)), 200, 40);
    set_text_scale(cx, large, &large_shell, 200);
    let (at_200, controls_200) = measure(cx, large);

    // Twice the size to within one pixel: rem-based sizes round per element.
    let doubled = |big: Pixels, small: Pixels| (big - small * 2.).abs() <= px(1.);
    for (index, (small, big)) in at_100.iter().zip(&at_200).enumerate() {
        let name = SHELL_PARTS
            .get(index)
            .copied()
            .unwrap_or("first session row");
        assert!(
            doubled(big.origin.x, small.origin.x)
                && doubled(big.origin.y, small.origin.y)
                && doubled(big.size.width, small.size.width)
                && doubled(big.size.height, small.size.height),
            "{name}: {small:?} -> {big:?}"
        );
    }
    // Kit controls scale with the text too. How exactly is the kit's
    // business; they must at least have grown clearly in both directions.
    for (small, big) in controls_100.iter().zip(&controls_200) {
        println!(
            "EVIDENCE text_scale_200 control: {:?} -> {:?}",
            small.size, big.size
        );
        assert!(big.size.height >= small.size.height * 1.5);
        assert!(big.size.width >= small.size.width * 1.5);
    }
}

#[gpui_kit::test]
fn at_200_percent_in_the_same_window_the_panels_give_way_and_controls_stay_reachable(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("ข้อความเดิม", cx);
    });
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        // 1440 px at 200% lays out like 720 px: neither panel is docked, and
        // nothing was dropped over the conversation.
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
        assert_eq!(
            window.find("region-conversation").bounds().size.width,
            px(1440.)
        );
        assert_eq!(window.find("header").bounds().size.height, px(88.));

        // Every control is whole and inside the window, in reading order.
        let viewport = Bounds {
            origin: Point::default(),
            size: WIDE,
        };
        let controls = [
            "toggle-sidebar",
            "toggle-inspector",
            "toggle-theme",
            "composer-stop",
            "composer-primary",
        ];
        for id in controls {
            let control = window.find(id);
            assert!(control.visible(), "{id}");
            let bounds = control.bounds();
            assert!(
                bounds.left() >= viewport.left()
                    && bounds.right() <= viewport.right()
                    && bounds.top() >= viewport.top()
                    && bounds.bottom() <= viewport.bottom(),
                "{id} is not inside the window: {bounds:?}"
            );
        }
        let header: Vec<_> = controls[..3]
            .iter()
            .map(|id| window.find(*id).bounds())
            .collect();
        assert!(header[0].right() <= header[1].left());
        assert!(header[1].right() <= header[2].left());

        // The draft and focus survived the change, and typing still works.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.input(" ที่พิมพ์ต่อ", cx);
        assert_eq!(
            window.find_by_label("Message").value(),
            Some("ข้อความเดิม ที่พิมพ์ต่อ")
        );

        // The sidebar is still one keystroke away, at twice the size.
        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar").bounds();
        assert_eq!(sidebar.size.width, px(tokens::SIDEBAR_DEFAULT * 2.));
        let row = session_row_id(&shell.read(cx).sidebar().rows()[0].id);
        assert_eq!(window.find(row).bounds().size.height, px(88.));
        window.press("escape", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

const REACHABLE_AT_ANY_SIZE: [&str; 7] = [
    "toggle-sidebar",
    "toggle-inspector",
    "toggle-theme",
    "composer-stop",
    "composer-primary",
    "region-composer",
    "status",
];

fn inside_window(window: &Window, id: &'static str, window_size: Size<Pixels>) -> bool {
    let bounds = window.find(id).bounds();
    bounds.left() >= px(0.)
        && bounds.right() <= window_size.width
        && bounds.top() >= px(0.)
        && bounds.bottom() <= window_size.height
}

#[gpui_kit::test]
fn the_minimum_window_must_grow_with_the_text_size(cx: &mut TestAppContext) {
    // 480 x 360 is the binary's minimum window size. At 100% it holds every
    // control.
    let minimum = size(px(480.), px(360.));
    let (handle, shell) = open(cx, minimum, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        for id in REACHABLE_AT_ANY_SIZE {
            assert!(inside_window(window, id, minimum), "{id} at 100%");
        }
    });

    // At 200% the same window does not: this is a measured limitation, kept
    // here as a record rather than asserted either way.
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let report: Vec<String> = REACHABLE_AT_ANY_SIZE
            .into_iter()
            .map(|id| {
                let place = if inside_window(window, id, minimum) {
                    "inside"
                } else {
                    "OUTSIDE"
                };
                format!("{id}={place}")
            })
            .collect();
        println!("EVIDENCE min_window_200pct 480x360: {}", report.join(" "));
    });

    // A minimum that scales with the text (960 x 720 at 200%) holds them all.
    let scaled_minimum = size(px(960.), px(720.));
    cx.simulate_window_resize(handle.into(), scaled_minimum);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        for id in REACHABLE_AT_ANY_SIZE {
            assert!(
                inside_window(window, id, scaled_minimum),
                "{id} at 200% in 960x720"
            );
        }
    });
}

#[gpui_kit::test]
fn text_size_shortcuts_step_up_to_200_percent_and_reset(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft", cx);
        let mut seen = vec![shell.read(cx).text_scale().percent()];
        for _ in 0..6 {
            window.press(&chord("="), cx);
            seen.push(shell.read(cx).text_scale().percent());
        }
        // It stops at 200% however often the shortcut is pressed.
        assert_eq!(seen, [100, 125, 150, 175, 200, 200, 200]);
        assert_eq!(window.find("header").bounds().size.height, px(88.));

        window.press(&chord("-"), cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 175);
        window.press(&chord("0"), cx);
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT);
        assert_eq!(window.find("header").bounds().size.height, px(44.));
        // Back to three panes, with the draft and focus untouched.
        assert!(window.find("region-inspector").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn the_display_scale_factor_does_not_change_the_logical_layout(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    let measure = |cx: &mut TestAppContext| {
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            SHELL_PARTS.map(|id| window.find(id).bounds())
        })
    };
    let before = measure(cx);
    cx.simulate_window_scale_factor_change(handle.into(), 2.0);
    assert_eq!(measure(cx), before);
    cx.simulate_window_scale_factor_change(handle.into(), 1.0);
    assert_eq!(measure(cx), before);
}
