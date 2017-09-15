use bytes::Bytes;
use futures::{Async, Future, future};
use petronel;
use prost::Message;
use protobuf;
use std::rc::Rc;
use std::sync::Mutex;
use tk_bufstream::{ReadBuf, WriteBuf};
use tk_http::Status;
use tk_http::server::{Codec, Dispatcher, Encoder, EncoderDone, Error as TkError, Head, RecvMode,
                      WebsocketHandshake};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};
use websocket::{self, Frame};

const MAX_REQUEST_LENGTH: usize = 128_000; // Not expecting huge requests here
const MAX_PACKET_SIZE: usize = 10 << 20;

pub(crate) struct RequestDispatcher<S> {
    pub(crate) petronel_client: petronel::Client<WebsocketSubscriber<S>>,
    pub(crate) handle: Handle,
}

impl<S> Dispatcher<S> for RequestDispatcher<S>
where
    S: AsyncRead + AsyncWrite + 'static,
{
    type Codec = RequestCodec<S>;

    fn headers_received(&mut self, headers: &Head) -> Result<Self::Codec, TkError> {
        let websocket_handshake = headers.get_websocket_upgrade().unwrap_or(None);

        let petronel_client = if websocket_handshake.is_some() {
            Some(self.petronel_client.clone())
        } else {
            None
        };

        Ok(RequestCodec {
            petronel_client,
            websocket_handshake,
            subscriber: None,
            handle: self.handle.clone(),
        })
    }
}

pub(crate) struct RequestCodec<S> {
    petronel_client: Option<petronel::Client<WebsocketSubscriber<S>>>,
    websocket_handshake: Option<WebsocketHandshake>,
    subscriber: Option<WebsocketSubscriber<S>>,
    handle: Handle,
}

impl<S> Codec<S> for RequestCodec<S>
where
    S: AsyncRead + AsyncWrite + 'static,
{
    type ResponseFuture = Box<Future<Item = EncoderDone<S>, Error = TkError>>;

    fn recv_mode(&mut self) -> RecvMode {
        if self.websocket_handshake.is_some() {
            RecvMode::hijack()
        } else {
            RecvMode::buffered_upfront(MAX_REQUEST_LENGTH)
        }
    }

    fn data_received(&mut self, data: &[u8], _end: bool) -> Result<Async<usize>, TkError> {
        // TODO: Handle request
        Ok(Async::Ready(data.len()))
    }

    fn start_response(&mut self, mut e: Encoder<S>) -> Self::ResponseFuture {
        if let Some(ref ws) = self.websocket_handshake {
            e.status(Status::SwitchingProtocol);
            e.add_header("Connection", "upgrade").unwrap();
            e.add_header("Upgrade", "websocket").unwrap();
            e.format_header("Sec-Websocket-Accept", &ws.accept).unwrap();
            e.format_header("Sec-Websocket-Protocol", "binary").unwrap();
            e.done_headers().unwrap();
            Box::new(future::ok(e.done())) as Self::ResponseFuture
        } else {
            // TODO
            let body = "Not implemented yet";

            e.status(Status::Ok);
            e.add_header("Content-Type", "text/plain").unwrap();
            e.add_length(body.as_bytes().len() as u64).unwrap();

            if e.done_headers().unwrap() {
                e.write_body(body.as_bytes());
            }

            Box::new(future::ok(e.done())) as Self::ResponseFuture
        }
    }

    fn hijack(&mut self, write_buf: WriteBuf<S>, read_buf: ReadBuf<S>) {
        let subscription_future = self.petronel_client
            .take()
            .expect("petronel_client should be Some")
            .subscribe(WebsocketSubscriber {
                write_buf: Rc::new(Mutex::new(write_buf)),
            })
            .map_err(|_| ())
            .and_then(|subscription| {
                WebsocketReader {
                    read_buf,
                    subscription,
                }
            });

        self.handle.spawn(subscription_future);
    }
}

pub(crate) struct WebsocketSubscriber<S> {
    // TODO: Better way to do this that doesn't involve Rc<Mutex<...>>
    write_buf: Rc<Mutex<WriteBuf<S>>>,
}

impl<S> Clone for WebsocketSubscriber<S> {
    fn clone(&self) -> Self {
        WebsocketSubscriber { write_buf: self.write_buf.clone() }
    }
}

impl<S> petronel::Subscriber for WebsocketSubscriber<S>
where
    S: AsyncWrite,
{
    type Item = Bytes;

    fn send(&mut self, message: &Self::Item) -> Result<(), ()> {
        // TODO: Better way of doing this that doesn't require Mutex
        let mut write_buf = self.write_buf.lock().unwrap();
        write_buf.out_buf.extend(message);
        write_buf.flush().map_err(|_| ())
    }
}

pub struct WebsocketReader<S> {
    read_buf: ReadBuf<S>,
    subscription: petronel::Subscription<WebsocketSubscriber<S>>,
}

impl<S> WebsocketReader<S> {
    fn handle_frame(frame: Frame<&[u8]>) {
        let parsed = if let Frame::Binary(bytes) = frame {
            protobuf::RequestMessage::decode(bytes)
        } else {
            return; // TODO: Close websocket
        };

        println!("{:#?}", parsed);
    }
}

impl<S> Future for WebsocketReader<S>
where
    S: AsyncRead,
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            let amount_consumed = {
                websocket::parse_frame(&mut self.read_buf.in_buf, MAX_PACKET_SIZE, true)
                    .map_err(|_| ())?
                    .map(|(frame, amount_consumed)| {
                        // TODO: Convert to message, do stuff with it
                        Self::handle_frame(frame);

                        amount_consumed
                    })
            };

            if let Some(amount) = amount_consumed {
                self.read_buf.in_buf.consume(amount);
            } else {
                let bytes_read = self.read_buf.read().map_err(|_| ())?;

                if bytes_read == 0 {
                    if self.read_buf.done() {
                        return Ok(Async::Ready(()));
                    } else {
                        return Ok(Async::NotReady);
                    }
                }
            }
        }
    }
}
