use std::sync::mpsc::{self, Receiver, TryRecvError};

type WriteResult = Result<(), String>;

#[derive(Default)]
pub(super) struct ClipboardWrite {
    running: Option<Receiver<WriteResult>>,
    pending: Option<String>,
}

impl ClipboardWrite {
    pub(super) fn submit(&mut self, text: String) -> WriteResult {
        if self.running.is_some() {
            // Keep the latest copy behind the active one so an older helper
            // cannot take ownership of the system clipboard afterwards.
            self.pending = Some(text);
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("formless-clipboard-write".into())
            .spawn(move || {
                let _ = tx.send(formless_runtime::clipboard_write(&text));
            })
            .map_err(|error| format!("could not start clipboard worker: {error}"))?;
        self.running = Some(rx);
        Ok(())
    }

    pub(super) fn poll(&mut self) -> WriteResult {
        let Some(running) = &self.running else {
            return Ok(());
        };
        let result = match running.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => Err("clipboard worker stopped".into()),
        };
        self.running = None;
        if let Some(text) = self.pending.take() {
            return self.submit(text);
        }
        result
    }
}
