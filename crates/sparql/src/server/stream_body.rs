//! Streaming response body fed by a bounded channel from the blocking
//! serializer thread (see `server/query.rs::stream_select`). Design:
//! `docs/specs/SPEC-22-http-streaming-results.md`.

use crate::error::SparqlError;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Receiver;

/// `http_body::Body` over the pre-buffered first chunk (header + first
/// result chunk, produced before the 200 was committed) followed by
/// whatever the blocking serializer sends. An `Err` item aborts the
/// response mid-body: hyper drops the connection without the
/// chunked-encoding terminator (HTTP/2: RST_STREAM), so clients detect the
/// truncation at the protocol level.
///
/// Dropping this body drops the receiver, which makes the serializer's
/// `blocking_send` fail — the blocking task returns early and releases the
/// store read lock. That is the client-disconnect cancellation path.
// Constructed here; consumed by the server/query.rs streaming handler (Task 6).
#[allow(dead_code)]
pub(crate) struct ChannelBody {
    first: Option<Bytes>,
    rx: Receiver<Result<Bytes, SparqlError>>,
}

impl ChannelBody {
    #[allow(dead_code)]
    pub(crate) fn new(first: Bytes, rx: Receiver<Result<Bytes, SparqlError>>) -> Self {
        Self {
            first: Some(first),
            rx,
        }
    }
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = SparqlError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, SparqlError>>> {
        let this = self.get_mut();
        if let Some(b) = this.first.take() {
            return Poll::Ready(Some(Ok(Frame::data(b))));
        }
        this.rx
            .poll_recv(cx)
            .map(|opt| opt.map(|r| r.map(Frame::data)))
    }

    fn size_hint(&self) -> SizeHint {
        // Length unknown until the stream ends — forces chunked encoding.
        SizeHint::default()
    }
}

#[cfg(test)]
mod tests {
    use super::ChannelBody;
    use crate::error::SparqlError;
    use bytes::Bytes;
    use http_body::Body as _;
    use std::pin::Pin;

    #[tokio::test]
    async fn yields_first_then_channel_frames_then_ends() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut body = ChannelBody::new(Bytes::from_static(b"head+chunk1"), rx);
        tx.send(Ok(Bytes::from_static(b"chunk2"))).await.unwrap();
        tx.send(Ok(Bytes::from_static(b"footer"))).await.unwrap();
        drop(tx);

        let mut got: Vec<Bytes> = Vec::new();
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
        {
            got.push(
                frame
                    .expect("clean stream")
                    .into_data()
                    .expect("data frame"),
            );
        }
        assert_eq!(
            got,
            vec![
                Bytes::from_static(b"head+chunk1"),
                Bytes::from_static(b"chunk2"),
                Bytes::from_static(b"footer"),
            ]
        );
    }

    #[tokio::test]
    async fn err_item_surfaces_as_body_error() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut body = ChannelBody::new(Bytes::from_static(b"head"), rx);
        tx.send(Err(SparqlError::Executor("mid-stream".into())))
            .await
            .unwrap();
        drop(tx);

        let first = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap();
        assert!(first.is_ok(), "pre-buffered first chunk is clean");
        let second = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap();
        assert!(second.is_err(), "the Err item must abort the body");
    }
}
