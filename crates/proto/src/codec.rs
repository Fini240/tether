//! Length-delimited bincode framing.

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::message::Frame;

/// Hard ceiling on a single frame. File chunks are the only thing that gets
/// near it, and they are chunked well below this. A peer that claims a larger
/// frame is either broken or hostile, so we drop the connection.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

/// Bytes per file chunk. Small enough that a big transfer cannot stall
/// interactive input behind a single write on the shared connection.
pub const FILE_CHUNK_LEN: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed frame: {0}")]
    Decode(#[from] bincode::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    TooLarge(usize),
}

/// Wire format: a 4-byte big-endian length, then that many bincode bytes.
#[derive(Debug, Default)]
pub struct Codec {
    /// Length of the frame we are mid-way through reading, if any.
    pending: Option<usize>,
}

impl Codec {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Decoder for Codec {
    type Item = Frame;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        let len = match self.pending {
            Some(len) => len,
            None => {
                if src.len() < 4 {
                    src.reserve(4 - src.len());
                    return Ok(None);
                }
                let len = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
                if len > MAX_FRAME_LEN {
                    return Err(CodecError::TooLarge(len));
                }
                src.advance(4);
                self.pending = Some(len);
                len
            }
        };

        if src.len() < len {
            // Ask for the whole remainder up front so we don't grow the buffer
            // once per read syscall on a large chunk.
            src.reserve(len - src.len());
            return Ok(None);
        }

        let bytes = src.split_to(len);
        self.pending = None;
        Ok(Some(bincode::deserialize(&bytes)?))
    }
}

impl Encoder<Frame> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        let payload = bincode::serialize(&item)?;
        if payload.len() > MAX_FRAME_LEN {
            return Err(CodecError::TooLarge(payload.len()));
        }
        dst.reserve(4 + payload.len());
        dst.put_u32(payload.len() as u32);
        dst.put_slice(&payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{InputEvent, KeyCode, Modifiers};

    fn roundtrip(frame: Frame) -> Frame {
        let mut buf = BytesMut::new();
        Codec::new().encode(frame, &mut buf).unwrap();
        Codec::new().decode(&mut buf).unwrap().unwrap()
    }

    #[test]
    fn roundtrips_a_key_event() {
        let frame = Frame::Input(InputEvent::Key {
            key: KeyCode::C,
            pressed: true,
            modifiers: Modifiers::META,
            repeat: false,
        });
        assert_eq!(roundtrip(frame.clone()), frame);
    }

    #[test]
    fn roundtrips_a_mouse_move() {
        let frame = Frame::Input(InputEvent::MouseMove { x: -1920, y: 400 });
        assert_eq!(roundtrip(frame.clone()), frame);
    }

    #[test]
    fn decodes_a_frame_split_across_reads() {
        let frame = Frame::Input(InputEvent::MouseMove { x: 7, y: 9 });
        let mut whole = BytesMut::new();
        Codec::new().encode(frame.clone(), &mut whole).unwrap();

        // Feed one byte at a time. Everything before the final byte must
        // decode to `None` — note the decoder drains the length prefix out of
        // `buf`, so the loop counts bytes *fed* rather than bytes buffered.
        let mut codec = Codec::new();
        let mut buf = BytesMut::new();
        let total = whole.len();
        for (fed, byte) in whole.iter().enumerate() {
            buf.put_u8(*byte);
            let decoded = codec.decode(&mut buf).unwrap();
            if fed + 1 < total {
                assert!(decoded.is_none(), "decoded early after {} bytes", fed + 1);
            } else {
                assert_eq!(decoded.unwrap(), frame);
            }
        }
    }

    #[test]
    fn rejects_an_oversized_length_prefix() {
        let mut buf = BytesMut::new();
        buf.put_u32(u32::MAX);
        assert!(matches!(
            Codec::new().decode(&mut buf),
            Err(CodecError::TooLarge(_))
        ));
    }
}
