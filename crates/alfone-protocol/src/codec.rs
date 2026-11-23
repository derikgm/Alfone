use crate::message::Message;

/// 4-byte big-endian length prefix followed by a postcard payload.
pub const LENGTH_PREFIX: usize = 4;

/// Hard ceiling for one control message. Clipboard data is chunked well below
/// this; a larger frame means a bug or a hostile peer.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("frame of {size} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    TooLarge { size: usize },
    #[error("truncated frame: header says {expected}, got {actual}")]
    Truncated { expected: usize, actual: usize },
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
}

/// Serialize a message with its length prefix.
pub fn encode_frame(message: &Message) -> Result<Vec<u8>, CodecError> {
    let body = postcard::to_stdvec(message)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { size: body.len() });
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Serialize a message body without the length prefix.
pub fn encode_body(message: &Message) -> Result<Vec<u8>, CodecError> {
    let body = postcard::to_stdvec(message)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { size: body.len() });
    }
    Ok(body)
}

/// Deserialize a body produced by [`encode_body`].
pub fn decode_body(body: &[u8]) -> Result<Message, CodecError> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { size: body.len() });
    }
    Ok(postcard::from_bytes(body)?)
}

/// Deserialize a full frame produced by [`encode_frame`].
pub fn decode_frame(frame: &[u8]) -> Result<Message, CodecError> {
    if frame.len() < LENGTH_PREFIX {
        return Err(CodecError::Truncated {
            expected: LENGTH_PREFIX,
            actual: frame.len(),
        });
    }
    let expected = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if expected > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { size: expected });
    }
    let body = &frame[LENGTH_PREFIX..];
    if body.len() != expected {
        return Err(CodecError::Truncated {
            expected,
            actual: body.len(),
        });
    }
    decode_body(body)
}

/// Read the announced length without decoding the body.
pub fn announced_len(header: &[u8; LENGTH_PREFIX]) -> Result<usize, CodecError> {
    let len = u32::from_be_bytes(*header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { size: len });
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{InputEvent, Message};

    #[test]
    fn frame_roundtrip() {
        let msg = Message::Input(InputEvent::PointerRel { dx: -5, dy: 7 });
        let frame = encode_frame(&msg).unwrap();
        assert_eq!(decode_frame(&frame).unwrap(), msg);
    }

    #[test]
    fn rejects_oversized() {
        let mut frame = vec![0u8; LENGTH_PREFIX];
        frame[..4].copy_from_slice(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
        assert!(matches!(
            decode_frame(&frame),
            Err(CodecError::TooLarge { .. })
        ));
    }

    #[test]
    fn rejects_truncated() {
        assert!(decode_frame(&[0, 0]).is_err());
        let frame = encode_frame(&Message::Ping { id: 1 }).unwrap();
        assert!(decode_frame(&frame[..frame.len() - 1]).is_err());
    }
}
