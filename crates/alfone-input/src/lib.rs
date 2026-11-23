//! Platform input capture and injection.
//!
//! Backends: Linux/X11 ([`linux`], XRecord + XTEST) and Windows ([`windows`],
//! low-level hooks + `SendInput`). The daemon never talks to X11/Windows
//! directly: it uses the traits in this crate. That keeps the routing logic
//! testable with [`mock`] and lets us add a new backend (Wayland/libei, macOS,
//! for example) without touching the network or the UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use alfone_protocol::InputEvent;

pub mod mock;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct CapturedEvent {
    pub event: InputEvent,
    /// Monotonic timestamp in milliseconds, for latency measurements.
    pub time_ms: u64,
}

/// Threaded capture. The backend runs on its own thread and exposes a channel.
pub trait Capture: Send {
    fn screen(&self) -> ScreenInfo;
    fn start(&mut self) -> anyhow::Result<CaptureHandle>;
}

pub struct CaptureHandle {
    pub events: Receiver<CapturedEvent>,
    stop: Arc<AtomicBool>,
}

impl CaptureHandle {
    pub fn new(events: Receiver<CapturedEvent>, stop: Arc<AtomicBool>) -> Self {
        Self { events, stop }
    }

    /// Best effort: the thread exits after the next event. Capture threads are
    /// detached because some platform APIs block until an event arrives.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Event injection. All coordinates are in this machine's screen space.
pub trait Inject: Send {
    fn screen(&self) -> ScreenInfo;
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()>;
    fn pointer(&mut self) -> anyhow::Result<(i32, i32)>;
    fn warp(&mut self, x: i32, y: i32) -> anyhow::Result<()>;

    /// Grab input so local applications do not also receive events while the
    /// pointer is on a remote screen. Backends that cannot grab can no-op.
    fn set_remote_mode(&mut self, remote: bool) -> anyhow::Result<()> {
        let _ = remote;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub fn create_capture() -> anyhow::Result<Box<dyn Capture>> {
    Ok(Box::new(linux::X11Capture::new()?))
}

#[cfg(target_os = "linux")]
pub fn create_inject() -> anyhow::Result<Box<dyn Inject>> {
    Ok(Box::new(linux::X11Inject::new()?))
}

#[cfg(target_os = "windows")]
pub fn create_capture() -> anyhow::Result<Box<dyn Capture>> {
    Ok(Box::new(windows::WinCapture::new()?))
}

#[cfg(target_os = "windows")]
pub fn create_inject() -> anyhow::Result<Box<dyn Inject>> {
    Ok(Box::new(windows::WinInject::new()?))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn create_capture() -> anyhow::Result<Box<dyn Capture>> {
    unsupported::create_capture()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn create_inject() -> anyhow::Result<Box<dyn Inject>> {
    unsupported::create_inject()
}
