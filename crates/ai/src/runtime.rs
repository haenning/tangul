//! A tokio runtime for the assistant's work outside the engine (model calls over
//! HTTP, the tools they call): the app's own executor isn't tokio. Awaiting the
//! handle works from any executor; aborting it cancels the work.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tangul-ai")
            .enable_all()
            .build()
            .expect("the assistant's runtime")
    })
}

/// Runs `work` on the assistant's runtime.
pub fn spawn<F>(work: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(work)
}

/// Work on the assistant's runtime that stops when this is dropped.
pub struct Work(JoinHandle<()>);

impl Drop for Work {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `work` until it ends or the returned [`Work`] is dropped.
pub fn start<F>(work: F) -> Work
where
    F: Future<Output = ()> + Send + 'static,
{
    Work(spawn(work))
}

#[cfg(test)]
mod tests {
    #[test]
    fn runs_work_awaited_elsewhere() {
        let handle = super::spawn(async { 1 + 1 });
        assert_eq!(await_elsewhere(handle), 2);
    }

    #[test]
    fn dropped_work_stops() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let work = super::start(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let _ = tx.send(());
        });
        drop(work);
        // The sender went with the aborted task, unsent.
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        );
    }

    /// Awaits on another thread and executor, as the app does.
    fn await_elsewhere<T: Send + 'static>(handle: tokio::task::JoinHandle<T>) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            tx.send(rt.block_on(handle).unwrap()).unwrap();
        });
        rx.recv().unwrap()
    }
}
