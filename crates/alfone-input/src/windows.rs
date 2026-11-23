//! Windows backend: low-level hooks for capture and `SendInput` injection.
//!
//! Capture installs `WH_KEYBOARD_LL`/`WH_MOUSE_LL` hooks through `rdev::grab`
//! in a dedicated thread. Using *grab* (instead of plain `listen`) is what lets
//! [`WinInject::set_remote_mode`] block local applications from seeing the
//! input while the pointer is on the remote screen: the hook forwards every
//! event to the daemon and, when remote mode is on, swallows it before it
//! reaches the window below the cursor.
//!
//! Injection uses `enigo` (`SendInput` with virtual keys). The server sends
//! X11 keysyms, so [`WinInject`] maps keysym → virtual key and presses the
//! required Shift itself (same `AutoModifiers` pattern as the X11 backend).
//! Characters that have no virtual key (accents, ñ, emoji…) fall back to
//! enigo's unicode text entry, which works regardless of the keyboard layout.

use anyhow::{anyhow, bail, Context, Result};
use enigo::{
    Axis, Button as EnigoButton, Coordinate, Direction, Enigo, Key as EKey, Keyboard, Mouse,
    Settings,
};
use rdev::{grab, Button as RButton, Event, EventType, Key as RKey};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use alfone_protocol::{key, InputEvent, KeyAction, KeyEvent, Modifiers, PointerButton};

use crate::{Capture, CaptureHandle, CapturedEvent, Inject, ScreenInfo};

/// Process-wide remote mode. `WinCapture`'s hook reads it to swallow local
/// input while the pointer is on a remote screen, and `WinInject` sets it from
/// [`Inject::set_remote_mode`]. Capture and injection are separate objects, so
/// a global is the simplest way to connect them inside the daemon process.
static REMOTE_MODE: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Shared key <-> keysym tables
// ---------------------------------------------------------------------------

fn keysym_from_rkey(k: RKey) -> Option<u32> {
    let keysym = match k {
        RKey::AltGr => key::ALT_R,
        RKey::Alt => key::ALT_L,
        RKey::Backspace => key::BACKSPACE,
        RKey::CapsLock => key::CAPS_LOCK,
        RKey::ControlLeft => key::CONTROL_L,
        RKey::ControlRight => key::CONTROL_R,
        RKey::Delete => key::DELETE,
        RKey::DownArrow => key::DOWN,
        RKey::End => key::END,
        RKey::Escape => key::ESCAPE,
        RKey::F1 => key::F1,
        RKey::F2 => key::F2,
        RKey::F3 => key::F3,
        RKey::F4 => key::F4,
        RKey::F5 => key::F5,
        RKey::F6 => key::F6,
        RKey::F7 => key::F7,
        RKey::F8 => key::F8,
        RKey::F9 => key::F9,
        RKey::F10 => key::F10,
        RKey::F11 => key::F11,
        RKey::F12 => key::F12,
        RKey::Home => key::HOME,
        RKey::LeftArrow => key::LEFT,
        RKey::MetaLeft => key::SUPER_L,
        RKey::MetaRight => key::SUPER_R,
        RKey::PageDown => key::PAGE_DOWN,
        RKey::PageUp => key::PAGE_UP,
        RKey::Return => key::RETURN,
        RKey::RightArrow => key::RIGHT,
        RKey::ShiftLeft => key::SHIFT_L,
        RKey::ShiftRight => key::SHIFT_R,
        RKey::Space => key::SPACE,
        RKey::Tab => key::TAB,
        RKey::UpArrow => key::UP,
        RKey::PrintScreen => key::PRINT,
        RKey::ScrollLock => key::SCROLL_LOCK,
        RKey::Pause => key::PAUSE,
        RKey::NumLock => key::NUM_LOCK,
        RKey::BackQuote => key::GRAVE,
        RKey::Minus => key::MINUS,
        RKey::Equal => key::EQUAL,
        RKey::Comma => key::COMMA,
        RKey::Dot => key::PERIOD,
        RKey::Slash => key::SLASH,
        RKey::SemiColon => key::SEMICOLON,
        RKey::Quote => key::APOSTROPHE,
        RKey::LeftBracket => key::BRACKET_LEFT,
        RKey::RightBracket => key::BRACKET_RIGHT,
        RKey::BackSlash | RKey::IntlBackslash => key::BACKSLASH,
        RKey::KpReturn => key::KP_ENTER,
        RKey::KpMinus => key::KP_SUBTRACT,
        RKey::KpPlus => key::KP_ADD,
        RKey::KpMultiply => key::KP_MULTIPLY,
        RKey::KpDivide => key::KP_DIVIDE,
        RKey::Kp0 => key::KP_0,
        RKey::Kp1 => key::KP_1,
        RKey::Kp2 => key::KP_2,
        RKey::Kp3 => key::KP_3,
        RKey::Kp4 => key::KP_4,
        RKey::Kp5 => key::KP_5,
        RKey::Kp6 => key::KP_6,
        RKey::Kp7 => key::KP_7,
        RKey::Kp8 => key::KP_8,
        RKey::Kp9 => key::KP_9,
        RKey::KpDelete => key::KP_DECIMAL,
        // `Function` and `Unknown(_)` have no stable keysym.
        _ => return None,
    };
    Some(keysym)
}

