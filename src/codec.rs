use byteorder::{BigEndian, ByteOrder};
use bytes::Bytes;
use futures::{Async, Future};
use std::marker::PhantomData;
use tk_bufstream::{Buf, Decode, Encode};
use tk_bufstream::{ReadBuf, WriteBuf};
use tk_http::server::{Codec, Dispatcher, Encoder, EncoderDone, Error as TkError, Head, RecvMode};

const MAX_REQUEST_LENGTH: usize = 128_000; // Not expecting huge requests here

pub struct RequestDispatcher {}

impl<S> Dispatcher<S> for RequestDispatcher {
    type Codec = RequestCodec;

    fn headers_received(&mut self, headers: &Head) -> Result<Self::Codec, TkError> {
        unimplemented!()
    }
}

pub struct RequestCodec {
    is_websocket: bool,
}

impl<S> Codec<S> for RequestCodec {
    type ResponseFuture = Box<Future<Item = EncoderDone<S>, Error = TkError>>;

    fn recv_mode(&mut self) -> RecvMode {
        if self.is_websocket {
            RecvMode::hijack()
        } else {
            RecvMode::buffered_upfront(MAX_REQUEST_LENGTH)
        }
    }

    fn data_received(&mut self, data: &[u8], end: bool) -> Result<Async<usize>, TkError> {
        unimplemented!()
    }

    fn start_response(&mut self, e: Encoder<S>) -> Self::ResponseFuture {
        unimplemented!()
    }

    fn hijack(&mut self, write_buf: WriteBuf<S>, read_buf: ReadBuf<S>) {
        // self.handle.spawn(self.service.start_websocket(out, inp));
        unimplemented!()
    }
}

pub enum Frame<B>
where
    B: AsRef<[u8]>,
{
    Ping(B),
    Pong(B),
    Text(B),
    Binary(B),
    Close(u16, B),
}

/*
impl<B> Encode for RequestCodec<B>
where
    B: AsRef<[u8]>,
{
    type Item = Frame<B>;

    fn encode(&mut self, data: Self::Item, buf: &mut Buf) {
        use self::Frame::*;
        match data {
            Ping(data) => write_packet(buf, 0x9, data.as_ref()),
            Pong(data) => write_packet(buf, 0xA, data.as_ref()),
            Text(data) => write_packet(buf, 0x1, data.as_ref()),
            Binary(data) => write_packet(buf, 0x2, data.as_ref()),
            Close(code, reason) => write_close(buf, code, reason.as_ref()),
        }
    }
}
*/

// Copied from zero_copy.rs, but with mask removed
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L124-L162
pub fn write_packet(buf: &mut Buf, opcode: u8, data: &[u8]) {
    debug_assert!(opcode & 0xF0 == 0);
    let first_byte = opcode | 0x80; // always fin
    match data.len() {
        len @ 0...125 => {
            buf.extend(&[first_byte, len as u8]);
        }
        len @ 126...65535 => {
            buf.extend(&[first_byte, 126, (len >> 8) as u8, (len & 0xFF) as u8]);
        }
        len => {
            buf.extend(
                &[
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
                ],
            );
        }
    }

    buf.extend(data);
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
