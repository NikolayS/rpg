use crate::client::{InnerClient, Responses};
use crate::codec::FrontendMessage;
use crate::connection::RequestMessages;
use crate::query::extract_row_affected;
use crate::{query, slice_iter, Error, Statement};
use bytes::{Buf, BufMut, BytesMut};
use futures_channel::mpsc;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use log::debug;
use pin_project_lite::pin_project;
use postgres_protocol::message::backend::Message;
use postgres_protocol::message::frontend;
use postgres_protocol::message::frontend::CopyData;
use std::future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

enum CopyInMessage {
    Message(FrontendMessage),
    /// Sentinel sent by `copy_in()` after the server confirms it entered copy
    /// mode (CopyInResponse).  `CopyInReceiver` uses this to decide whether a
    /// CopyFail must be sent when the sender is dropped prematurely.
    CopyModeEntered,
    Done,
}

pub struct CopyInReceiver {
    receiver: mpsc::Receiver<CopyInMessage>,
    done: bool,
    /// Set to `true` once `CopyModeEntered` is received, meaning the server is
    /// actually in COPY IN mode and a CopyFail is needed to abort.
    in_copy_mode: bool,
}

impl CopyInReceiver {
    fn new(receiver: mpsc::Receiver<CopyInMessage>) -> CopyInReceiver {
        CopyInReceiver {
            receiver,
            done: false,
            in_copy_mode: false,
        }
    }
}

impl Stream for CopyInReceiver {
    type Item = FrontendMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<FrontendMessage>> {
        if self.done {
            return Poll::Ready(None);
        }

        match ready!(self.receiver.poll_next_unpin(cx)) {
            Some(CopyInMessage::Message(message)) => Poll::Ready(Some(message)),
            Some(CopyInMessage::CopyModeEntered) => {
                // Internal signal — not a wire message.  Mark that we are now
                // in copy mode so that a premature sender drop sends CopyFail.
                self.in_copy_mode = true;
                // Wake ourselves so poll_next is called again immediately for
                // the next real message.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Some(CopyInMessage::Done) => {
                self.done = true;
                let mut buf = BytesMut::new();
                frontend::copy_done(&mut buf);
                frontend::sync(&mut buf);
                Poll::Ready(Some(FrontendMessage::Raw(buf.freeze())))
            }
            None => {
                self.done = true;
                if self.in_copy_mode {
                    // Server entered copy mode but the client dropped the sink
                    // without calling finish() — send CopyFail to abort.
                    let mut buf = BytesMut::new();
                    frontend::copy_fail("", &mut buf).unwrap();
                    frontend::sync(&mut buf);
                    Poll::Ready(Some(FrontendMessage::Raw(buf.freeze())))
                } else {
                    // The server never entered copy mode (it rejected the COPY
                    // command with an ErrorResponse before CopyInResponse).
                    // Do NOT send CopyFail — it would be a protocol violation
                    // that terminates the connection.
                    Poll::Ready(None)
                }
            }
        }
    }
}

enum SinkState {
    Active,
    Closing,
    Reading,
}

pin_project! {
    /// A sink for `COPY ... FROM STDIN` query data.
    ///
    /// The copy *must* be explicitly completed via the `Sink::close` or `finish` methods. If it is
    /// not, the copy will be aborted.
    #[project(!Unpin)]
    pub struct CopyInSink<T> {
        #[pin]
        sender: mpsc::Sender<CopyInMessage>,
        responses: Responses,
        buf: BytesMut,
        state: SinkState,
        _p2: PhantomData<T>,
    }
}