fn rkey_digit(k: RKey) -> Option<char> {
    let digit = match k {
        RKey::Num0 => '0',
        RKey::Num1 => '1',
        RKey::Num2 => '2',
        RKey::Num3 => '3',
        RKey::Num4 => '4',
        RKey::Num5 => '5',
        RKey::Num6 => '6',
        RKey::Num7 => '7',
        RKey::Num8 => '8',
        RKey::Num9 => '9',
        _ => return None,
    };
    Some(digit)
}

fn rkey_letter(k: RKey) -> Option<char> {
    let letter = match k {
        RKey::KeyA => 'a',
        RKey::KeyB => 'b',
        RKey::KeyC => 'c',
        RKey::KeyD => 'd',
        RKey::KeyE => 'e',
        RKey::KeyF => 'f',
        RKey::KeyG => 'g',
        RKey::KeyH => 'h',
        RKey::KeyI => 'i',
        RKey::KeyJ => 'j',
        RKey::KeyK => 'k',
        RKey::KeyL => 'l',
        RKey::KeyM => 'm',
        RKey::KeyN => 'n',
        RKey::KeyO => 'o',
        RKey::KeyP => 'p',
        RKey::KeyQ => 'q',
        RKey::KeyR => 'r',
        RKey::KeyS => 's',
        RKey::KeyT => 't',
        RKey::KeyU => 'u',
        RKey::KeyV => 'v',
        RKey::KeyW => 'w',
        RKey::KeyX => 'x',
        RKey::KeyY => 'y',
        RKey::KeyZ => 'z',
        _ => return None,
    };
    Some(letter)
}

fn enigo_button(button: PointerButton) -> Option<EnigoButton> {
    let button = match button {
        PointerButton::Left => EnigoButton::Left,
        PointerButton::Middle => EnigoButton::Middle,
        PointerButton::Right => EnigoButton::Right,
        PointerButton::Back => EnigoButton::Back,
        PointerButton::Forward => EnigoButton::Forward,
        PointerButton::Other(_) => return None,
    };
    Some(button)
}

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// Mutable state of the capture hook, guarded by a mutex because `rdev::grab`
/// requires an `Fn` callback.
struct HookState {
    tx: Sender<CapturedEvent>,
    mods: Modifiers,
    /// Last typed character per physical key, from the OS layout (`Event.name`).
    chars: HashMap<RKey, char>,
}

