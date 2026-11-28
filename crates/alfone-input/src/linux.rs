//! Linux/X11 backend using XRecord for capture and XTEST for injection.
//!
//! Both extensions are old, stable and available on every Xorg server. Wayland
//! is intentionally out of scope here: it needs libei + xdg-desktop-portal and
//! different permissions. The trait boundary keeps that work isolated.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::record::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, ButtonPressEvent, ButtonReleaseEvent, ConnectionExt as _, KeyPressEvent, KeyReleaseEvent,
    MotionNotifyEvent,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::x11_utils::TryParse;

use alfone_protocol::{key, InputEvent, KeyAction, KeyEvent, Modifiers, PointerButton};

use crate::{Capture, CaptureHandle, CapturedEvent, Inject, ScreenInfo};

const ISO_LEVEL3_SHIFT: u32 = 0xfe03;
const MODE_SWITCH: u32 = 0xff7e;

pub struct X11Capture {
    screen: ScreenInfo,
    started: bool,
}

impl X11Capture {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context(
            "cannot connect to the X server (is DISPLAY set? Alfone currently needs X11)",
        )?;
        let screen = screen_info(&conn, screen_num);
        if conn
            .extension_information(record::X11_EXTENSION_NAME)
            .context("checking XRecord extension")?
            .is_none()
        {
            bail!("the X server does not provide the RECORD extension");
        }
        Ok(Self {
            screen,
            started: false,
        })
    }
}

impl Capture for X11Capture {
    fn screen(&self) -> ScreenInfo {
        self.screen
    }

    fn start(&mut self) -> Result<CaptureHandle> {
        if self.started {
            bail!("capture already started");
        }
        self.started = true;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let screen = self.screen;
        std::thread::Builder::new()
            .name("alfone-x11-capture".into())
            .spawn(move || {
                if let Err(e) = capture_loop(screen, tx, thread_stop) {
                    tracing::warn!("X11 capture stopped: {e:#}");
                }
            })
            .context("spawning capture thread")?;
        Ok(CaptureHandle::new(rx, stop))
    }
}

fn capture_loop(
    screen: ScreenInfo,
    tx: Sender<CapturedEvent>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // XRecord wants two connections: one for control, one for data.
    let (ctrl, _screen_num) = x11rb::connect(None).context("opening XRecord control connection")?;
    let (data, _) = x11rb::connect(None).context("opening XRecord data connection")?;

    let version = ctrl
        .record_query_version(
            record::X11_XML_VERSION.0 as _,
            record::X11_XML_VERSION.1 as _,
        )?
        .reply()?;
    tracing::debug!(
        "XRecord {}.{}",
        version.major_version,
        version.minor_version
    );

    let keymap = Keymap::load(&ctrl)?;
    let rc = ctrl.generate_id()?;
    let empty = record::Range8 { first: 0, last: 0 };
    let empty_ext = record::ExtRange {
        major: empty,
        minor: record::Range16 { first: 0, last: 0 },
    };
    let range = record::Range {
        core_requests: empty,
        core_replies: empty,
        ext_requests: empty_ext,
        ext_replies: empty_ext,
        delivered_events: empty,
        // Core events 2..=6: KeyPress, KeyRelease, ButtonPress, ButtonRelease,
        // MotionNotify.
        device_events: record::Range8 {
            first: xproto::KEY_PRESS_EVENT,
            last: xproto::MOTION_NOTIFY_EVENT,
        },
        errors: empty,
        client_started: false,
        client_died: false,
    };

    ctrl.record_create_context(rc, 0, &[record::CS::ALL_CLIENTS.into()], &[range])?
        .check()
        .context("creating XRecord context")?;

    tracing::info!(
        "capturing input from X11 ({}x{})",
        screen.width,
        screen.height
    );

    for reply in data.record_enable_context(rc)? {
        let reply = reply.context("reading XRecord reply")?;
        if reply.client_swapped {
            bail!("byte-swapped X clients are not supported");
        }
        parse_reply(&reply.data, &keymap, &tx)?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }

    let _ = ctrl.record_disable_context(rc);
    let _ = ctrl.sync();
    Ok(())
}

