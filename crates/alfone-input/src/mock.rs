//! In-memory backend used by tests and by `alfone doctor`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self};
use std::sync::{Arc, Mutex};

use alfone_protocol::InputEvent;

use crate::{Capture, CaptureHandle, CapturedEvent, Inject, ScreenInfo};

pub struct MockCapture {
    screen: ScreenInfo,
    script: Vec<CapturedEvent>,
}

impl MockCapture {
    pub fn new(screen: ScreenInfo, script: Vec<CapturedEvent>) -> Self {
        Self { screen, script }
    }
}

impl Capture for MockCapture {
    fn screen(&self) -> ScreenInfo {
        self.screen
    }

    fn start(&mut self) -> anyhow::Result<CaptureHandle> {
        let (tx, rx) = mpsc::channel();
        let events = std::mem::take(&mut self.script);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::spawn(move || {
            for event in events {
                if thread_stop.load(Ordering::Relaxed) || tx.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(CaptureHandle::new(rx, stop))
    }
}

pub struct MockInject {
    screen: ScreenInfo,
    pub events: Arc<Mutex<Vec<InputEvent>>>,
    pointer: (i32, i32),
}

impl MockInject {
    pub fn new(screen: ScreenInfo) -> Self {
        Self {
            screen,
            events: Arc::new(Mutex::new(Vec::new())),
            pointer: (0, 0),
        }
    }
}

impl Inject for MockInject {
    fn screen(&self) -> ScreenInfo {
        self.screen
    }

    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        if let InputEvent::PointerAbs { x, y } = event {
            self.pointer = (*x, *y);
        }
        self.events.lock().unwrap().push(*event);
        Ok(())
    }

    fn pointer(&mut self) -> anyhow::Result<(i32, i32)> {
        Ok(self.pointer)
    }

    fn warp(&mut self, x: i32, y: i32) -> anyhow::Result<()> {
        self.pointer = (x, y);
        Ok(())
    }

    fn set_remote_mode(&mut self, _remote: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_capture_emits_script() {
        let mut capture = MockCapture::new(
            ScreenInfo {
                width: 1920,
                height: 1080,
            },
            vec![CapturedEvent {
                event: InputEvent::PointerAbs { x: 3, y: 4 },
                time_ms: 0,
            }],
        );
        let handle = capture.start().unwrap();
        let event = handle.events.recv().unwrap();
        assert_eq!(event.event, InputEvent::PointerAbs { x: 3, y: 4 });
    }

    #[test]
    fn mock_inject_tracks_pointer() {
        let mut inject = MockInject::new(ScreenInfo {
            width: 100,
            height: 100,
        });
        inject
            .inject(&InputEvent::PointerAbs { x: 10, y: 20 })
            .unwrap();
        assert_eq!(inject.pointer().unwrap(), (10, 20));
        assert_eq!(inject.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn capture_handle_exposes_receiver() {
        let (_tx, rx) = mpsc::channel::<CapturedEvent>();
        let handle: CaptureHandle = CaptureHandle::new(rx, Arc::new(AtomicBool::new(false)));
        assert!(handle.events.try_recv().is_err());
    }
}