impl HookState {
    /// Maps an rdev event to a protocol event. `None` means "not interesting".
    fn convert(&mut self, event: &Event) -> Option<CapturedEvent> {
        let (action, rkey) = match event.event_type {
            EventType::KeyPress(k) => (KeyAction::Press, k),
            EventType::KeyRelease(k) => (KeyAction::Release, k),
            EventType::MouseMove { x, y } => {
                return Some(CapturedEvent {
                    event: InputEvent::PointerAbs {
                        x: clamp_i32(x),
                        y: clamp_i32(y),
                    },
                    time_ms: 0,
                });
            }
            EventType::ButtonPress(b) => {
                return Some(CapturedEvent {
                    event: InputEvent::Button {
                        button: button_from_rbutton(b),
                        pressed: true,
                    },
                    time_ms: 0,
                });
            }
            EventType::ButtonRelease(b) => {
                return Some(CapturedEvent {
                    event: InputEvent::Button {
                        button: button_from_rbutton(b),
                        pressed: false,
                    },
                    time_ms: 0,
                });
            }
            // rdev already divides by WHEEL_DELTA: positive = up/right.
            EventType::Wheel { delta_x, delta_y } => {
                return Some(CapturedEvent {
                    event: InputEvent::Wheel {
                        dx: i32::try_from(delta_x).unwrap_or(0).saturating_mul(120),
                        dy: i32::try_from(delta_y).unwrap_or(0).saturating_mul(120),
                    },
                    time_ms: 0,
                });
            }
        };

        // `Event.name` is the character the OS would type for this key press,
        // shift and layout included. That gives us the same "level" behaviour
        // the X11 backend gets from its keymap.
        if action == KeyAction::Press {
            if let Some(c) = name_char(event) {
                self.chars.insert(rkey, c);
            }
        }
        let keysym = if let Some(c) = self.chars.get(&rkey).copied() {
            key::from_char(c)
        } else if let Some(keysym) = keysym_from_rkey(rkey) {
            keysym
        } else if let Some(c) = rkey_letter(rkey).or_else(|| rkey_digit(rkey)) {
            // Fallback when the OS gives no character for the key press.
            key::from_char(c)
        } else {
            0
        };

        self.update_modifiers(keysym, action);
        Some(CapturedEvent {
            event: InputEvent::Key(KeyEvent {
                action,
                keysym,
                hid: 0,
                native: 0,
                modifiers: self.mods,
            }),
            time_ms: 0,
        })
    }

    fn update_modifiers(&mut self, keysym: u32, action: KeyAction) {
        let bit = match keysym {
            key::SHIFT_L | key::SHIFT_R => Modifiers::SHIFT,
            key::CONTROL_L | key::CONTROL_R => Modifiers::CONTROL,
            key::ALT_L | key::ALT_R => Modifiers::ALT,
            key::META_L | key::META_R | key::SUPER_L | key::SUPER_R => Modifiers::SUPER,
            key::CAPS_LOCK => Modifiers::CAPS_LOCK,
            key::NUM_LOCK => Modifiers::NUM_LOCK,
            _ => return,
        };
        match action {
            KeyAction::Press | KeyAction::Repeat => self.mods |= bit,
            KeyAction::Release => self.mods -= bit,
        }
    }
}

fn name_char(event: &Event) -> Option<char> {
    let name = event.name.as_deref()?;
    if name.chars().count() == 1 {
        name.chars().next()
    } else {
        None
    }
}

fn button_from_rbutton(button: RButton) -> PointerButton {
    match button {
        RButton::Left => PointerButton::Left,
        RButton::Middle => PointerButton::Middle,
        RButton::Right => PointerButton::Right,
        // XBUTTON1 = 1, XBUTTON2 = 2 on Windows.
        RButton::Unknown(1) => PointerButton::Back,
        RButton::Unknown(2) => PointerButton::Forward,
        RButton::Unknown(n) => PointerButton::Other(n),
    }
}

fn clamp_i32(value: f64) -> i32 {
    if value.is_nan() || value < i32::MIN as f64 {
        0
    } else if value > i32::MAX as f64 {
        i32::MAX
    } else {
        value as i32
    }
}

pub struct WinCapture {
    screen: ScreenInfo,
    started: bool,
}

impl WinCapture {
    pub fn new() -> Result<Self> {
        let (width, height) = rdev::display_size().map_err(|e| {
            anyhow!("cannot read the display size (is a desktop session active?): {e:?}")
        })?;
        Ok(Self {
            screen: ScreenInfo {
                width: u32::try_from(width).unwrap_or(0),
                height: u32::try_from(height).unwrap_or(0),
            },
            started: false,
        })
    }
}

