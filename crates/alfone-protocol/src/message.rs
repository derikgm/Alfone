use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::key::Keysym;

/// Operating system / display server a peer runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    Windows,
    MacOs,
    LinuxX11,
    LinuxWayland,
    FreeBsd,
    Other(String),
}

impl Os {
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(all(target_os = "linux", not(feature = "wayland")))]
        {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                Self::LinuxWayland
            } else {
                Self::LinuxX11
            }
        }
        #[cfg(all(target_os = "linux", feature = "wayland"))]
        {
            Self::LinuxWayland
        }
        #[cfg(target_os = "freebsd")]
        {
            Self::FreeBsd
        }
        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd"
        )))]
        {
            Self::Other(std::env::consts::OS.to_string())
        }
    }

    pub fn is_wayland(&self) -> bool {
        matches!(self, Self::LinuxWayland)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Stable random id generated on first run (hex).
    pub id: String,
    pub name: String,
    pub os: Os,
    pub app_version: String,
}

impl DeviceInfo {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            os: Os::current(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub input: bool,
    pub clipboard: bool,
    pub audio: bool,
    pub video: bool,
    pub file_transfer: bool,
}

impl Capabilities {
    pub fn input_only() -> Self {
        Self {
            input: true,
            ..Self::default()
        }
    }

