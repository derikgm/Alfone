//! Clipboard synchronization for Alfone.
//!
//! Every clipboard message travels on its own QUIC unidirectional stream, so
//! bulk chunks never block input on the control stream. A transfer starts with
//! an [`ClipboardOffer`], is explicitly requested by the receiver, and is only
//! written to the local clipboard after a `Complete` whose size and SHA-256
//! match the offer. Oversized, stale or malformed transfers are cancelled and
//! forgotten after a short TTL.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alfone_protocol::message::{ClipboardFormat, ClipboardMessage, ClipboardOffer, Message};
use alfone_protocol::codec::LENGTH_PREFIX;
use alfone_protocol::{decode_frame, encode_frame, MAX_FRAME_BYTES};
use anyhow::{anyhow, Context, Result};
use arboard::{Clipboard, ImageData};
use image::codecs::png::PngEncoder;
use image::{ImageBuffer, ImageFormat, RgbaImage};
use quinn::Connection;
use sha2::{Digest, Sha256};
use tokio::time::{MissedTickBehavior, Interval};
use tracing::{debug, info, warn};

/// Hard ceiling for one clipboard payload. It matches the protocol frame cap
/// before chunking, so a single chunk always fits in a control frame too.
pub const MAX_CLIPBOARD_BYTES: usize = 8 * 1024 * 1024;
/// Payload bytes per `ClipboardChunk`.
pub const CHUNK_BYTES: usize = 16 * 1024;
/// How often the system clipboard is inspected for changes.
pub const POLL_INTERVAL: Duration = Duration::from_millis(400);
const TRANSFER_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ClipboardSettings {
    pub enabled: bool,
    pub max_bytes: usize,
    pub chunk_bytes: usize,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: MAX_CLIPBOARD_BYTES,
            chunk_bytes: CHUNK_BYTES,
        }
    }
}

/// Small abstraction so the transfer state machine can be tested without a
/// real desktop clipboard.
pub trait ClipboardBackend {
    fn read(&mut self, max_bytes: usize) -> Result<Option<(ClipboardFormat, Vec<u8>)>>;
    fn write(&mut self, offer: &ClipboardOffer, data: &[u8]) -> Result<()>;
}

/// System clipboard through `arboard`. Text, HTML and PNG images are
/// supported. File drag-and-drop stays in v2, as planned.
pub struct SystemClipboard {
    inner: Clipboard,
}

impl SystemClipboard {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: Clipboard::new().context("opening the system clipboard")?,
        })
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read(&mut self, max_bytes: usize) -> Result<Option<(ClipboardFormat, Vec<u8>)>> {
        if let Ok(text) = self.inner.get_text() {
            if !text.is_empty() {
                return Ok(Some((ClipboardFormat::Text, text.into_bytes())));
            }
        }
        if let Ok(html) = self.inner.get().html() {
            if !html.is_empty() {
                return Ok(Some((ClipboardFormat::Html, html.into_bytes())));
            }
        }
        if let Ok(image) = self.inner.get_image() {
            let png = encode_png(&image)?;
            if png.len() <= max_bytes {
                return Ok(Some((ClipboardFormat::Png, png)));
            }
            warn!(size = png.len(), max_bytes, "clipboard image is too large");
        }
        Ok(None)
    }

    fn write(&mut self, offer: &ClipboardOffer, data: &[u8]) -> Result<()> {
        match offer.format {
            ClipboardFormat::Text => {
                let text = String::from_utf8(data.to_vec()).context("clipboard text is not UTF-8")?;
                self.inner.set_text(text)?;
            }
            ClipboardFormat::Html => {
                let html = String::from_utf8(data.to_vec()).context("clipboard HTML is not UTF-8")?;
                self.inner.set_html(html, None::<String>)?;
            }
            ClipboardFormat::Png => {
                self.inner.set_image(decode_png(data)?)?;
            }
            ClipboardFormat::Rtf | ClipboardFormat::Files => {
                return Err(anyhow!("unsupported clipboard format {:?}", offer.format));
            }
        }
        Ok(())
    }
}