impl Capture for WinCapture {
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
        let hook = Arc::new(Mutex::new(HookState {
            tx,
            mods: Modifiers::empty(),
            chars: HashMap::new(),
        }));
        let thread_stop = stop.clone();

        std::thread::Builder::new()
            .name("alfone-win-capture".into())
            .spawn(move || {
                let result = grab(move |event: Event| {
                    // Low-level hooks must not stall the system: keep work to a
                    // minimum and never block on the channel (mpsc is
                    // unbounded, so `send` always returns immediately).
                    {
                        let mut state = hook.lock().expect("capture state poisoned");
                        if let Some(captured) = state.convert(&event) {
                            let _ = state.tx.send(captured);
                        }
                    }
                    // While the pointer is on the remote screen, local
                    // applications must not receive this input. The daemon
                    // still sees it above, which is what makes the "escape"
                    // hotkey (handled by the daemon) work.
                    if REMOTE_MODE.load(Ordering::Relaxed) {
                        None
                    } else {
                        Some(event)
                    }
                });
                if let Err(e) = result {
                    tracing::warn!("Windows input hook stopped: {e:?}");
                }
                while !thread_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            })
            .context("spawning capture thread")?;

        Ok(CaptureHandle::new(rx, stop))
    }
}

// ---------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------

/// What a keysym needs to be injected as.
enum Target {
    /// A virtual key; the bool says whether Shift must be held around it.
    Key(EKey, bool),
    /// No virtual key exists: type the character instead (unicode path).
    Text(char),
}

#[derive(Debug, Clone, Copy, Default)]
struct Auto {
    shift: bool,
}

pub struct WinInject {
    enigo: Enigo,
    screen: ScreenInfo,
    /// Keys currently held locally, keysym -> auto modifiers used for it.
    held: HashMap<u32, Auto>,
}

fn input_error(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("input simulation failed: {e}")
}

impl WinInject {
    pub fn new() -> Result<Self> {
        let enigo =
            Enigo::new(&Settings::default()).map_err(|e| anyhow!("input simulator: {e}"))?;
        let (width, height) = enigo
            .main_display()
            .map_err(|e| anyhow!("reading the display size: {e}"))?;
        Ok(Self {
            enigo,
            screen: ScreenInfo {
                width: u32::try_from(width).unwrap_or(0),
                height: u32::try_from(height).unwrap_or(0),
            },
            held: HashMap::new(),
        })
    }

    fn shift_held(&self) -> bool {
        self.held.contains_key(&key::SHIFT_L) || self.held.contains_key(&key::SHIFT_R)
    }