impl<T> CopyInSink<T>
where
    T: Buf + 'static + Send,
{
    /// A poll-based version of `finish`.
    pub fn poll_finish(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<u64, Error>> {
        loop {
            match self.state {
                SinkState::Active => {
                    ready!(self.as_mut().poll_flush(cx))?;
                    let mut this = self.as_mut().project();
                    ready!(this.sender.as_mut().poll_ready(cx)).map_err(|_| Error::closed())?;
                    this.sender
                        .start_send(CopyInMessage::Done)
                        .map_err(|_| Error::closed())?;
                    *this.state = SinkState::Closing;
                }
                SinkState::Closing => {
                    let this = self.as_mut().project();
                    ready!(this.sender.poll_close(cx)).map_err(|_| Error::closed())?;
                    *this.state = SinkState::Reading;
                }
                SinkState::Reading => {
                    let this = self.as_mut().project();
                    match ready!(this.responses.poll_next(cx))? {
                        Message::CommandComplete(body) => {
                            let rows = extract_row_affected(&body)?;
                            return Poll::Ready(Ok(rows));
                        }
                        _ => return Poll::Ready(Err(Error::unexpected_message())),
                    }
                }
            }
        }
    }

    /// Completes the copy, returning the number of rows inserted.
    ///
    /// The `Sink::close` method is equivalent to `finish`, except that it does not return the
    /// number of rows.
    pub async fn finish(mut self: Pin<&mut Self>) -> Result<u64, Error> {
        future::poll_fn(|cx| self.as_mut().poll_finish(cx)).await
    }
}

impl<T> Sink<T> for CopyInSink<T>
where
    T: Buf + 'static + Send,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.project()
            .sender
            .poll_ready(cx)
            .map_err(|_| Error::closed())
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Error> {
        let this = self.project();

        let data: Box<dyn Buf + Send> = if item.remaining() > 4096 {
            if this.buf.is_empty() {
                Box::new(item)
            } else {
                Box::new(this.buf.split().freeze().chain(item))
            }
        } else {
            this.buf.put(item);
            if this.buf.len() > 4096 {
                Box::new(this.buf.split().freeze())
            } else {
                return Ok(());
            }
        };

        let data = CopyData::new(data).map_err(Error::encode)?;
        this.sender
            .start_send(CopyInMessage::Message(FrontendMessage::CopyData(data)))
            .map_err(|_| Error::closed())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let mut this = self.project();

        if !this.buf.is_empty() {
            ready!(this.sender.as_mut().poll_ready(cx)).map_err(|_| Error::closed())?;
            let data: Box<dyn Buf + Send> = Box::new(this.buf.split().freeze());
            let data = CopyData::new(data).map_err(Error::encode)?;
            this.sender
                .as_mut()
                .start_send(CopyInMessage::Message(FrontendMessage::CopyData(data)))
                .map_err(|_| Error::closed())?;
        }

        this.sender.poll_flush(cx).map_err(|_| Error::closed())
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_finish(cx).map_ok(|_| ())
    }
}

pub async fn copy_in<T>(client: &InnerClient, statement: Statement) -> Result<CopyInSink<T>, Error>
where
    T: Buf + 'static + Send,
{
    debug!("executing copy in statement {}", statement.name());

    let buf = query::encode(client, &statement, slice_iter(&[]))?;

    let (mut sender, receiver) = mpsc::channel(1);
    let receiver = CopyInReceiver::new(receiver);
    let mut responses = client.send(RequestMessages::CopyIn(receiver))?;

    sender
        .send(CopyInMessage::Message(FrontendMessage::Raw(buf)))
        .await
        .map_err(|_| Error::closed())?;

    match responses.next().await? {
        Message::BindComplete => {}
        _ => return Err(Error::unexpected_message()),
    }

    match responses.next().await? {
        Message::CopyInResponse(_) => {}
        _ => return Err(Error::unexpected_message()),
    }

    // Notify the CopyInReceiver that the server has entered copy mode.  This
    // ensures that if the returned CopyInSink is dropped without calling
    // finish(), CopyInReceiver will send CopyFail to abort the copy.  Without
    // this signal, CopyInReceiver would not send CopyFail (to avoid a protocol
    // violation when the server never entered copy mode).
    sender
        .send(CopyInMessage::CopyModeEntered)
        .await
        .map_err(|_| Error::closed())?;

    Ok(CopyInSink {
        sender,
        responses,
        buf: BytesMut::new(),
        state: SinkState::Active,
        _p2: PhantomData,
    })
}
