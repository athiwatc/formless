use std::ffi::c_void;
use std::ptr::{self, NonNull};

use block2::RcBlock;
use core_foundation::base::{CFEqual, CFType, CFTypeRef, TCFType as _};
use core_foundation::boolean::kCFBooleanTrue;
use core_foundation::string::CFStringRef;
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, Context, Entity, EntityInputHandler as _, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSEvent, NSEventMask, NSEventModifierFlags, NSTextInputContext, NSView, NSWindow,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::Shell;

const ESCAPE_KEY_CODE: u16 = 53;
const SPACING_ACUTE: &str = "´";

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> CFTypeRef;
    fn TISGetInputSourceProperty(source: CFTypeRef, property: CFStringRef) -> *const c_void;
    static kTISPropertyInputSourceType: CFStringRef;
    static kTISTypeKeyboardLayout: CFStringRef;
    static kTISPropertyInputSourceIsASCIICapable: CFStringRef;
}

fn ascii_keyboard_layout() -> bool {
    // The Copy function returns an owned source. Its properties are borrowed
    // for the lifetime of that source and have documented CF types.
    unsafe {
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return false;
        }
        let source = CFType::wrap_under_create_rule(source);
        let layout = TISGetInputSourceProperty(source.as_CFTypeRef(), kTISPropertyInputSourceType);
        let ascii =
            TISGetInputSourceProperty(source.as_CFTypeRef(), kTISPropertyInputSourceIsASCIICapable);
        !layout.is_null()
            && CFEqual(layout, kTISTypeKeyboardLayout.cast()) != 0
            && !ascii.is_null()
            && CFEqual(ascii, kCFBooleanTrue.cast()) != 0
    }
}

fn same_object<T, U>(left: &T, right: &U) -> bool {
    ptr::eq(
        ptr::from_ref(left).cast::<()>(),
        ptr::from_ref(right).cast::<()>(),
    )
}

struct NativeInputOwner {
    view: Retained<NSView>,
    window: Retained<NSWindow>,
    context: Retained<NSTextInputContext>,
}

impl NativeInputOwner {
    fn capture(window: &Window, event_window: &NSWindow, main: MainThreadMarker) -> Option<Self> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // The borrowed AppKit handle points to GPUI's live NSView. Retain it
        // only for this event so post-cancellation checks can compare identities.
        let view = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }?;
        let native_window = view.window()?;
        if !same_object(event_window, &*native_window) || !native_window.isKeyWindow() {
            return None;
        }
        let context = view.inputContext()?;
        let owner = Self {
            view,
            window: native_window,
            context,
        };
        owner.is_current(main).then_some(owner)
    }

    fn is_current(&self, main: MainThreadMarker) -> bool {
        self.window.isKeyWindow()
            && self
                .view
                .window()
                .is_some_and(|window| same_object(&*window, &*self.window))
            && self
                .window
                .firstResponder()
                .is_some_and(|view| same_object(&*view, &*self.view))
            && self
                .view
                .inputContext()
                .is_some_and(|context| same_object(&*context, &*self.context))
            && NSTextInputContext::currentInputContext(main)
                .is_some_and(|context| same_object(&*context, &*self.context))
            && same_object(&*self.context.client(), &*self.view)
    }
}

fn composer_is_active(composer: &Entity<TextareaState>, window: &mut Window, cx: &mut App) -> bool {
    window
        .focused_input(cx)
        .is_some_and(|input| input.as_textarea() == Some(composer))
}

fn canceled_composer_is_current(
    shell: &Shell,
    composer: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    shell.composer == *composer
        && composer_is_active(composer, window, cx)
        && composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_none()
        })
}

fn composer_has_marked_acute(
    composer: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    composer.update(cx, |input, cx| {
        let Some(marked) = input.marked_text_range(window, cx) else {
            return false;
        };
        let mut adjusted = None;
        input
            .text_for_range(marked.clone(), &mut adjusted, window, cx)
            .as_deref()
            == Some(SPACING_ACUTE)
            && adjusted.is_none_or(|adjusted| adjusted == marked)
    })
}

/// The monitor observes only key events dispatched within Formless.
pub(super) struct EscapeMonitor(Retained<AnyObject>);

impl Drop for EscapeMonitor {
    fn drop(&mut self) {
        // This token came from AppKit and has one owner on the GUI thread.
        unsafe { NSEvent::removeMonitor(&self.0) }
    }
}

