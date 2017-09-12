use bytes::Bytes;
use std::marker::PhantomData;
use tk_bufstream::{Buf, Decode, Encode};
use tk_http::server::{Codec, Dispatcher, Error as TkError, Head};

const MAX_REQUEST_LENGTH: usize = 128_000; // Not expecting huge requests here

pub struct RequestDispatcher {}

impl<S> Dispatcher<S> for RequestDispatcher {
    type Codec = Box<Codec<S>>; // TODO

    fn headers_received(&mut self, headers: &Head) -> Result<Self::Codec, TkError> {
        unimplemented!()
    }
}

pub struct RequestCodec<B> {
    
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

/*
impl<B> Codec for RequestCodec<B> {
    type ResponseFuture = R::Future;

    fn recv_mode(&mut self) -> RecvMode {
        if self.request.as_ref().unwrap().websocket_handshake.is_some() {
            RecvMode::hijack()
        } else {
            RecvMode::buffered_upfront(self.max_request_length)
        }
    }

    fn data_received(&mut self, data: &[u8], end: bool)
        -> Result<Async<usize>, Error>
    {
        assert!(end);
        self.request.as_mut().unwrap().body = data.to_vec();
        Ok(Async::Ready(data.len()))
    }

    fn start_response(&mut self, e: Encoder<S>) -> R::Future {
        self.service.call(self.request.take().unwrap(), e)
    }

    fn hijack(&mut self, write_buf: WriteBuf<S>, read_buf: ReadBuf<S>){
        let inp = read_buf.framed(RequestCodec);
        let out = write_buf.framed(RequestCodec);
        self.handle.spawn(self.service.start_websocket(out, inp));
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
pub fn write_close(buf: &mut Buf, code: u16, reason: &[u8]) {
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

// Copied from zero_copy.rs
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L55-122
pub fn parse_frame<'x>(buf: &'x mut Buf, limit: usize, masked: bool)
    -> Result<Option<(Frame<'x>, usize)>, ErrorEnum>
{
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
        let mask = [buf[start-4], buf[start-3], buf[start-2], buf[start-1]];
        for idx in 0..size { // hopefully llvm is smart enough to optimize it
            buf[start + idx] ^= mask[idx % 4];
        }
    }
    let data = &buf[start..(start + size)];
    let frame = match opcode {
        0x9 => Ping(data),
        0xA => Pong(data),
        0x1 => Text(from_utf8(data)?),
        0x2 => Binary(data),
        // TODO(tailhook) implement shutdown packets
        0x8 => {
            if data.len() < 2 {
                Close(1006, "")
            } else {
                Close(BigEndian::read_u16(&data[..2]), from_utf8(&data[2..])?)
            }
        }
        x => return Err(ErrorEnum::InvalidOpcode(x)),
    };
    return Ok(Some((frame, start + size)));
}
