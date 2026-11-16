use super::*;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};

type Job = Box<dyn FnOnce(&TokenUsage) + Send>;

pub(super) struct Writer {
    sender: Option<mpsc::SyncSender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    dropped: AtomicU64,
}

impl Writer {
    pub(super) fn start(usage: TokenUsage) -> Result<Self, EngineError> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(256);
        let thread = std::thread::Builder::new()
            .name("token-usage".into())
            .spawn(move || {
                for job in receiver {
                    job(&usage);
                }
            })
            .map_err(fail)?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            dropped: AtomicU64::new(0),
        })
    }

    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl TokenUsage {
    pub(super) fn dispatch(&self, job: impl FnOnce(&TokenUsage) + Send + 'static) {
        if let Some(writer) = &self.2 {
            if writer
                .sender
                .as_ref()
                .unwrap()
                .try_send(Box::new(job))
                .is_err()
            {
                writer.dropped.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            job(self);
        }
    }

    // Queries and deletion reconciliation already run on blocking workers.
    // A barrier makes their view include all preceding accepted observations.
    pub(super) fn flush(&self) -> Result<(), EngineError> {
        if let Some(writer) = &self.2 {
            let (sender, receiver) = mpsc::sync_channel(0);
            writer
                .sender
                .as_ref()
                .unwrap()
                .send(Box::new(move |_| {
                    let _ = sender.send(());
                }))
                .map_err(fail)?;
            receiver.recv().map_err(fail)?;
        }
        Ok(())
    }
}
