use async_task::Task;
use pin_project_lite::pin_project;

pin_project! {
    pub struct JoinHandle<T> {
        #[pin]
        pub(crate) task: Option<Task<T>>,
    }

    impl<T> PinnedDrop for JoinHandle<T> {
        fn drop(this: Pin<&mut Self>) {
            if let Some(task) = this.project().task.take() {
                task.detach();
            }
        }
    }
}

impl<T> JoinHandle<T> {
    pub async fn abort(mut self) {
        if let Some(task) = self.task.take() {
            task.cancel().await;
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(task) = self.project().task.as_pin_mut() {
            task.poll(cx)
        } else {
            unreachable!("Polled an empty JoinHandle");
        }
    }
}
