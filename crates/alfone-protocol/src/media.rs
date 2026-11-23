//! Payload formats for the low-latency channels.
//!
//! Audio and video travel as QUIC **datagrams**, not as postcard messages:
//! they are loss-tolerant and must never block the control channel. Each
//! datagram starts with a small fixed header so that lost/duplicated/reordered
//! packets can be detected.

/// 8-byte header: sequence (u32), capture timestamp in microseconds (u32).
pub const AUDIO_HEADER_LEN: usize = 8;
/// Keep datagrams below a typical 1200-byte QUIC/IP path MTU.
pub const MAX_AUDIO_PAYLOAD: usize = 1150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    pub sequence: u32,
    pub timestamp_us: u32,
}

pub fn encode_audio_packet(header: AudioHeader, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_AUDIO_PAYLOAD {
        return None;
    }
    let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + payload.len());
    out.extend_from_slice(&header.sequence.to_be_bytes());
    out.extend_from_slice(&header.timestamp_us.to_be_bytes());
    out.extend_from_slice(payload);
    Some(out)
}

pub fn decode_audio_packet(datagram: &[u8]) -> Option<(AudioHeader, &[u8])> {
    if datagram.len() < AUDIO_HEADER_LEN {
        return None;
    }
    let sequence = u32::from_be_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]);
    let timestamp_us = u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]);
    Some((
        AudioHeader {
            sequence,
            timestamp_us,
        },
        &datagram[AUDIO_HEADER_LEN..],
    ))
}

/// 9-byte header for one fragment of an encoded video frame.
///
/// A frame is split into `count` fragments of at most
/// [`MAX_VIDEO_FRAGMENT_PAYLOAD`] bytes. The receiver reassembles by
/// `frame_id`, drops incomplete frames after a short deadline and requests a
/// keyframe if a fragment of an intra frame is missing.
pub const VIDEO_HEADER_LEN: usize = 9;
pub const MAX_VIDEO_FRAGMENT_PAYLOAD: usize = 1150;

pub const VIDEO_FLAG_KEYFRAME: u8 = 1 << 0;
pub const VIDEO_FLAG_LAST: u8 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub frame_id: u32,
    pub index: u16,
    pub count: u16,
    pub flags: u8,
}

impl VideoHeader {
    pub fn is_keyframe(self) -> bool {
        self.flags & VIDEO_FLAG_KEYFRAME != 0
    }

    pub fn is_last(self) -> bool {
        self.flags & VIDEO_FLAG_LAST != 0
    }
}

pub fn encode_video_fragment(header: VideoHeader, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_VIDEO_FRAGMENT_PAYLOAD || header.count == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(VIDEO_HEADER_LEN + payload.len());
    out.extend_from_slice(&header.frame_id.to_be_bytes());
    out.extend_from_slice(&header.index.to_be_bytes());
    out.extend_from_slice(&header.count.to_be_bytes());
    out.push(header.flags);
    out.extend_from_slice(payload);
    Some(out)
}

pub fn decode_video_fragment(datagram: &[u8]) -> Option<(VideoHeader, &[u8])> {
    if datagram.len() < VIDEO_HEADER_LEN {
        return None;
    }
    let frame_id = u32::from_be_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]);
    let index = u16::from_be_bytes([datagram[4], datagram[5]]);
    let count = u16::from_be_bytes([datagram[6], datagram[7]]);
    let flags = datagram[8];
    if count == 0 || index >= count {
        return None;
    }
    Some((
        VideoHeader {
            frame_id,
            index,
            count,
            flags,
        },
        &datagram[VIDEO_HEADER_LEN..],
    ))
}

/// Reassembles fragments into whole frames.
#[derive(Debug, Default)]
pub struct FrameAssembler {
    current: Option<PartialFrame>,
    completed: u64,
    dropped: u64,
}

#[derive(Debug)]
struct PartialFrame {
    id: u32,
    parts: Vec<Option<Vec<u8>>>,
    flags: u8,
    received: usize,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one fragment. Returns a full frame when it is complete.
    pub fn push(&mut self, header: VideoHeader, payload: &[u8]) -> Option<(u32, bool, Vec<u8>)> {
        if self
            .current
            .as_ref()
            .map_or(true, |f| f.id != header.frame_id)
        {
            if self.current.is_some() {
                self.dropped += 1;
            }
            self.current = Some(PartialFrame {
                id: header.frame_id,
                parts: vec![None; header.count as usize],
                flags: header.flags,
                received: 0,
            });
        }
        let frame = self.current.as_mut()?;
        let slot = frame.parts.get_mut(header.index as usize)?;
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            frame.received += 1;
        }
        if frame.received == frame.parts.len() {
            let done = self.current.take()?;
            let mut data = Vec::with_capacity(
                done.parts
                    .iter()
                    .map(|p| p.as_ref().map_or(0, Vec::len))
                    .sum(),
            );
            for part in done.parts {
                data.extend_from_slice(part.as_deref()?);
            }
            self.completed += 1;
            Some((done.id, done.flags & VIDEO_FLAG_KEYFRAME != 0, data))
        } else {
            None
        }
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_roundtrip() {
        let header = AudioHeader {
            sequence: 42,
            timestamp_us: 123_456,
        };
        let packet = encode_audio_packet(header, b"opus").unwrap();
        let (got, payload) = decode_audio_packet(&packet).unwrap();
        assert_eq!(got, header);
        assert_eq!(payload, b"opus");
    }

    #[test]
    fn video_fragments_reassemble() {
        let payload = vec![7u8; 2500];
        let chunks: Vec<_> = payload.chunks(MAX_VIDEO_FRAGMENT_PAYLOAD).collect();
        let count = chunks.len() as u16;
        let mut asm = FrameAssembler::new();
        let mut result = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let flags = (if i == 0 { VIDEO_FLAG_KEYFRAME } else { 0 })
                | (if i + 1 == chunks.len() {
                    VIDEO_FLAG_LAST
                } else {
                    0
                });
            let header = VideoHeader {
                frame_id: 9,
                index: i as u16,
                count,
                flags,
            };
            let packet = encode_video_fragment(header, chunk).unwrap();
            let (h, p) = decode_video_fragment(&packet).unwrap();
            result = asm.push(h, p);
        }
        let (id, keyframe, data) = result.expect("frame");
        assert_eq!(id, 9);
        assert!(keyframe);
        assert_eq!(data, payload);
        assert_eq!(asm.completed(), 1);
    }
}
