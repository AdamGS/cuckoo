//! Utility futures

use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Default, Debug)]
struct Yield {
    done: bool,
}

impl Future for Yield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        println!("Polling {:?}", self);
        if self.done {
            return Poll::Ready(());
        }

        self.done = true;
        cx.waker().wake_by_ref();

        Poll::Pending
    }
}

pub async fn yield_now() {
    Yield::default().await
}

#[cfg(test)]
mod tests {
    use crate::Runtime;

    use super::*;

    #[test]
    #[should_panic(expected = "got here")]
    fn test_basic() {
        let rt = Runtime::new(0);

        rt.block_on(async move {
            println!("hello");
            yield_now().await;
            panic!("got here");
        });
    }
}