    fn key_target(keysym: u32) -> Option<Target> {
        let target = match keysym {
            key::BACKSPACE => Target::Key(EKey::Backspace, false),
            key::TAB => Target::Key(EKey::Tab, false),
            key::LINEFEED | key::RETURN => Target::Key(EKey::Return, false),
            key::CLEAR => Target::Key(EKey::Clear, false),
            key::PAUSE => Target::Key(EKey::Pause, false),
            key::SCROLL_LOCK => Target::Key(EKey::Scroll, false),
            key::ESCAPE => Target::Key(EKey::Escape, false),
            key::PRINT => Target::Key(EKey::PrintScr, false),
            key::DELETE => Target::Key(EKey::Delete, false),
            key::HOME => Target::Key(EKey::Home, false),
            key::LEFT => Target::Key(EKey::LeftArrow, false),
            key::UP => Target::Key(EKey::UpArrow, false),
            key::RIGHT => Target::Key(EKey::RightArrow, false),
            key::DOWN => Target::Key(EKey::DownArrow, false),
            key::PAGE_UP => Target::Key(EKey::PageUp, false),
            key::PAGE_DOWN => Target::Key(EKey::PageDown, false),
            key::END => Target::Key(EKey::End, false),
            key::BEGIN => Target::Key(EKey::Numpad5, false),
            key::INSERT => Target::Key(EKey::Insert, false),
            key::MENU => Target::Key(EKey::Apps, false),
            key::NUM_LOCK => Target::Key(EKey::Numlock, false),
            key::KP_ENTER => Target::Key(EKey::Return, false),
            key::KP_HOME => Target::Key(EKey::Numpad7, false),
            key::KP_LEFT => Target::Key(EKey::Numpad4, false),
            key::KP_UP => Target::Key(EKey::Numpad8, false),
            key::KP_RIGHT => Target::Key(EKey::Numpad6, false),
            key::KP_DOWN => Target::Key(EKey::Numpad2, false),
            key::KP_PAGE_UP => Target::Key(EKey::Numpad9, false),
            key::KP_PAGE_DOWN => Target::Key(EKey::Numpad3, false),
            key::KP_END => Target::Key(EKey::Numpad1, false),
            key::KP_INSERT => Target::Key(EKey::Numpad0, false),
            key::KP_DELETE => Target::Key(EKey::Decimal, false),
            key::KP_MULTIPLY => Target::Key(EKey::Multiply, false),
            key::KP_ADD => Target::Key(EKey::Add, false),
            key::KP_SUBTRACT => Target::Key(EKey::Subtract, false),
            key::KP_DECIMAL => Target::Key(EKey::Decimal, false),
            key::KP_DIVIDE => Target::Key(EKey::Divide, false),
            key::F1 => Target::Key(EKey::F1, false),
            key::F2 => Target::Key(EKey::F2, false),
            key::F3 => Target::Key(EKey::F3, false),
            key::F4 => Target::Key(EKey::F4, false),
            key::F5 => Target::Key(EKey::F5, false),
            key::F6 => Target::Key(EKey::F6, false),
            key::F7 => Target::Key(EKey::F7, false),
            key::F8 => Target::Key(EKey::F8, false),
            key::F9 => Target::Key(EKey::F9, false),
            key::F10 => Target::Key(EKey::F10, false),
            key::F11 => Target::Key(EKey::F11, false),
            key::F12 => Target::Key(EKey::F12, false),
            key::SHIFT_L => Target::Key(EKey::LShift, false),
            key::SHIFT_R => Target::Key(EKey::RShift, false),
            key::CONTROL_L => Target::Key(EKey::LControl, false),
            key::CONTROL_R => Target::Key(EKey::RControl, false),
            key::CAPS_LOCK => Target::Key(EKey::CapsLock, false),
            key::META_L | key::SUPER_L => Target::Key(EKey::Meta, false),
            key::META_R | key::SUPER_R => Target::Key(EKey::RWin, false),
            key::ALT_L => Target::Key(EKey::LMenu, false),
            key::ALT_R => Target::Key(EKey::RMenu, false),
            _ => return None,
        };
        Some(target)
    }