impl Shell {
    /// Register native cancellation once for the lifetime of this shell.
    pub fn install_native_composer_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_composer_escape.is_some() {
            return;
        }
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let shell = cx.weak_entity();
        let window = window.to_async(cx);
        let monitor = RcBlock::new(move |event: NonNull<NSEvent>| {
            // AppKit owns the incoming event for the duration of this callback.
            let key = unsafe { event.as_ref() };
            let modifiers = NSEventModifierFlags::Shift
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Command
                | NSEventModifierFlags::Function;
            if key.keyCode() != ESCAPE_KEY_CODE
                || key.modifierFlags().intersects(modifiers)
                || !ascii_keyboard_layout()
            {
                return event.as_ptr();
            }
            let Some(event_window) = key.window(main) else {
                return event.as_ptr();
            };
            let mut window = window.clone();
            let canceled = window.update(|window, cx| {
                let owner = NativeInputOwner::capture(window, &event_window, main)?;
                let shell = shell.upgrade()?;
                shell.update(cx, |shell, cx| {
                    let composer = shell.composer.clone();
                    if composer_is_active(&composer, window, cx)
                        && composer_has_marked_acute(&composer, window, cx)
                        && shell.cancel_composer_composition(window, cx)
                    {
                        Some((owner, composer))
                    } else {
                        None
                    }
                })
            });
            let Ok(Some((owner, composer))) = canceled else {
                return event.as_ptr();
            };

            // App updates can deliver focus, close or Session-change effects.
            // Discard only the conversion still belonging to the same composer.
            let still_owns_conversion = window
                .update(|window, cx| {
                    let Some(shell) = shell.upgrade() else {
                        return false;
                    };
                    shell.update(cx, |shell, cx| {
                        canceled_composer_is_current(shell, &composer, window, cx)
                    })
                })
                .unwrap_or(false);
            if still_owns_conversion && owner.is_current(main) {
                // AppKit may call the text client here. Do this after releasing
                // the GPUI app borrow, before consuming the original Escape.
                owner.context.discardMarkedText();
            }
            ptr::null_mut()
        });
        // This callback returns the valid borrowed event pointer or nil.
        self.native_composer_escape = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &monitor)
        }
        .map(EscapeMonitor);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use gpui_kit::test::{TestInput, TestQueryExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, ElementInputHandler, TestAppContext, WindowOptions};

    use super::*;
    use crate::fixture;
    use crate::shell::{bind_keys, PageSection};

    #[gpui_kit::test]
    fn discard_requires_the_original_cleared_composer_and_an_open_window(cx: &mut TestAppContext) {
        let (handle, shell) = cx.update(|cx| {
            gpui_kit::init(cx);
            bind_keys(cx);
            let sessions = fixture::sessions(4);
            let running = fixture::running(&sessions);
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Shell::new(&sessions, &running, Vec::new(), window, cx))
            })
            .unwrap()
        });
        cx.update(|cx| {
            handle
                .update(cx, |_, window, cx| {
                    window.render_frame(cx);
                    window.input("unsent", cx);
                })
                .unwrap();
        });
        cx.run_until_parked();
        let mut closed = cx.update(|cx| {
            handle
                .update(cx, |_, window, cx| {
                    window.render_frame(cx);
                    let composer = shell.read(cx).composer.clone();
                    assert!(composer_is_active(&composer, window, cx));
                    let bounds = window.find_by_label("Message").bounds();
                    TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window)
                        .compose_text(None, SPACING_ACUTE, None, cx);
                    assert!(composer_has_marked_acute(&composer, window, cx));
                    assert!(!shell.update(cx, |shell, cx| {
                        canceled_composer_is_current(shell, &composer, window, cx)
                    }));
                    window.press("escape", cx);
                    assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent");
                    assert!(shell.update(cx, |shell, cx| {
                        canceled_composer_is_current(shell, &composer, window, cx)
                    }));

                    shell.update(cx, |shell, cx| {
                        shell.focus_section(PageSection::Conversation, window, cx)
                    });
                    window.render_frame(cx);
                    assert!(!shell.update(cx, |shell, cx| {
                        canceled_composer_is_current(shell, &composer, window, cx)
                    }));
                    shell.update(cx, |shell, cx| {
                        shell.move_selection(1, cx);
                        shell.activate_selected(window, cx);
                    });
                    window.render_frame(cx);
                    let next = shell.read(cx).composer.clone();
                    assert_ne!(next, composer);
                    assert!(!shell.update(cx, |shell, cx| {
                        canceled_composer_is_current(shell, &composer, window, cx)
                    }));

                    let closed = window.to_async(cx);
                    window.remove_window();
                    closed
                })
                .unwrap()
        });
        assert!(closed.update(|_, _| ()).is_err());
    }
}
