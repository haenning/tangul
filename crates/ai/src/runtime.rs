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

#[cfg(test)]
mod tests {
    #[test]
    fn runs_work_awaited_elsewhere() {
        let handle = super::spawn(async { 1 + 1 });
        assert_eq!(await_elsewhere(handle), 2);
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