    /// ASCII text keysyms mapped to their US-layout virtual key. Layouts that
    /// move these keys still get the right *character* for the unshifted base
    /// keys because Windows maps virtual keys through the active layout.
    fn char_target(c: char) -> Option<Target> {
        let target = if c.is_ascii_lowercase() {
            EKey::Unicode(c)
        } else if c.is_ascii_uppercase() {
            // Lowercase key + Shift, like the X11 backend's level 1.
            EKey::Unicode(c.to_ascii_lowercase())
        } else {
            let (key, _) = match c {
                ' ' => (EKey::Space, false),
                '!' => (EKey::Num1, true),
                '@' => (EKey::Num2, true),
                '#' => (EKey::Num3, true),
                '$' => (EKey::Num4, true),
                '%' => (EKey::Num5, true),
                '^' => (EKey::Num6, true),
                '&' => (EKey::Num7, true),
                '*' => (EKey::Num8, true),
                '(' => (EKey::Num9, true),
                ')' => (EKey::Num0, true),
                '_' => (EKey::OEMMinus, true),
                '+' => (EKey::OEMPlus, true),
                '{' => (EKey::OEM4, true),
                '}' => (EKey::OEM6, true),
                '|' => (EKey::OEM5, true),
                ':' => (EKey::OEM1, true),
                '"' => (EKey::OEM7, true),
                '<' => (EKey::OEMComma, true),
                '>' => (EKey::OEMPeriod, true),
                '?' => (EKey::OEM2, true),
                '~' => (EKey::OEM3, true),
                ',' => (EKey::OEMComma, false),
                '.' => (EKey::OEMPeriod, false),
                '-' => (EKey::OEMMinus, false),
                '=' => (EKey::OEMPlus, false),
                ';' => (EKey::OEM1, false),
                '\'' => (EKey::OEM7, false),
                '/' => (EKey::OEM2, false),
                '`' => (EKey::OEM3, false),
                '[' => (EKey::OEM4, false),
                '\\' => (EKey::OEM5, false),
                ']' => (EKey::OEM6, false),
                c if c.is_ascii_digit() => (
                    match c {
                        '0' => EKey::Num0,
                        '1' => EKey::Num1,
                        '2' => EKey::Num2,
                        '3' => EKey::Num3,
                        '4' => EKey::Num4,
                        '5' => EKey::Num5,
                        '6' => EKey::Num6,
                        '7' => EKey::Num7,
                        '8' => EKey::Num8,
                        _ => EKey::Num9,
                    },
                    false,
                ),
                _ => return None,
            };
            key
        };
        let needs_shift = c.is_ascii_uppercase()
            || matches!(
                c,
                '!' | '@'
                    | '#'
                    | '$'
                    | '%'
                    | '^'
                    | '&'
                    | '*'
                    | '('
                    | ')'
                    | '_'
                    | '+'
                    | '{'
                    | '}'
                    | '|'
                    | ':'
                    | '"'
                    | '<'
                    | '>'
                    | '?'
                    | '~'
            );
        // `EKey::Unicode` on Windows maps through `VkKeyScanEx` and *loses* the
        // shift state, so uppercase must be handled with an explicit Shift.
        // Uppercase letters map to the lowercase variant above on purpose.
        if let EKey::Unicode(uc) = target {
            if uc.is_ascii_lowercase() {
                return Some(Target::Key(uc_to_letter(uc), needs_shift));
            }
            return Some(Target::Text(c));
        }
        Some(Target::Key(target, needs_shift))
    }

    fn target_for(&self, keysym: u32) -> Option<Target> {
        if let Some(target) = Self::key_target(keysym) {
            return Some(target);
        }
        key::to_char(keysym).and_then(Self::char_target)
    }

    fn key(&mut self, event: &KeyEvent) -> Result<()> {
        // Modifier keys are forwarded as-is.
        if key::is_modifier(event.keysym) {
            let Some(target) = Self::key_target(event.keysym) else {
                return Ok(());
            };
            let Target::Key(k, _) = target else {
                return Ok(());
            };
            match event.action {
                KeyAction::Press | KeyAction::Repeat => {
                    if !self.held.contains_key(&event.keysym) {
                        self.enigo.key(k, Direction::Press).map_err(input_error)?;
                        self.held.insert(event.keysym, Auto::default());
                    }
                }
                KeyAction::Release => {
                    if self.held.remove(&event.keysym).is_some() {
                        self.enigo.key(k, Direction::Release).map_err(input_error)?;
                    }
                }
            }
            return Ok(());
        }

        match self.target_for(event.keysym) {
            Some(Target::Key(k, needs_shift)) => {
                let auto = Auto {
                    shift: needs_shift && !self.shift_held(),
                };
                match event.action {
                    KeyAction::Release => {
                        if let Some(auto) = self.held.remove(&event.keysym) {
                            self.enigo.key(k, Direction::Release).map_err(input_error)?;
                            if auto.shift {
                                let _ = self.enigo.key(EKey::LShift, Direction::Release);
                            }
                        }
                    }
                    KeyAction::Press | KeyAction::Repeat => {
                        if auto.shift {
                            self.enigo
                                .key(EKey::LShift, Direction::Press)
                                .map_err(input_error)?;
                        }
                        self.enigo.key(k, Direction::Press).map_err(input_error)?;
                        // A new Press of an already-held key is Windows'
                        // auto-repeat; mimic Linux: deterministic tap.
                        let _ = self.enigo.key(k, Direction::Release);
                        if event.action == KeyAction::Repeat {
                            if auto.shift {
                                let _ = self.enigo.key(EKey::LShift, Direction::Release);
                            }
                            self.held.remove(&event.keysym);
                        } else {
                            self.held.insert(event.keysym, auto);
                        }
                    }
                }
            }
            Some(Target::Text(c)) => {
                // Unicode characters have no stable virtual key; type them on
                // press only, releases are meaningless for text entry.
                if event.action != KeyAction::Release {
                    let text = c.to_string();
                    self.enigo.text(&text).map_err(input_error)?;
                }
            }
            None => {
                tracing::debug!(keysym = event.keysym, "no virtual key for keysym");
            }
        }
        Ok(())
    }
}