fn parse_reply(data: &[u8], keymap: &Keymap, tx: &Sender<CapturedEvent>) -> Result<()> {
    let mut data = data;
    while data.len() >= 32 {
        let kind = data[0] & 0x7f;
        let (event, rest): (Option<CapturedEvent>, &[u8]) = match kind {
            xproto::KEY_PRESS_EVENT => {
                let (ev, rest) = KeyPressEvent::try_parse(data)?;
                (
                    Some(CapturedEvent {
                        event: InputEvent::Key(key_from_x11(
                            keymap,
                            ev.detail,
                            ev.state.into(),
                            KeyAction::Press,
                        )),
                        time_ms: ev.time as u64,
                    }),
                    rest,
                )
            }
            xproto::KEY_RELEASE_EVENT => {
                let (ev, rest) = KeyReleaseEvent::try_parse(data)?;
                (
                    Some(CapturedEvent {
                        event: InputEvent::Key(key_from_x11(
                            keymap,
                            ev.detail,
                            ev.state.into(),
                            KeyAction::Release,
                        )),
                        time_ms: ev.time as u64,
                    }),
                    rest,
                )
            }
            xproto::BUTTON_PRESS_EVENT => {
                let (ev, rest) = ButtonPressEvent::try_parse(data)?;
                let event = match ev.detail {
                    4 => Some(InputEvent::Wheel { dx: 0, dy: 120 }),
                    5 => Some(InputEvent::Wheel { dx: 0, dy: -120 }),
                    6 => Some(InputEvent::Wheel { dx: -120, dy: 0 }),
                    7 => Some(InputEvent::Wheel { dx: 120, dy: 0 }),
                    n => Some(InputEvent::Button {
                        button: PointerButton::from_number(n),
                        pressed: true,
                    }),
                };
                (
                    event.map(|event| CapturedEvent {
                        event,
                        time_ms: ev.time as u64,
                    }),
                    rest,
                )
            }
            xproto::BUTTON_RELEASE_EVENT => {
                let (ev, rest) = ButtonReleaseEvent::try_parse(data)?;
                // Wheel buttons generate press+release; only the press is a
                // scroll tick.
                let event = if (4..=7).contains(&ev.detail) {
                    None
                } else {
                    Some(CapturedEvent {
                        event: InputEvent::Button {
                            button: PointerButton::from_number(ev.detail),
                            pressed: false,
                        },
                        time_ms: ev.time as u64,
                    })
                };
                (event, rest)
            }
            xproto::MOTION_NOTIFY_EVENT => {
                let (ev, rest) = MotionNotifyEvent::try_parse(data)?;
                (
                    Some(CapturedEvent {
                        event: InputEvent::PointerAbs {
                            x: i32::from(ev.root_x),
                            y: i32::from(ev.root_y),
                        },
                        time_ms: ev.time as u64,
                    }),
                    rest,
                )
            }
            _ => (None, &data[32..]),
        };

        if let Some(event) = event {
            if tx.send(event).is_err() {
                return Ok(());
            }
        }
        data = rest;
    }
    Ok(())
}

fn key_from_x11(keymap: &Keymap, detail: u8, state: u16, action: KeyAction) -> KeyEvent {
    let keysym = keymap.resolve(detail, state);
    KeyEvent {
        action,
        keysym,
        hid: 0,
        native: u32::from(detail),
        modifiers: modifiers_from_state(state),
    }
}

fn modifiers_from_state(state: u16) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    if state & 0x0001 != 0 {
        modifiers |= Modifiers::SHIFT;
    }
    if state & 0x0002 != 0 {
        modifiers |= Modifiers::CAPS_LOCK;
    }
    if state & 0x0004 != 0 {
        modifiers |= Modifiers::CONTROL;
    }
    if state & 0x0008 != 0 {
        modifiers |= Modifiers::ALT;
    }
    if state & 0x0010 != 0 {
        modifiers |= Modifiers::NUM_LOCK;
    }
    if state & 0x0040 != 0 {
        modifiers |= Modifiers::SUPER;
    }
    modifiers
}

fn screen_info(conn: &RustConnection, screen_num: usize) -> ScreenInfo {
    let root = &conn.setup().roots[screen_num];
    ScreenInfo {
        width: u32::from(root.width_in_pixels),
        height: u32::from(root.height_in_pixels),
    }
}

#[derive(Clone)]
struct Keymap {
    min_keycode: u8,
    per_keycode: usize,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn load(conn: &RustConnection) -> Result<Self> {
        let setup = conn.setup();
        let min_keycode = setup.min_keycode;
        let count = setup.max_keycode - setup.min_keycode + 1;
        let reply = conn
            .get_keyboard_mapping(min_keycode, count)?
            .reply()
            .context("reading keyboard mapping")?;
        Ok(Self {
            min_keycode,
            per_keycode: usize::from(reply.keysyms_per_keycode),
            keysyms: reply.keysyms,
        })
    }

    fn raw(&self, keycode: u8, level: usize) -> Option<u32> {
        if self.per_keycode == 0 || keycode < self.min_keycode {
            return None;
        }
        let base = usize::from(keycode - self.min_keycode) * self.per_keycode;
        self.keysyms.get(base + level).copied()
    }