    pub fn full() -> Self {
        Self {
            input: true,
            clipboard: true,
            audio: true,
            video: true,
            file_transfer: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Role {
    /// Owns the keyboard/mouse and the screen layout.
    #[default]
    Server,
    /// Receives input and can send its display/audio.
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionMode {
    /// Independent desktops; the cursor crosses edges.
    #[default]
    Kvm,
    /// A display of the server is mirrored to the client.
    Mirror,
    /// The client acts as an extra monitor (requires a virtual display).
    Extend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub device: DeviceInfo,
    pub capabilities: Capabilities,
    pub mode: SessionMode,
    /// True when the server expects a `PairRequest` before accepting input.
    pub pairing_required: bool,
    /// Random bytes so both sides can derive a short authentication string.
    pub nonce: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloBack {
    pub protocol: u16,
    pub device: DeviceInfo,
    pub capabilities: Capabilities,
    pub mode: SessionMode,
    pub nonce: [u8; 16],
    /// Fingerprint the client verified for the server certificate.
    pub server_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// Short code shown on both screens (e.g. `ABCD-EFGH`).
    pub code: String,
    pub device: DeviceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    pub accepted: bool,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Modifiers: u16 {
        const SHIFT     = 1 << 0;
        const CONTROL   = 1 << 1;
        const ALT       = 1 << 2;
        const META      = 1 << 3;
        const SUPER     = 1 << 4;
        const CAPS_LOCK = 1 << 5;
        const NUM_LOCK  = 1 << 6;
        const SCROLL    = 1 << 7;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAction {
    Press,
    Release,
    Repeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
    Other(u8),
}

impl PointerButton {
    pub fn number(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
            Self::Back => 8,
            Self::Forward => 9,
            Self::Other(n) => n,
        }
    }

    pub fn from_number(n: u8) -> Self {
        match n {
            1 => Self::Left,
            2 => Self::Middle,
            3 => Self::Right,
            8 => Self::Back,
            9 => Self::Forward,
            other => Self::Other(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub action: KeyAction,
    /// Logical key.
    pub keysym: Keysym,
    /// USB HID usage (page 0x07), or 0 when unknown.
    pub hid: u16,
    /// Sender-native scancode, useful for diagnostics and for layouts where
    /// the keysym is ambiguous.
    pub native: u32,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute position in the **target screen's** coordinate space.
    PointerAbs {
        x: i32,
        y: i32,
    },
    /// Relative motion, used by games and by clients that prefer raw deltas.
    PointerRel {
        dx: i32,
        dy: i32,
    },
    Button {
        button: PointerButton,
        pressed: bool,
    },
    Wheel {
        /// Horizontal, +120 per notch to the right.
        dx: i32,
        /// Vertical, +120 per notch up/away from the user.
        dy: i32,
    },
    Key(KeyEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterScreen {
    /// Position inside the target screen, in target pixels.
    pub x: i32,
    pub y: i32,
    /// Orders enter/leave against input events.
    pub sequence: u64,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveScreen {
    pub sequence: u64,
}

// ---------------------------------------------------------------------------
// Display / layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// 1000 = 100 %. Integer to keep the protocol `Eq`.
    pub scale_milli: u32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub displays: Vec<Display>,
    pub primary: u32,
}

impl DisplayInfo {
    pub fn single(width: u32, height: u32) -> Self {
        Self {
            displays: vec![Display {
                id: 0,
                name: "default".into(),
                x: 0,
                y: 0,
                width,
                height,
                scale_milli: 1000,
                primary: true,
            }],
            primary: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenDescriptor {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub screens: Vec<ScreenDescriptor>,
    pub primary: String,
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardFormat {
    Text,
    Html,
    Rtf,
    Png,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardOffer {
    pub transfer: u64,
    /// Clipboard sequence number, used to discard stale transfers.
    pub sequence: u64,
    pub format: ClipboardFormat,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardChunk {
    pub transfer: u64,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardMessage {
    Offer(ClipboardOffer),
    Request { transfer: u64 },
    Chunk(ClipboardChunk),
    Complete { transfer: u64 },
    Cancel { transfer: u64, reason: String },
}

// ---------------------------------------------------------------------------
// Media control (payloads live in `media`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u16,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            frame_ms: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioControl {
    Start(AudioFormat),
    Stop,
    Mute(bool),
    Volume(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    Jpeg,
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFormat {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u16,
    pub bitrate_kbps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoControl {
    Start(VideoFormat),
    Stop,
    RequestKeyframe,
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    HelloBack(HelloBack),
    PairRequest(PairRequest),
    PairResponse(PairResponse),
    Ping { id: u64 },
    Pong { id: u64 },
    Disconnect { reason: String },
    Error { code: u16, message: String },
    Displays(DisplayInfo),
    Layout(LayoutSnapshot),
    EnterScreen(EnterScreen),
    LeaveScreen(LeaveScreen),
    Input(InputEvent),
    Clipboard(ClipboardMessage),
    Audio(AudioControl),
    Video(VideoControl),
    Mode(SessionMode),
}

impl Message {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::HelloBack(_) => "hello-back",
            Self::PairRequest(_) => "pair-request",
            Self::PairResponse(_) => "pair-response",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::Disconnect { .. } => "disconnect",
            Self::Error { .. } => "error",
            Self::Displays(_) => "displays",
            Self::Layout(_) => "layout",
            Self::EnterScreen(_) => "enter-screen",
            Self::LeaveScreen(_) => "leave-screen",
            Self::Input(_) => "input",
            Self::Clipboard(_) => "clipboard",
            Self::Audio(_) => "audio",
            Self::Video(_) => "video",
            Self::Mode(_) => "mode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) {
        let bytes = postcard::to_stdvec(&msg).expect("encode");
        let back: Message = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(msg, back);
    }

    #[test]
    fn every_message_roundtrips() {
        let device = DeviceInfo::new("abc", "desktop");
        roundtrip(Message::Hello(Hello {
            protocol: crate::PROTOCOL_VERSION,
            device: device.clone(),
            capabilities: Capabilities::full(),
            mode: SessionMode::Kvm,
            pairing_required: true,
            nonce: [7; 16],
        }));
        roundtrip(Message::HelloBack(HelloBack {
            protocol: crate::PROTOCOL_VERSION,
            device,
            capabilities: Capabilities::input_only(),
            mode: SessionMode::Kvm,
            nonce: [1; 16],
            server_fingerprint: [9; 32],
        }));
        roundtrip(Message::Input(InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            keysym: crate::key::from_char('ñ'),
            hid: 0x2f,
            native: 42,
            modifiers: Modifiers::SHIFT | Modifiers::CONTROL,
        })));
        roundtrip(Message::Input(InputEvent::PointerAbs { x: -20, y: 1080 }));
        roundtrip(Message::Input(InputEvent::Button {
            button: PointerButton::Right,
            pressed: true,
        }));
        roundtrip(Message::Input(InputEvent::Wheel { dx: 0, dy: -120 }));
        roundtrip(Message::Clipboard(ClipboardMessage::Offer(
            ClipboardOffer {
                transfer: 3,
                sequence: 4,
                format: ClipboardFormat::Png,
                size: 1234,
                sha256: [3; 32],
            },
        )));
        roundtrip(Message::Displays(DisplayInfo::single(1920, 1080)));
        roundtrip(Message::Audio(AudioControl::Start(AudioFormat::default())));
    }

    #[test]
    fn message_is_compact() {
        let msg = Message::Input(InputEvent::PointerRel { dx: 1, dy: -1 });
        let bytes = postcard::to_stdvec(&msg).unwrap();
        assert!(bytes.len() <= 8, "unexpectedly large: {bytes:?}");
    }
}