/// Encode raw RGBA pixels from `arboard` as PNG for the wire.
pub fn encode_png(image: &ImageData<'_>) -> Result<Vec<u8>> {
    if image.width == 0 || image.height == 0 {
        return Err(anyhow!("empty clipboard image"));
    }
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow!("clipboard image dimensions overflow"))?;
    if image.bytes.len() != expected {
        return Err(anyhow!(
            "clipboard image has {} bytes, expected {expected}",
            image.bytes.len()
        ));
    }
    let buffer: RgbaImage = ImageBuffer::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.to_vec(),
    )
    .ok_or_else(|| anyhow!("could not build the clipboard image buffer"))?;
    let mut out = Vec::new();
    buffer.write_with_encoder(PngEncoder::new(&mut out))?;
    Ok(out)
}

/// Decode a PNG payload back into raw RGBA pixels.
pub fn decode_png(data: &[u8]) -> Result<ImageData<'static>> {
    let image = image::load_from_memory_with_format(data, ImageFormat::Png)
        .context("decoding clipboard PNG")?
        .to_rgba8();
    Ok(ImageData {
        width: image.width() as usize,
        height: image.height() as usize,
        bytes: image.into_raw().into(),
    })
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

#[derive(Debug, Clone)]
struct Outgoing {
    data: Vec<u8>,
    created: Instant,
}

#[derive(Debug, Clone)]
struct Incoming {
    offer: ClipboardOffer,
    data: Vec<u8>,
    next_offset: u64,
    created: Instant,
}

/// Pure transfer state machine. The CLI drives it from a QUIC clipboard task.
#[derive(Debug)]
pub struct ClipboardAgent {
    settings: ClipboardSettings,
    sequence: u64,
    next_transfer: u64,
    outgoing: HashMap<u64, Outgoing>,
    incoming: HashMap<u64, Incoming>,
    /// Hash of the last local value we offered or wrote remotely. Prevents a
    /// remote paste from bouncing straight back to the sender.
    last_hash: Option<[u8; 32]>,
    /// Hash just written from the network; skipped once on the next poll.
    suppress_hash: Option<[u8; 32]>,
    offered: Option<u64>,
    requested: Option<u64>,
    last_remote_sequence: Option<u64>,
}