    fn resolve(&self, keycode: u8, state: u16) -> u32 {
        let shift = state & 0x0001 != 0;
        let caps = state & 0x0002 != 0;
        let altgr = state & 0x0080 != 0;
        let base = self.raw(keycode, 0).unwrap_or(0);

        let level = if altgr {
            if shift {
                3
            } else {
                2
            }
        } else if shift {
            1
        } else if caps && (0x61..=0x7a).contains(&base) {
            // Caps lock uppercases letters, not digits or punctuation.
            1
        } else {
            0
        };

        self.raw(keycode, level)
            .filter(|k| *k != 0)
            .or_else(|| self.raw(keycode, 0))
            .filter(|k| *k != 0)
            .unwrap_or(base)
    }

    fn reverse(&self) -> HashMap<u32, KeyTarget> {
        let mut map = HashMap::new();
        for keycode in self.min_keycode
            ..=self.min_keycode.saturating_add(
                (self.keysyms.len() / self.per_keycode.max(1)).saturating_sub(1) as u8,
            )
        {
            for level in 0..self.per_keycode {
                if let Some(keysym) = self.raw(keycode, level).filter(|k| *k != 0) {
                    map.entry(keysym).or_insert(KeyTarget {
                        keycode,
                        level: level as u8,
                    });
                }
            }
        }
        map
    }
}

#[derive(Debug, Clone, Copy)]
struct KeyTarget {
    keycode: u8,
    level: u8,
}

pub struct X11Inject {
    conn: RustConnection,
    screen_num: usize,
    screen: ScreenInfo,
    target: HashMap<u32, KeyTarget>,
    held: HashMap<u8, AutoModifiers>,
}

#[derive(Debug, Clone, Copy, Default)]
struct AutoModifiers {
    shift: bool,
    altgr: bool,
}

