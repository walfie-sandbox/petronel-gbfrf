use futures::{Async, Future, future};
use prost::Message;
use protobuf;
use tk_bufstream::{ReadBuf, WriteBuf};
use tk_http::Status;
use tk_http::server::{Codec, Dispatcher, Encoder, EncoderDone, Error as TkError, Head, RecvMode,
                      WebsocketHandshake};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};
use websocket::{self, Frame};

const MAX_REQUEST_LENGTH: usize = 128_000; // Not expecting huge requests here
const MAX_PACKET_SIZE: usize = 10 << 20;

pub struct RequestDispatcher {
    pub handle: Handle,
}

impl<S> Dispatcher<S> for RequestDispatcher
where
    S: AsyncRead + AsyncWrite + 'static,
{
    type Codec = RequestCodec;

    fn headers_received(&mut self, headers: &Head) -> Result<Self::Codec, TkError> {
        let websocket_handshake = headers.get_websocket_upgrade().unwrap_or(None);

        Ok(RequestCodec {
            websocket_handshake,
            handle: self.handle.clone(),
        })
    }
}

pub struct RequestCodec {
    websocket_handshake: Option<WebsocketHandshake>,
    handle: Handle,
}

impl<S> Codec<S> for RequestCodec
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

    fn hijack(&mut self, mut write_buf: WriteBuf<S>, read_buf: ReadBuf<S>) {
        let profile_image = {
            "https://avatars0.githubusercontent.com/u/11370525?v=4&s=64"
        }.into();

        let response = protobuf::ResponseMessage {
            data: Some(protobuf::response_message::Data::RaidTweetMessage(
                protobuf::RaidTweetResponse {
                    boss_name: "Lv60 ユグドラシル・マグナ".into(),
                    raid_id: "ABCD1234".into(),
                    screen_name: "walfieee".into(),
                    tweet_id: 42069,
                    profile_image,
                    text: "アイカツ！".into(),
                    created_at: 1505071158723,
                    language: protobuf::Language::English as i32,
                },
            )),
        };

        if let Some(message_bytes) = websocket::serialize_protobuf(response) {
            write_buf.out_buf.extend(&message_bytes);
            let _ = write_buf.flush();
        } else {
            websocket::write_close(&mut write_buf.out_buf, 1011, b"Internal server error");
        }

        self.handle.spawn(WebsocketHandler {
            write_buf,
            read_buf,
        });
    }
}

pub struct WebsocketHandler<S> {
    write_buf: WriteBuf<S>,
    read_buf: ReadBuf<S>,
}

impl<S> WebsocketHandler<S> {
    fn handle_frame(frame: Frame<&[u8]>) {
        let parsed = if let Frame::Binary(bytes) = frame {
            protobuf::RequestMessage::decode(bytes)
        } else {
            return; // TODO: Close websocket
        };

        println!("{:#?}", parsed);
    }
}

impl<S> Future for WebsocketHandler<S>
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