impl ClipboardAgent {
    pub fn new(settings: ClipboardSettings) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            ^ ((std::process::id() as u64) << 32);
        Self {
            settings,
            sequence: 0,
            next_transfer: seed | 1,
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            last_hash: None,
            suppress_hash: None,
            offered: None,
            requested: None,
            last_remote_sequence: None,
        }
    }

    /// Live toggle from the local control plane.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.settings.enabled = enabled;
        if !enabled {
            self.outgoing.clear();
            self.incoming.clear();
            self.offered = None;
            self.requested = None;
        }
    }

    fn new_transfer(&mut self) -> u64 {
        self.next_transfer = self.next_transfer.wrapping_add(2).max(1);
        self.next_transfer
    }

    /// Drop transfers that were never requested/completed.
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.outgoing.retain(|id, transfer| {
            let keep = now.duration_since(transfer.created) < TRANSFER_TTL;
            if !keep && self.offered == Some(*id) {
                self.offered = None;
            }
            keep
        });
        self.incoming.retain(|id, transfer| {
            let keep = now.duration_since(transfer.created) < TRANSFER_TTL;
            if !keep && self.requested == Some(*id) {
                self.requested = None;
            }
            keep
        });
    }

    /// Inspect the local clipboard and, if it changed, stage an offer.
    pub fn poll_local(
        &mut self,
        backend: &mut dyn ClipboardBackend,
    ) -> Result<Option<ClipboardOffer>> {
        let suppress = self.suppress_hash.take();
        if !self.settings.enabled || self.offered.is_some() {
            return Ok(None);
        }
        let Some((format, data)) = backend.read(self.settings.max_bytes)? else {
            return Ok(None);
        };
        if data.is_empty() {
            return Ok(None);
        }
        if data.len() > self.settings.max_bytes {
            warn!(size = data.len(), limit = self.settings.max_bytes, "clipboard value skipped: too large");
            return Ok(None);
        }
        let hash = sha256(&data);
        if suppress == Some(hash) || self.last_hash == Some(hash) {
            return Ok(None);
        }

        self.sequence = self.sequence.wrapping_add(1);
        let transfer = self.new_transfer();
        let offer = ClipboardOffer {
            transfer,
            sequence: self.sequence,
            format,
            size: data.len() as u64,
            sha256: hash,
        };
        self.outgoing.insert(
            transfer,
            Outgoing {
                data,
                created: Instant::now(),
            },
        );
        self.last_hash = Some(hash);
        self.offered = Some(transfer);
        info!(transfer, size = offer.size, ?format, "clipboard offer staged");
        Ok(Some(offer))
    }

    /// Handle one peer message; return the messages that must be sent back.
    pub fn handle(
        &mut self,
        message: ClipboardMessage,
        backend: &mut dyn ClipboardBackend,
    ) -> Result<Vec<ClipboardMessage>> {
        let mut reply = Vec::new();
        match message {
            ClipboardMessage::Offer(offer) => {
                if !self.settings.enabled {
                    return Ok(reply);
                }
                if offer.size as usize > self.settings.max_bytes {
                    warn!(transfer = offer.transfer, size = offer.size, "rejecting oversized clipboard offer");
                    reply.push(ClipboardMessage::Cancel {
                        transfer: offer.transfer,
                        reason: "clipboard value exceeds the size limit".into(),
                    });
                    return Ok(reply);
                }
                if self.requested.is_some() {
                    reply.push(ClipboardMessage::Cancel {
                        transfer: offer.transfer,
                        reason: "another clipboard transfer is in progress".into(),
                    });
                    return Ok(reply);
                }
                if self.last_remote_sequence == Some(offer.sequence) {
                    debug!(sequence = offer.sequence, "ignoring duplicate clipboard offer");
                    return Ok(reply);
                }
                self.last_remote_sequence = Some(offer.sequence);
                self.incoming.insert(
                    offer.transfer,
                    Incoming {
                        offer: offer.clone(),
                        data: Vec::with_capacity(offer.size as usize),
                        next_offset: 0,
                        created: Instant::now(),
                    },
                );
                self.requested = Some(offer.transfer);
                info!(transfer = offer.transfer, size = offer.size, "clipboard transfer requested");
                // Requesting the transfer is the receiver's explicit consent.
                reply.push(ClipboardMessage::Request {
                    transfer: offer.transfer,
                });
            }
            ClipboardMessage::Request { transfer } => {
                let Some(outgoing) = self.outgoing.remove(&transfer) else {
                    reply.push(ClipboardMessage::Cancel {
                        transfer,
                        reason: "unknown or expired clipboard transfer".into(),
                    });
                    return Ok(reply);
                };
                if self.offered == Some(transfer) {
                    self.offered = None;
                }
                let chunk_bytes = self.settings.chunk_bytes.max(1);
                let mut offset = 0usize;
                for piece in outgoing.data.chunks(chunk_bytes) {
                    reply.push(ClipboardMessage::Chunk(alfone_protocol::ClipboardChunk {
                        transfer,
                        offset: offset as u64,
                        data: piece.to_vec(),
                    }));
                    offset += piece.len();
                }
                reply.push(ClipboardMessage::Complete { transfer });
                info!(transfer, bytes = offset, "clipboard transfer sent");
            }
            ClipboardMessage::Chunk(chunk) => {
                let Some(incoming) = self.incoming.get_mut(&chunk.transfer) else {
                    reply.push(ClipboardMessage::Cancel {
                        transfer: chunk.transfer,
                        reason: "unknown or expired clipboard transfer".into(),
                    });
                    return Ok(reply);
                };
                let expected_end = chunk
                    .offset
                    .checked_add(chunk.data.len() as u64)
                    .unwrap_or(u64::MAX);
                if chunk.offset != incoming.next_offset || expected_end > incoming.offer.size {
                    reply.push(ClipboardMessage::Cancel {
                        transfer: chunk.transfer,
                        reason: "out-of-order clipboard chunk".into(),
                    });
                    return Ok(reply);
                }
                incoming.data.extend_from_slice(&chunk.data);
                incoming.next_offset = expected_end;
            }
            ClipboardMessage::Complete { transfer } => {
                let Some(incoming) = self.incoming.remove(&transfer) else {
                    reply.push(ClipboardMessage::Cancel {
                        transfer,
                        reason: "unknown or expired clipboard transfer".into(),
                    });
                    return Ok(reply);
                };
                if self.requested == Some(transfer) {
                    self.requested = None;
                }
                if incoming.data.len() as u64 != incoming.offer.size
                    || sha256(&incoming.data) != incoming.offer.sha256
                {
                    warn!(transfer, "clipboard hash mismatch");
                    reply.push(ClipboardMessage::Cancel {
                        transfer,
                        reason: "clipboard hash mismatch".into(),
                    });
                    return Ok(reply);
                }
                match backend.write(&incoming.offer, &incoming.data) {
                    Ok(()) => {
                        self.suppress_hash = Some(incoming.offer.sha256);
                        self.last_hash = Some(incoming.offer.sha256);
                        info!(transfer, "clipboard value applied");
                    }
                    Err(e) => {
                        warn!(transfer, "could not write clipboard: {e:#}");
                        reply.push(ClipboardMessage::Cancel {
                            transfer,
                            reason: e.to_string(),
                        });
                    }
                }
            }
            ClipboardMessage::Cancel { transfer, reason } => {
                self.outgoing.remove(&transfer);
                self.incoming.remove(&transfer);
                if self.offered == Some(transfer) {
                    self.offered = None;
                }
                if self.requested == Some(transfer) {
                    self.requested = None;
                }
                warn!(transfer, %reason, "clipboard transfer cancelled");
            }
        }
        Ok(reply)
    }
}