impl X11Inject {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context(
            "cannot connect to the X server (is DISPLAY set? Alfone currently needs X11)",
        )?;
        let screen = screen_info(&conn, screen_num);
        let keymap = Keymap::load(&conn)?;
        let target = keymap.reverse();
        Ok(Self {
            conn,
            screen_num,
            screen,
            target,
            held: HashMap::new(),
        })
    }

    fn root(&self) -> xproto::Window {
        self.conn.setup().roots[self.screen_num].root
    }

    fn fake_key(&self, keycode: u8, press: bool) -> Result<()> {
        let kind = if press {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        self.conn
            .xtest_fake_input(kind, keycode, 0, self.root(), 0, 0, 0)?
            .check()?;
        Ok(())
    }

    fn fake_button(&self, button: u8, press: bool) -> Result<()> {
        let kind = if press {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        self.conn
            .xtest_fake_input(kind, button, 0, self.root(), 0, 0, 0)?
            .check()?;
        Ok(())
    }

    fn fake_motion(&self, x: i32, y: i32) -> Result<()> {
        let x = x.clamp(
            0,
            i32::try_from(self.screen.width.saturating_sub(1)).unwrap_or(i32::MAX),
        );
        let y = y.clamp(
            0,
            i32::try_from(self.screen.height.saturating_sub(1)).unwrap_or(i32::MAX),
        );
        self.conn
            .xtest_fake_input(
                xproto::MOTION_NOTIFY_EVENT,
                0,
                0,
                self.root(),
                i16::try_from(x).unwrap_or(0),
                i16::try_from(y).unwrap_or(0),
                0,
            )?
            .check()?;
        Ok(())
    }

    fn modifier_keycode(&self, keysym: u32) -> Option<u8> {
        self.target.get(&keysym).map(|t| t.keycode)
    }

    fn altgr_keycode(&self) -> Option<u8> {
        self.modifier_keycode(ISO_LEVEL3_SHIFT)
            .or_else(|| self.modifier_keycode(MODE_SWITCH))
    }

    fn key(&mut self, event: &KeyEvent) -> Result<()> {
        let Some(target) = self.target.get(&event.keysym).copied() else {
            tracing::debug!(keysym = event.keysym, "no local keycode for keysym");
            return Ok(());
        };

        // Modifier keys are forwarded as-is.
        if key::is_modifier(event.keysym) {
            match event.action {
                KeyAction::Press | KeyAction::Repeat => {
                    if !self.held.contains_key(&target.keycode) {
                        self.fake_key(target.keycode, true)?;
                        self.held.insert(target.keycode, AutoModifiers::default());
                    }
                }
                KeyAction::Release => {
                    if self.held.remove(&target.keycode).is_some() {
                        self.fake_key(target.keycode, false)?;
                    }
                }
            }
            return Ok(());
        }

        let needs_shift = target.level == 1 || target.level == 3;
        let needs_altgr = target.level == 2 || target.level == 3;
        let shift_code = self.modifier_keycode(key::SHIFT_L);
        let altgr_code = self.altgr_keycode();

        let mut auto = AutoModifiers::default();

        if event.action == KeyAction::Release {
            if let Some(auto) = self.held.remove(&target.keycode) {
                self.fake_key(target.keycode, false)?;
                if auto.shift {
                    if let Some(code) = shift_code {
                        self.fake_key(code, false)?;
                    }
                }
                if auto.altgr {
                    if let Some(code) = altgr_code {
                        self.fake_key(code, false)?;
                    }
                }
            }
            return Ok(());
        }

        if needs_shift {
            if let Some(code) = shift_code {
                if !self.held.contains_key(&code) {
                    self.fake_key(code, true)?;
                    auto.shift = true;
                }
            }
        }
        if needs_altgr {
            if let Some(code) = altgr_code {
                if !self.held.contains_key(&code) {
                    self.fake_key(code, true)?;
                    auto.altgr = true;
                }
            }
        }

        self.fake_key(target.keycode, true)?;
        self.held.insert(target.keycode, auto);

        // X11 auto-repeat will handle held keys, but for explicit repeat events
        // (and to make taps deterministic) send the release immediately.
        if event.action == KeyAction::Repeat {
            self.fake_key(target.keycode, false)?;
            self.held.remove(&target.keycode);
            if auto.shift {
                if let Some(code) = shift_code {
                    self.fake_key(code, false)?;
                }
            }
            if auto.altgr {
                if let Some(code) = altgr_code {
                    self.fake_key(code, false)?;
                }
            }
        }
        Ok(())
    }
}

impl Inject for X11Inject {
    fn screen(&self) -> ScreenInfo {
        self.screen
    }

    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::Key(event) => self.key(event),
            InputEvent::PointerAbs { x, y } => self.fake_motion(*x, *y),
            InputEvent::PointerRel { dx, dy } => {
                let (x, y) = self.pointer()?;
                self.fake_motion(x + dx, y + dy)
            }
            InputEvent::Button { button, pressed } => self.fake_button(button.number(), *pressed),
            InputEvent::Wheel { dx, dy } => {
                let vertical = dy / 120;
                let horizontal = dx / 120;
                for _ in 0..vertical.unsigned_abs() {
                    let button = if vertical > 0 { 4 } else { 5 };
                    self.fake_button(button, true)?;
                    self.fake_button(button, false)?;
                }
                for _ in 0..horizontal.unsigned_abs() {
                    let button = if horizontal > 0 { 7 } else { 6 };
                    self.fake_button(button, true)?;
                    self.fake_button(button, false)?;
                }
                Ok(())
            }
        }
    }

    fn pointer(&mut self) -> Result<(i32, i32)> {
        let reply = self.conn.query_pointer(self.root())?.reply()?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    fn warp(&mut self, x: i32, y: i32) -> Result<()> {
        let root = self.root();
        self.conn
            .warp_pointer(
                x11rb::NONE,
                root,
                0,
                0,
                0,
                0,
                i16::try_from(x).unwrap_or(0),
                i16::try_from(y).unwrap_or(0),
            )?
            .check()?;
        Ok(())
    }

    fn set_remote_mode(&mut self, remote: bool) -> Result<()> {
        let root = self.root();
        // The pointer is warped to the center while remote so XRecord keeps
        // delivering relative motion. Hide it so that internal warp is invisible.
        let cursor = if remote {
            x11rb::protocol::xfixes::hide_cursor(&self.conn, root)
        } else {
            x11rb::protocol::xfixes::show_cursor(&self.conn, root)
        };
        if let Err(e) = cursor?.check() {
            tracing::warn!(?e, "could not toggle the local cursor visibility");
        }
        if remote {
            let mask = xproto::EventMask::BUTTON_PRESS
                | xproto::EventMask::BUTTON_RELEASE
                | xproto::EventMask::POINTER_MOTION;
            if let Err(e) = self
                .conn
                .grab_keyboard(
                    false,
                    root,
                    x11rb::CURRENT_TIME,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )?
                .reply()
            {
                tracing::warn!("could not grab keyboard: {e}");
            }
            if let Err(e) = self
                .conn
                .grab_pointer(
                    false,
                    root,
                    mask,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )?
                .reply()
            {
                tracing::warn!("could not grab pointer: {e}");
            }
        } else {
            let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME)?.check();
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME)?.check();
        }
        Ok(())
    }
}
