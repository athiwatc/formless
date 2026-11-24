//! UTF-8 and VT output for an attached Windows console. File and pipe
//! output uses Rust's normal byte streams.
#[cfg(windows)]
mod native {
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::rc::Rc;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetStdHandle, SetConsoleCP,
        SetConsoleMode, SetConsoleOutputCP, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    const UTF8: u32 = 65001;

    struct Saved {
        depth: usize,
        input: Option<u32>,
        output: Option<u32>,
        modes: Vec<(HANDLE, u32, u32)>,
    }

    thread_local! {
        static SAVED: RefCell<Option<Saved>> = const { RefCell::new(None) };
    }

    impl Saved {
        fn enable() -> Self {
            let mut saved = Self {
                depth: 1,
                input: None,
                output: None,
                modes: Vec::new(),
            };
            // SAFETY: code pages belong to this process's attached console,
            // and GetConsoleMode validates the borrowed standard handles.
            unsafe {
                let input = GetConsoleCP();
                if input != 0 && input != UTF8 && SetConsoleCP(UTF8) != 0 {
                    saved.input = Some(input);
                }
                let output = GetConsoleOutputCP();
                if output != 0 && output != UTF8 && SetConsoleOutputCP(UTF8) != 0 {
                    saved.output = Some(output);
                }
                for stream in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                    let handle = GetStdHandle(stream);
                    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                        continue;
                    }
                    let mut mode = 0;
                    if GetConsoleMode(handle, &mut mode) == 0 {
                        continue;
                    }
                    let enabled =
                        mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                    if mode != enabled && SetConsoleMode(handle, enabled) != 0 {
                        saved.modes.push((handle, mode, enabled));
                    }
                }
            }
            saved
        }
    }

    impl Drop for Saved {
        fn drop(&mut self) {
            // SAFETY: query each borrowed console handle before restoring
            // it. Preserve any intervening changes by the host terminal.
            unsafe {
                for &(handle, original, enabled) in self.modes.iter().rev() {
                    let mut current = 0;
                    if GetConsoleMode(handle, &mut current) != 0 && current == enabled {
                        SetConsoleMode(handle, original);
                    }
                }
                if let Some(page) = self.output.filter(|_| GetConsoleOutputCP() == UTF8) {
                    SetConsoleOutputCP(page);
                }
                if let Some(page) = self.input.filter(|_| GetConsoleCP() == UTF8) {
                    SetConsoleCP(page);
                }
            }
        }
    }

    #[must_use]
    pub struct ConsoleEncoding(PhantomData<Rc<()>>);

    impl ConsoleEncoding {
        pub fn enable() -> Self {
            SAVED.with(|slot| {
                let mut slot = slot.borrow_mut();
                match slot.as_mut() {
                    Some(saved) => saved.depth += 1,
                    None => *slot = Some(Saved::enable()),
                }
            });
            Self(PhantomData)
        }
    }

    impl Drop for ConsoleEncoding {
        fn drop(&mut self) {
            SAVED.with(|slot| {
                let mut slot = slot.borrow_mut();
                if let Some(saved) = slot.as_mut() {
                    saved.depth -= 1;
                    if saved.depth == 0 {
                        slot.take();
                    }
                }
            });
        }
    }

    pub fn restore_before_exit() {
        SAVED.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(windows)]
pub use native::{restore_before_exit, ConsoleEncoding};

#[cfg(not(windows))]
#[must_use]
pub struct ConsoleEncoding;

#[cfg(not(windows))]
impl ConsoleEncoding {
    pub fn enable() -> Self {
        Self
    }
}

#[cfg(not(windows))]
pub fn restore_before_exit() {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use ratatui::{backend::CrosstermBackend, Terminal};
    use windows_sys::Win32::System::Console::{
        AllocConsole, FreeConsole, GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetStdHandle,
        SetConsoleCP, SetConsoleOutputCP, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };

    struct ConsoleFixture {
        owned: bool,
        input: u32,
        output: u32,
    }

    impl Drop for ConsoleFixture {
        fn drop(&mut self) {
            // SAFETY: restore the test console and release only a console
            // allocated by this process for this fixture.
            unsafe {
                SetConsoleCP(self.input);
                SetConsoleOutputCP(self.output);
                if self.owned {
                    FreeConsole();
                }
            }
        }
    }

    #[test]
    fn windows_console_smoke() {
        // SAFETY: this native test owns its process and saves any inherited
        // console settings before exercising both legacy code pages.
        let fixture = unsafe {
            let owned = GetConsoleOutputCP() == 0;
            if owned {
                assert_ne!(AllocConsole(), 0, "allocate native test console");
            }
            ConsoleFixture {
                owned,
                input: GetConsoleCP(),
                output: GetConsoleOutputCP(),
            }
        };
        assert_ne!(fixture.input, 0);
        assert_ne!(fixture.output, 0);
        for page in [437, 1252] {
            // SAFETY: the fixture holds a valid attached console.
            unsafe {
                assert_ne!(SetConsoleCP(page), 0);
                assert_ne!(SetConsoleOutputCP(page), 0);
            }
            let output_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            let mut original_mode = 0;
            assert_ne!(
                unsafe { GetConsoleMode(output_handle, &mut original_mode) },
                0,
                "stdout must be a native console handle"
            );
            let guard = ConsoleEncoding::enable();
            let nested = ConsoleEncoding::enable();
            // SAFETY: query the attached console without changing it.
            assert_eq!(unsafe { GetConsoleCP() }, 65001);
            assert_eq!(unsafe { GetConsoleOutputCP() }, 65001);
            let mut enabled_mode = 0;
            assert_ne!(
                unsafe { GetConsoleMode(output_handle, &mut enabled_mode) },
                0
            );
            assert_ne!(enabled_mode & ENABLE_PROCESSED_OUTPUT, 0);
            assert_ne!(enabled_mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);

            let mut app = crate::tests::test_app();
            app.messages = vec![serde_json::from_value(serde_json::json!({
                "id": "unicode",
                "session_id": "fixture",
                "seq": 1,
                "role": "assistant",
                "generated_by": null,
                "content": "Windows frame ── 🔍",
                "created_at": "2026-01-01T00:00:00Z",
                "meta": null
            }))
            .unwrap()];
            let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout())).unwrap();
            terminal.draw(|frame| crate::draw(frame, &mut app)).unwrap();
            drop(nested);
            assert_eq!(unsafe { GetConsoleOutputCP() }, 65001);
            if page == 437 {
                drop(guard);
            } else {
                restore_before_exit();
                drop(guard);
            }
            assert_eq!(unsafe { GetConsoleCP() }, page);
            assert_eq!(unsafe { GetConsoleOutputCP() }, page);
            let mut restored_mode = 0;
            assert_ne!(
                unsafe { GetConsoleMode(output_handle, &mut restored_mode) },
                0
            );
            assert_eq!(restored_mode, original_mode);
        }
    }
}
