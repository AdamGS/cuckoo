//! A work-stealing runtime to run async CPU-heavy rust workloads inside multi-threaded applications.
//!
//! The main idea behind Cuckoo is that some systems already have opinionated threading models, and spinning up additional threads
//! just to run some async code might cause increased core contention and hurt overall performance.
//!
//! Cuckoo is mostly intended to run async code in a block fashion (within `block_on` blocks), and while a thread is waiting on
//! some future, it can take work from other threads and make some progress on them.
//!
//! ## Current Status
//!
//! Cuckoo is completely experimental, it might deadlock, cause unexpected behavior or is probably not very fast.

use std::{
    sync::Arc,
    thread::{self, ThreadId},
};

use async_task::{Runnable, Task};
use crossbeam::deque::{Injector, Stealer, Worker};
use dashmap::DashMap;

use pin_project_lite::pin_project;

thread_local! {
    static WORK_QUEUE: Worker<Runnable> = Worker::new_fifo();
}

/// The main runtime instance, holds all global state and thread-specific handles are created by it.
pub struct Runtime {
    injector: Arc<Injector<Runnable>>,
    stealers: Arc<DashMap<ThreadId, Stealer<Runnable>>>,
}

/// A handle to the runtime that can be cloned and sent across threads.
///
/// Right now the underlying work queue is thread-local, overtime there might also be a thread-local variant of the handel that can't be sent to other threads.
#[derive(Clone)]
pub struct Handle {
    injector: Arc<Injector<Runnable>>,
    stealers: Arc<DashMap<ThreadId, Stealer<Runnable>>>,
}

pin_project! {
    pub struct JoinHandle<T> {
        #[pin]
        task: Task<T>,
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.project().task.poll(cx)
    }
}

impl Runtime {
    /// Creates a new runtime with a fixed number of background threads.
    ///
    /// Background threads execute tasks that are spawned in the background, allowing foreground threads to keep doing other work.
    pub fn new(background_threads: usize) -> Self {
        let injector: Arc<Injector<Runnable>> = Arc::new(Injector::new());
        for _ in 0..background_threads {
            std::thread::spawn({
                let injector = Arc::clone(&injector);
                move || {
                    loop {
                        if let Some(task) = injector.steal().success() {
                            task.run();
                        }
                    }
                }
            });
        }
        Self {
            injector,
            stealers: Arc::new(DashMap::new()),
        }
    }

    pub fn handle(&self) -> Handle {
        Handle {
            injector: Arc::clone(&self.injector),
            stealers: Arc::clone(&self.stealers),
        }
    }

    /// Spawn a future to run in the background. The future can make progress by any participating thread.
    ///
    /// If the runtime doesn't have any background threads, the [`JoinHandle`] should be polled/awaited explicitly.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let injector = Arc::clone(&self.injector);
        let schedule = move |runnable| injector.push(runnable);
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();
        JoinHandle { task }
    }

    /// Run a future, blocking the current thread until its done.
    ///
    /// The future might end up running on a different thread, and this thread might make progress on other futures before returning.
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        self.handle().block_on(future)
    }
}

impl Handle {
    /// Run a future, blocking the current thread until its done.
    ///
    /// The future might end up running on a different thread, and this thread might make progress on other futures before returning.
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let schedule = {
            let stealers = Arc::clone(&self.stealers);
            move |runnable| {
                WORK_QUEUE.with(|q| {
                    // Make sure this thread's stealer is populated
                    stealers
                        .entry(thread::current().id())
                        .or_insert(q.stealer());
                    q.push(runnable);
                })
            }
        };
        let (runnable, task) = async_task::spawn(future, schedule);
        // I think we want to call `run` here to prioritize the existing task before we start stealing work from other threads.
        runnable.run();

        while !task.is_finished() {
            if let Some(stolen_task) = self.find_task() {
                stolen_task.run();
            }
        }

        futures::executor::block_on(task)
    }

    /// Spawn a future to run in the background. The future can make progress by any participating thread.
    ///
    /// If the runtime doesn't have any background threads, the [`JoinHandle`] should be polled/awaited explicitly.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let injector = Arc::clone(&self.injector);
        let schedule = move |runnable| injector.push(runnable);
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();
        JoinHandle { task }
    }

    fn find_task(&self) -> Option<Runnable> {
        WORK_QUEUE.with(|local| {
            // Pop a task from the local queue, if not empty.
            local.pop().or_else(|| {
                std::iter::repeat_with(|| {
                    self.injector
                        .steal_batch_with_limit_and_pop(local, 1)
                        .or_else(|| self.stealers.iter().map(|s| s.steal()).collect())
                })
                .find(|s| !s.is_retry())
                .and_then(|s| s.success())
            })
        })
    }
}

#[cfg(test)]
mod tests {

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(0)]
    #[case(1)]
    fn test_basic(#[case] thread_count: usize) {
        let rt = Runtime::new(thread_count);
        let jh: JoinHandle<()> = rt.spawn(async move { println!("spawned task!") });
        let handle = rt.handle();
        let h2 = handle.clone();

        handle.block_on(async move {
            _ = h2.spawn(async move {
                if thread_count == 0 {
                    panic!("This should never run");
                }
            });
            println!("blocking here!");
            println!("Awaited the second future");
        });
        println!("Finished block_on block");
        rt.block_on(jh);
    }
}
