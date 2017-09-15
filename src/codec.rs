use byteorder::{BigEndian, ByteOrder};
use bytes::{Bytes, BytesMut};
use futures::{Async, Future, future};
use prost::Message;
use protobuf;
use std::io::Write;
use std::marker::PhantomData;
use tk_bufstream::{Buf, Decode, Encode};
use tk_bufstream::{ReadBuf, WriteBuf};
use tk_http::Status;
use tk_http::server::{Codec, Dispatcher, Encoder, EncoderDone, Error as TkError, Head, RecvMode,
                      WebsocketHandshake};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};

const MAX_REQUEST_LENGTH: usize = 128_000; // Not expecting huge requests here
const MAX_PACKET_SIZE: usize = 10 << 20;

const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;
const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;

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

    fn data_received(&mut self, data: &[u8], end: bool) -> Result<Async<usize>, TkError> {
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

        if let Some(message_bytes) = serialize_protobuf(response) {
            write_buf.out_buf.extend(&message_bytes);
            let _ = write_buf.flush();
        } else {
            write_close(&mut write_buf.out_buf, 1011, b"Internal server error");
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
                parse_frame(&mut self.read_buf.in_buf, MAX_PACKET_SIZE, true)
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

pub enum Frame<B>
where
    B: AsRef<[u8]>,
{
    Ping(B), // 0x9
    Pong(B), // 0xA
    Text(B), // 0x1
    Binary(B), // 0x2
    Close(u16, B),
}

// Serialize protobuf message as part of a binary websocket frame.
// If serialization fails somehow, we just give up.
//
// Based on zero_copy.rs from tk-http.
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L124-L162
fn serialize_protobuf<M>(message: M) -> Option<Bytes>
where
    M: Message,
{
    let first_byte = OPCODE_BINARY | 0x80;

    let mut bytes = match message.encoded_len() {
        len @ 0...125 => {
            let init = [first_byte, len as u8];
            let mut b = BytesMut::with_capacity(init.len() + len);
            b.extend(&init);
            b
        }
        len @ 126...65535 => {
            let init = [first_byte, 126, (len >> 8) as u8, (len & 0xFF) as u8];
            let mut b = BytesMut::with_capacity(init.len() + len);
            b.extend_from_slice(&init);
            b
        }
        len => {
            let init = [
                first_byte,
                127,
                ((len >> 56) & 0xFF) as u8,
                ((len >> 48) & 0xFF) as u8,
                ((len >> 40) & 0xFF) as u8,
                ((len >> 32) & 0xFF) as u8,
                ((len >> 24) & 0xFF) as u8,
                ((len >> 16) & 0xFF) as u8,
                ((len >> 8) & 0xFF) as u8,
                (len & 0xFF) as u8,
            ];

            let mut b = BytesMut::with_capacity(init.len() + len);
            b.extend_from_slice(&init);
            b
        }
    };

    if let Err(e) = message.encode(&mut bytes) {
        println!("{:?}", e);
        None
    } else {
        Some(bytes.freeze())
    }
}

// Copied from zero_copy.rs, but with mask removed
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L164-L185
pub(crate) fn write_close(buf: &mut Buf, code: u16, reason: &[u8]) {
    assert!(reason.len() <= 123);
    buf.extend(
        &[
            0x88,
            (reason.len() + 2) as u8,
            (code >> 8) as u8,
            (code & 0xFF) as u8,
        ],
    );
    buf.extend(reason);
}

pub(crate) enum ErrorEnum {
    TooLong,
    Fragmented,
    Unmasked,
    InvalidOpcode(u8),
}

// Copied from zero_copy.rs, but with errors removed
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L55-122
pub(crate) fn parse_frame<'a>(
    buf: &'a mut Buf,
    limit: usize,
    masked: bool,
) -> Result<Option<(Frame<&'a [u8]>, usize)>, ErrorEnum> {
    use self::Frame::*;

    if buf.len() < 2 {
        return Ok(None);
    }
    let (size, fsize) = {
        match buf[1] & 0x7F {
            126 => {
                if buf.len() < 4 {
                    return Ok(None);
                }
                (BigEndian::read_u16(&buf[2..4]) as u64, 4)
            }
            127 => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                (BigEndian::read_u64(&buf[2..10]), 10)
            }
            size => (size as u64, 2),
        }
    };
    if size > limit as u64 {
        return Err(ErrorEnum::TooLong);
    }
    let size = size as usize;
    let start = fsize + if masked { 4 } else { 0 } /* mask size */;
    if buf.len() < start + size {
        return Ok(None);
    }

    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0F;
    // TODO(tailhook) should we assert that reserved bits are zero?
    let mask = buf[1] & 0x80 != 0;
    if !fin {
        return Err(ErrorEnum::Fragmented);
    }
    if mask != masked {
        return Err(ErrorEnum::Unmasked);
    }
    if mask {
        let mask = [
            buf[start - 4],
            buf[start - 3],
            buf[start - 2],
            buf[start - 1],
        ];
        for idx in 0..size {
            // hopefully llvm is smart enough to optimize it
            buf[start + idx] ^= mask[idx % 4];
        }
    }
    let data = &buf[start..(start + size)];
    let frame = match opcode {
        0x9 => Ping(data),
        0xA => Pong(data),
        0x1 => Text(data),
        0x2 => Binary(data),
        0x8 => {
            if data.len() < 2 {
                Close(1006, [].as_ref())
            } else {
                Close(BigEndian::read_u16(&data[..2]), &data[2..])
            }
        }
        x => return Err(ErrorEnum::InvalidOpcode(x)),
    };
    return Ok(Some((frame, start + size)));
}
