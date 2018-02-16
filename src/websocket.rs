use byteorder::{BigEndian, ByteOrder};
use bytes::{Bytes, BytesMut};
use prost::Message;
use tk_bufstream::Buf;

const OPCODE_BINARY: u8 = 0x2;
pub(crate) const EMPTY_PING: &[u8] = &[0x9 | 0x80, 0];

pub(crate) enum Frame<B>
where
    B: AsRef<[u8]>,
{
    Ping(B),   // 0x9
    Pong(B),   // 0xA
    Text(B),   // 0x1
    Binary(B), // 0x2
    Close(u16, B),
}

// Serialize protobuf message as part of a binary websocket frame.
// If serialization fails somehow, we just give up.
//
// Based on zero_copy.rs from tk-http.
// https://github.com/swindon-rs/tk-http/blob/3520464/src/websocket/zero_copy.rs#L124-L162
pub(crate) fn serialize_protobuf<M>(message: M) -> Option<Bytes>
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
    buf.extend(&[
        0x88,
        (reason.len() + 2) as u8,
        (code >> 8) as u8,
        (code & 0xFF) as u8,
    ]);
    buf.extend(reason);
}

pub(crate) enum ErrorEnum {
    TooLong,
    Fragmented,
    Unmasked,
    InvalidOpcode(u8),
}

// Copied from zero_copy.rs
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