/// Enigo's `Key::Unicode('a')` is not accepted by `key()` on Windows (it falls
/// back to text entry); letters have dedicated variants.
fn uc_to_letter(c: char) -> EKey {
    let key = match c {
        'a' => EKey::A,
        'b' => EKey::B,
        'c' => EKey::C,
        'd' => EKey::D,
        'e' => EKey::E,
        'f' => EKey::F,
        'g' => EKey::G,
        'h' => EKey::H,
        'i' => EKey::I,
        'j' => EKey::J,
        'k' => EKey::K,
        'l' => EKey::L,
        'm' => EKey::M,
        'n' => EKey::N,
        'o' => EKey::O,
        'p' => EKey::P,
        'q' => EKey::Q,
        'r' => EKey::R,
        's' => EKey::S,
        't' => EKey::T,
        'u' => EKey::U,
        'v' => EKey::V,
        'w' => EKey::W,
        'x' => EKey::X,
        'y' => EKey::Y,
        _ => EKey::Z,
    };
    key
}

impl Inject for WinInject {
    fn screen(&self) -> ScreenInfo {
        self.screen
    }

    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::Key(event) => self.key(event),
            InputEvent::PointerAbs { x, y } => self.warp(*x, *y),
            InputEvent::PointerRel { dx, dy } => {
                let (x, y) = self.enigo.location().map_err(input_error)?;
                self.warp(x.saturating_add(*dx), y.saturating_add(*dy))
            }
            InputEvent::Button { button, pressed } => {
                let Some(button) = enigo_button(*button) else {
                    return Ok(());
                };
                let direction = if *pressed {
                    Direction::Press
                } else {
                    Direction::Release
                };
                self.enigo.button(button, direction).map_err(input_error)
            }
            InputEvent::Wheel { dx, dy } => {
                // Protocol: +120 per notch, up/right positive. enigo's vertical
                // positive length scrolls down, horizontal positive goes right.
                let vertical = dy / 120;
                let horizontal = dx / 120;
                if vertical != 0 {
                    self.enigo
                        .scroll(-vertical, Axis::Vertical)
                        .map_err(input_error)?;
                }
                if horizontal != 0 {
                    self.enigo
                        .scroll(horizontal, Axis::Horizontal)
                        .map_err(input_error)?;
                }
                Ok(())
            }
        }
    }

    fn pointer(&mut self) -> Result<(i32, i32)> {
        self.enigo.location().map_err(input_error)
    }

    fn warp(&mut self, x: i32, y: i32) -> Result<()> {
        let x = x.clamp(
            0,
            i32::try_from(self.screen.width.saturating_sub(1)).unwrap_or(i32::MAX),
        );
        let y = y.clamp(
            0,
            i32::try_from(self.screen.height.saturating_sub(1)).unwrap_or(i32::MAX),
        );
        self.enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(input_error)
    }

    /// The capture hook drops local input while `REMOTE_MODE` is set, so
    /// injection needs no extra work on Windows.
    fn set_remote_mode(&mut self, remote: bool) -> Result<()> {
        REMOTE_MODE.store(remote, Ordering::Relaxed);
        Ok(())
    }
}
