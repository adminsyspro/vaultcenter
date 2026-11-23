//! Preserve the SDK's replayable file ranges while pacing each attempt.
use crate::bandwidth::{BandwidthError, VtlBandwidth};
use aws_smithy_types::{
    body::{Error, SdkBody},
    byte_stream::ByteStream,
};
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

type Permit = Pin<Box<dyn Future<Output = Result<(), BandwidthError>> + Send + Sync>>;
struct LimitedBody {
    inner: SdkBody,
    limiter: VtlBandwidth,
    buffered: Bytes,
    waiting: Option<Permit>,
    failed: bool,
}

pub(crate) fn limit(body: ByteStream, limiter: VtlBandwidth) -> ByteStream {
    // ByteStream::map delegates to SdkBody::map, which rebuilds this wrapper
    // for every SDK retry, including its file range reader and permits.
    body.map(move |inner| {
        SdkBody::from_body_1_x(LimitedBody {
            inner,
            limiter: limiter.clone(),
            buffered: Bytes::new(),
            waiting: None,
            failed: false,
        })
    })
}

impl Body for LimitedBody {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(None);
        }
        loop {
            if let Some(waiting) = this.waiting.as_mut() {
                match waiting.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => {
                        this.failed = true;
                        this.waiting = None;
                        return Poll::Ready(Some(Err(Box::new(error))));
                    }
                    Poll::Ready(Ok(())) => {
                        this.waiting = None;
                        let chunk = this.buffered.split_to(this.buffered.len().min(65_536));
                        return Poll::Ready(Some(Ok(Frame::data(chunk))));
                    }
                }
            }
            if this.buffered.is_empty() {
                match Pin::new(&mut this.inner).poll_frame(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(Err(error))) => {
                        this.failed = true;
                        return Poll::Ready(Some(Err(error)));
                    }
                    Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                        Ok(bytes) => {
                            this.buffered = bytes;
                            if this.buffered.is_empty() {
                                cx.waker().wake_by_ref();
                                return Poll::Pending;
                            }
                        }
                        Err(trailers) => return Poll::Ready(Some(Ok(trailers))),
                    },
                }
            }
            let limiter = this.limiter.clone();
            let count = this.buffered.len().min(65_536);
            this.waiting = Some(Box::pin(async move { limiter.acquire(count).await }));
        }
    }
    fn is_end_stream(&self) -> bool {
        self.failed
            || (self.buffered.is_empty() && self.waiting.is_none() && self.inner.is_end_stream())
    }
    fn size_hint(&self) -> SizeHint {
        let inner = Body::size_hint(&self.inner);
        let mut hint = SizeHint::new();
        hint.set_lower(inner.lower().saturating_add(self.buffered.len() as u64));
        if let Some(hi) = inner.upper() {
            hint.set_upper(hi.saturating_add(self.buffered.len() as u64));
        }
        hint
    }
}
