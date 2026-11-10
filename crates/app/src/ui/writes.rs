//! What the window asks to be written to the store, done off its thread.
//!
//! The store has one writer. A write asked for on the window's thread
//! waits for whatever holds it (a page of history, a downloaded file), and
//! the window stops with it: a click on a star, an edit, a reaction froze
//! the application for as long as the engine was busy. The window queues
//! those writes here instead and goes on; the store's change notification
//! is what brings their result back to the screen.
//!
//! One queue, one write at a time, in the order they were asked for: a
//! star and the unstar right after it, or two edits of one message, land
//! in that order. It runs on GPUI's background executor, so in the tests
//! it runs while the test lets the window settle.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt as _;
use gpui_kit::Task;

type Job = Box<dyn FnOnce() + Send>;

/// The window's queue of writes. Cheap to clone.
#[derive(Clone)]
pub(super) struct Writes {
    queue: mpsc::UnboundedSender<Job>,
}

impl Writes {
    /// Starts the queue. It works as long as the task lives.
    pub(super) fn start(cx: &gpui_kit::App) -> (Self, Task<()>) {
        let (queue, mut jobs) = mpsc::unbounded::<Job>();
        let task = cx.background_executor().spawn(async move {
            while let Some(job) = jobs.next().await {
                // One write that panics is that write lost, not the queue.
                if std::panic::catch_unwind(AssertUnwindSafe(job)).is_err() {
                    tracing::error!("a write asked for by the window panicked");
                }
            }
        });
        (Self { queue }, task)
    }

    /// Queues `job` and returns at once.
    pub(super) fn run(&self, job: impl FnOnce() + Send + 'static) {
        if self.queue.unbounded_send(Box::new(job)).is_err() {
            tracing::warn!("a write was asked for after the window closed");
        }
    }

    /// Queues `job`; the answer is its result, once it has run (`None`
    /// when the queue is gone).
    pub(super) fn ask<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        job: F,
    ) -> impl Future<Output = Option<T>> + use<T, F> {
        let (answer, answered) = oneshot::channel();
        self.run(move || {
            let _ = answer.send(job());
        });
        async move { answered.await.ok() }
    }
}