async fn send(conn: &Connection, message: ClipboardMessage) -> Result<()> {
    let frame = encode_frame(&Message::Clipboard(message))?;
    let mut stream = conn.open_uni().await?;
    stream.write_all(&frame).await?;
    stream.finish()?;
    Ok(())
}

fn ticker() -> Interval {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

/// Run the clipboard side-car for one QUIC session. Aborting this future is
/// enough to clean it up: the connection closes with the control link.
pub async fn run(
    conn: Connection,
    settings: ClipboardSettings,
    enabled: Arc<AtomicBool>,
) -> Result<()> {
    let mut backend = match SystemClipboard::new() {
        Ok(backend) => backend,
        Err(e) => {
            warn!("clipboard unavailable: {e:#}; synchronization disabled");
            return Ok(());
        }
    };
    let mut agent = ClipboardAgent::new(settings);
    let mut poll = ticker();
    info!("clipboard synchronization started");

    loop {
        tokio::select! {
            _ = poll.tick() => {
                agent.set_enabled(enabled.load(Ordering::Relaxed));
                agent.cleanup();
                if let Some(offer) = agent.poll_local(&mut backend)? {
                    send(&conn, ClipboardMessage::Offer(offer)).await?;
                }
            }
            stream = conn.accept_uni() => {
                let mut stream = stream.context("clipboard stream vanished")?;
                let bytes = stream
                    .read_to_end(MAX_FRAME_BYTES + LENGTH_PREFIX + 1)
                    .await?;
                let message = decode_frame(&bytes).context("invalid clipboard frame")?;
                let Message::Clipboard(message) = message else {
                    debug!(kind = message.kind(), "ignoring non-clipboard frame");
                    continue;
                };
                for reply in agent.handle(message, &mut backend)? {
                    send(&conn, reply).await?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alfone_protocol::message::ClipboardChunk;

    #[derive(Default)]
    struct FakeClipboard {
        content: Option<(ClipboardFormat, Vec<u8>)>,
        written: Vec<(ClipboardFormat, Vec<u8>)>,
    }

    impl ClipboardBackend for FakeClipboard {
        fn read(&mut self, _max_bytes: usize) -> Result<Option<(ClipboardFormat, Vec<u8>)>> {
            Ok(self.content.clone())
        }
        fn write(&mut self, offer: &ClipboardOffer, data: &[u8]) -> Result<()> {
            self.written.push((offer.format, data.to_vec()));
            Ok(())
        }
    }

    fn settings() -> ClipboardSettings {
        ClipboardSettings {
            enabled: true,
            max_bytes: 1024,
            chunk_bytes: 4,
        }
    }

    #[test]
    fn offer_request_chunks_and_complete_roundtrip() {
        let value = b"hola mundo".to_vec();
        let mut sender = ClipboardAgent::new(settings());
        let mut source = FakeClipboard {
            content: Some((ClipboardFormat::Text, value.clone())),
            written: Vec::new(),
        };
        let offer = sender.poll_local(&mut source).unwrap().unwrap();
        assert_eq!(offer.size, value.len() as u64);

        // The same value must not be offered twice.
        assert!(sender.poll_local(&mut source).unwrap().is_none());

        let responses = sender
            .handle(
                ClipboardMessage::Request {
                    transfer: offer.transfer,
                },
                &mut source,
            )
            .unwrap();
        assert!(responses.len() > 2);
        assert!(matches!(responses.first(), Some(ClipboardMessage::Chunk(_))));
        assert!(matches!(responses.last(), Some(ClipboardMessage::Complete { .. })));

        let mut receiver = ClipboardAgent::new(settings());
        let mut target = FakeClipboard::default();
        let request = receiver
            .handle(ClipboardMessage::Offer(offer.clone()), &mut target)
            .unwrap();
        assert!(matches!(
            request.first(),
            Some(ClipboardMessage::Request { transfer }) if *transfer == offer.transfer
        ));
        for response in responses {
            assert!(receiver.handle(response, &mut target).unwrap().is_empty());
        }
        assert_eq!(target.written, vec![(ClipboardFormat::Text, value)]);
    }

    #[test]
    fn rejects_oversized_offers_and_local_values() {
        let mut config = settings();
        config.max_bytes = 4;
        let mut agent = ClipboardAgent::new(config.clone());
        let mut backend = FakeClipboard {
            content: Some((ClipboardFormat::Text, b"12345".to_vec())),
            written: Vec::new(),
        };
        assert!(agent.poll_local(&mut backend).unwrap().is_none());

        let offer = ClipboardOffer {
            transfer: 7,
            sequence: 1,
            format: ClipboardFormat::Text,
            size: 5,
            sha256: [0; 32],
        };
        let replies = agent
            .handle(ClipboardMessage::Offer(offer), &mut backend)
            .unwrap();
        assert!(matches!(
            replies.first(),
            Some(ClipboardMessage::Cancel { transfer: 7, .. })
        ));
        assert!(backend.written.is_empty());
    }

    #[test]
    fn hash_mismatch_never_reaches_the_clipboard() {
        let value = b"hola".to_vec();
        let offer = ClipboardOffer {
            transfer: 11,
            sequence: 3,
            format: ClipboardFormat::Text,
            size: value.len() as u64,
            sha256: sha256(&value),
        };
        let mut receiver = ClipboardAgent::new(settings());
        let mut backend = FakeClipboard::default();
        receiver
            .handle(ClipboardMessage::Offer(offer.clone()), &mut backend)
            .unwrap();
        receiver
            .handle(
                ClipboardMessage::Chunk(ClipboardChunk {
                    transfer: offer.transfer,
                    offset: 0,
                    data: b"malo".to_vec(),
                }),
                &mut backend,
            )
            .unwrap();
        let replies = receiver
            .handle(
                ClipboardMessage::Complete {
                    transfer: offer.transfer,
                },
                &mut backend,
            )
            .unwrap();
        assert!(matches!(
            replies.first(),
            Some(ClipboardMessage::Cancel { transfer: 11, .. })
        ));
        assert!(backend.written.is_empty());
    }

    #[test]
    fn png_payload_roundtrips() {
        let original = ImageData {
            width: 2,
            height: 2,
            bytes: [0u8, 0, 0, 255].repeat(4).into(),
        };
        let png = encode_png(&original).unwrap();
        let decoded = decode_png(&png).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 2);
        assert_eq!(decoded.bytes.as_ref(), original.bytes.as_ref());
    }
}
