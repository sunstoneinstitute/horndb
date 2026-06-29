//! Body wrapper that tallies bytes and records them to the metrics registry.
//!
//! `CountingBody<B>` is transparent: it yields exactly the same frames as the
//! inner body. When the inner body signals end-of-stream (poll_frame returns
//! `Ready(None)`), it fires a single `inc_by` on the appropriate counter.
//! The `done` flag prevents a double-observation if the body is polled again
//! after returning `None`.

use bytes::Bytes;
use horndb_metrics::labels::{Endpoint, EndpointLabel};
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Which direction a `CountingBody` is tracking.
#[derive(Clone, Copy)]
pub enum Direction {
    Request,
    Response,
}

pin_project_lite::pin_project! {
    /// A body adaptor that counts the total bytes delivered then records them.
    pub struct CountingBody<B> {
        #[pin]
        inner: B,
        bytes: u64,
        endpoint: Endpoint,
        dir: Direction,
        done: bool,
    }
}

impl<B> CountingBody<B> {
    pub fn new(inner: B, endpoint: Endpoint, dir: Direction) -> Self {
        Self {
            inner,
            bytes: 0,
            endpoint,
            dir,
            done: false,
        }
    }
}

fn observe(endpoint: &Endpoint, dir: Direction, bytes: u64) {
    let m = horndb_metrics::metrics();
    let fam = match dir {
        Direction::Request => &m.sparql.request_bytes,
        Direction::Response => &m.sparql.response_bytes,
    };
    fam.get_or_create(&EndpointLabel {
        endpoint: endpoint.clone(),
    })
    .inc_by(bytes);
}

impl<B> Body for CountingBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        match this.inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref() {
                    *this.bytes += d.len() as u64;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                if !*this.done {
                    *this.done = true;
                    observe(this.endpoint, *this.dir, *this.bytes);
                }
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}
