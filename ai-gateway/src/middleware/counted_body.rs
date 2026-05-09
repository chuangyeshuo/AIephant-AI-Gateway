//! Fire a one-shot callback when the response body is fully consumed or dropped
//! (in-flight count, Redis DECR, etc.).

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::{Body, Frame};

pub struct CountedBody<B> {
    inner: B,
    released: Arc<AtomicBool>,
    on_release: Arc<dyn Fn() + Send + Sync>,
}

impl<B> CountedBody<B> {
    pub fn new(inner: B, on_release: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            inner,
            released: Arc::new(AtomicBool::new(false)),
            on_release,
        }
    }

    fn fire_release(&self) {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            (self.on_release)();
        }
    }
}

impl<B: Unpin> Body for CountedBody<B>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let out = Pin::new(&mut self.inner).poll_frame(cx);
        let terminate = matches!(&out, Poll::Ready(None | Some(Err(_))));
        let out = out;
        if terminate {
            self.get_mut().fire_release();
        }
        out
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

impl<B> Drop for CountedBody<B> {
    fn drop(&mut self) {
        self.fire_release();
    }
}
