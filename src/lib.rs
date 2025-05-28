use std::{
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    thread::{self, ThreadId},
    time::Duration,
};

use async_task::{Runnable, Task};
use crossbeam::deque::{Injector, Stealer, Worker};
use parking_lot::RwLock;
use pin_project_lite::pin_project;

thread_local! {
    static WORK_QUEUE: Rc<Worker<Runnable>> = Rc::new(Worker::new_fifo());
}

pub struct Runtime {
    injector: Arc<Injector<Runnable>>,
    stealers: Arc<RwLock<HashMap<ThreadId, Stealer<Runnable>>>>,
}

#[derive(Clone)]
pub struct Handle {
    injector: Arc<Injector<Runnable>>,
    stealers: Arc<RwLock<HashMap<ThreadId, Stealer<Runnable>>>>,
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
                let injector = injector.clone();
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
            stealers: Arc::new(RwLock::new(HashMap::default())),
        }
    }

    pub fn handle(&self) -> Handle {
        Handle {
            injector: Arc::clone(&self.injector),
            stealers: Arc::clone(&self.stealers),
        }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let injector = self.injector.clone();
        let schedule = move |runnable| injector.push(runnable);
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();
        JoinHandle { task }
    }
}

impl Handle {
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let schedule = {
            let stealers = self.stealers.clone();
            move |runnable| {
                WORK_QUEUE.with(|q| {
                    // Make sure this thread's stealer is populated
                    stealers
                        .write()
                        .entry(thread::current().id())
                        .or_insert(q.stealer());
                    q.push(runnable);
                })
            }
        };
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();

        while !task.is_finished() {
            if let Some(stolen_task) = self.find_task() {
                stolen_task.run();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        futures::executor::block_on(task)
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + Sync,
    {
        let injector = self.injector.clone();
        let schedule = move |runnable| injector.push(runnable);
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();
        JoinHandle { task }
    }

    fn find_task(&self) -> Option<Runnable> {
        let local = WORK_QUEUE.with(Rc::clone);
        // Pop a task from the local queue, if not empty.
        local.pop().or_else(|| {
            std::iter::repeat_with(|| {
                self.injector
                    .steal_batch_with_limit_and_pop(local.as_ref(), 1)
                    .or_else(|| self.stealers.read().values().map(|s| s.steal()).collect())
            })
            .find(|s| !s.is_retry())
            .and_then(|s| s.success())
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_basic() {
        let rt = Runtime::new(1);
        let jh = rt.spawn(async move { println!("spawned task!") });
        let handle = rt.handle();
        let h2 = handle.clone();

        handle.block_on(async move {
            let jh2 = h2.spawn(async move {
                println!("another spawn!");
            });
            println!("blocking here!");
            jh2.await;
            println!("Awaited the second future");
        });
        println!("Finished block_on block");
        futures::executor::block_on(jh);
    }
}
