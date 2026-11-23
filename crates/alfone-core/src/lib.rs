//! Configuration, screen layout and edge routing.
//!
//! The server owns the layout: it knows which physical machine sits on each
//! side of every screen. When the pointer hits an edge, [`Layout::cross`]
//! produces the target screen and the position inside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use alfone_protocol::{LayoutSnapshot, Role, ScreenDescriptor};

pub const DEFAULT_PORT: u16 = 24800;
/// Port of the local HTTP control plane (`alfone serve` -> Tauri UI).
pub const DEFAULT_HTTP_PORT: u16 = 24801;

// ---------------------------------------------------------------------------
// Screen layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Top,
    Bottom,
}

impl Direction {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenConfig {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
    #[serde(default)]
    pub top: Option<String>,
    #[serde(default)]
    pub bottom: Option<String>,
}

impl ScreenConfig {
    pub fn new(id: impl Into<String>, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            width,
            height,
            left: None,
            right: None,
            top: None,
            bottom: None,
        }
    }

    pub fn neighbor(&self, direction: Direction) -> Option<&str> {
        match direction {
            Direction::Left => self.left.as_deref(),
            Direction::Right => self.right.as_deref(),
            Direction::Top => self.top.as_deref(),
            Direction::Bottom => self.bottom.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub screens: Vec<ScreenConfig>,
    pub primary: String,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            screens: vec![ScreenConfig::new("local", "This computer", 1920, 1080)],
            primary: "local".into(),
        }
    }
}

impl LayoutConfig {
    pub fn snapshot(&self) -> LayoutSnapshot {
        LayoutSnapshot {
            screens: self
                .screens
                .iter()
                .map(|s| ScreenDescriptor {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    width: s.width,
                    height: s.height,
                    left: s.left.clone(),
                    right: s.right.clone(),
                    top: s.top.clone(),
                    bottom: s.bottom.clone(),
                })
                .collect(),
            primary: self.primary.clone(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("the primary screen '{0}' does not exist")]
    MissingPrimary(String),
    #[error("duplicate screen id '{0}'")]
    DuplicateId(String),
    #[error("screen '{0}' has an empty id")]
    EmptyId(String),
    #[error("screen '{id}' has invalid size {width}x{height}")]
    InvalidSize {
        id: String,
        width: u32,
        height: u32,
    },
    #[error("screen '{0}' points to itself")]
    SelfReference(String),
    #[error("screen '{from}' references unknown screen '{to}'")]
    UnknownNeighbor { from: String, to: String },
    #[error("screens '{a}' and '{b}' disagree about their shared edge")]
    AsymmetricEdge { a: String, b: String },
}

#[derive(Debug, Clone)]
pub struct Layout {
    screens: HashMap<String, ScreenConfig>,
    primary: String,
}

impl Layout {
    pub fn new(config: &LayoutConfig) -> Result<Self, Vec<LayoutError>> {
        let mut errors = Vec::new();
        let mut screens = HashMap::new();

        for screen in &config.screens {
            if screen.id.trim().is_empty() {
                errors.push(LayoutError::EmptyId(screen.name.clone()));
            }
            if screen.width == 0 || screen.height == 0 {
                errors.push(LayoutError::InvalidSize {
                    id: screen.id.clone(),
                    width: screen.width,
                    height: screen.height,
                });
            }
            if screens.insert(screen.id.clone(), screen.clone()).is_some() {
                errors.push(LayoutError::DuplicateId(screen.id.clone()));
            }
        }

        if !screens.contains_key(&config.primary) {
            errors.push(LayoutError::MissingPrimary(config.primary.clone()));
        }

        for screen in screens.values() {
            for direction in [
                Direction::Left,
                Direction::Right,
                Direction::Top,
                Direction::Bottom,
            ] {
                let Some(to) = screen.neighbor(direction) else {
                    continue;
                };
                if to == screen.id {
                    errors.push(LayoutError::SelfReference(screen.id.clone()));
                    continue;
                }
                let Some(target) = screens.get(to) else {
                    errors.push(LayoutError::UnknownNeighbor {
                        from: screen.id.clone(),
                        to: to.to_string(),
                    });
                    continue;
                };
                // Both sides must agree, otherwise returning from the target
                // screen would send the pointer to the wrong place.
                if target.neighbor(direction.opposite()) != Some(screen.id.as_str()) {
                    errors.push(LayoutError::AsymmetricEdge {
                        a: screen.id.clone(),
                        b: to.to_string(),
                    });
                }
            }
        }

        if errors.is_empty() {
            Ok(Self {
                screens,
                primary: config.primary.clone(),
            })
        } else {
            Err(errors)
        }
    }

    pub fn primary(&self) -> &ScreenConfig {
        self.screens
            .get(&self.primary)
            .expect("validated primary screen")
    }

    pub fn screen(&self, id: &str) -> Option<&ScreenConfig> {
        self.screens.get(id)
    }

    pub fn screens(&self) -> impl Iterator<Item = &ScreenConfig> {
        self.screens.values()
    }

    pub fn neighbor(&self, from: &str, direction: Direction) -> Option<&str> {
        self.screens.get(from)?.neighbor(direction)
    }

    /// Serialisable view of the layout, sent to peers and used by the UI.
    pub fn snapshot(&self) -> LayoutSnapshot {
        let mut screens: Vec<&ScreenConfig> = self.screens.values().collect();
        screens.sort_by(|a, b| a.id.cmp(&b.id));
        LayoutSnapshot {
            screens: screens
                .into_iter()
                .map(|s| ScreenDescriptor {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    width: s.width,
                    height: s.height,
                    left: s.left.clone(),
                    right: s.right.clone(),
                    top: s.top.clone(),
                    bottom: s.bottom.clone(),
                })
                .collect(),
            primary: self.primary.clone(),
        }
    }

    /// Compute where the pointer appears when it leaves `from` through
    /// `direction`. `ratio` is the position along the shared edge in `0..=1`.
    pub fn cross(&self, from: &str, direction: Direction, ratio: f32) -> Option<Crossing> {
        let source = self.screens.get(from)?;
        let target_id = source.neighbor(direction)?;
        let target = self.screens.get(target_id)?;
        let ratio = ratio.clamp(0.0, 1.0);

        let (x, y) = match direction {
            Direction::Right => (
                1,
                (ratio * target.height.saturating_sub(1) as f32).round() as i32,
            ),
            Direction::Left => (
                target.width.saturating_sub(2) as i32,
                (ratio * target.height.saturating_sub(1) as f32).round() as i32,
            ),
            Direction::Bottom => (
                (ratio * target.width.saturating_sub(1) as f32).round() as i32,
                1,
            ),
            Direction::Top => (
                (ratio * target.width.saturating_sub(1) as f32).round() as i32,
                target.height.saturating_sub(2) as i32,
            ),
        };

        Some(Crossing {
            target: target_id.to_string(),
            x,
            y,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing {
    pub target: String,
    pub x: i32,
    pub y: i32,
}

/// Detects when a local absolute pointer reaches an edge with outward motion.
#[derive(Debug, Clone, Copy)]
pub struct EdgeDetector {
    /// Pixels from the border that count as "at the edge".
    pub threshold: i32,
}

impl Default for EdgeDetector {
    fn default() -> Self {
        Self { threshold: 1 }
    }
}

impl EdgeDetector {
    /// `x`,`y` is the pointer, `dx`,`dy` the last motion, `w`,`h` the screen.
    pub fn detect(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        w: u32,
        h: u32,
    ) -> Option<(Direction, f32)> {
        let w = w as i32;
        let h = h as i32;
        if w <= 0 || h <= 0 {
            return None;
        }
        let threshold = self.threshold.max(0);

        if dx < 0 && x <= threshold {
            Some((Direction::Left, y as f32 / h as f32))
        } else if dx > 0 && x >= w - 1 - threshold {
            Some((Direction::Right, y as f32 / h as f32))
        } else if dy < 0 && y <= threshold {
            Some((Direction::Top, x as f32 / w as f32))
        } else if dy > 0 && y >= h - 1 - threshold {
            Some((Direction::Bottom, x as f32 / w as f32))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub device_id: String,
    pub device_name: String,
    pub role: Role,
    pub server: ServerConfig,
    pub client: ClientConfig,
    pub layout: LayoutConfig,
    pub clipboard: bool,
    pub audio: AudioConfig,
    pub video: VideoConfig,
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// UDP address for the QUIC listener.
    pub listen: String,
    /// Announce this machine on the LAN via mDNS.
    pub discovery: bool,
    /// Localhost address for the HTTP control plane used by the desktop UI.
    pub http_listen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub server: String,
    /// Hex SHA-256 of the server certificate. Empty means "ask the user".
    pub fingerprint: String,
    pub auto_reconnect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u16,
    pub bitrate_bps: u32,
    pub jitter_ms: u32,
    /// Empty = system default.
    pub capture_device: String,
    pub playback_device: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub enabled: bool,
    pub display_id: u32,
    pub fps: u16,
    pub bitrate_kbps: u32,
    /// Start in mirror mode. `Extend` needs a virtual display driver.
    pub mode: alfone_protocol::SessionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Accept a new fingerprint only after the user compares the short code.
    pub trust_on_first_use: bool,
    /// Hex fingerprint -> human name.
    pub trusted_peers: HashMap<String, String>,
    /// Bearer token for the local HTTP control plane. Empty = generate on save.
    pub http_token: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: random_id(),
            device_name: default_device_name(),
            role: Role::Server,
            server: ServerConfig {
                listen: format!("0.0.0.0:{DEFAULT_PORT}"),
                discovery: true,
                http_listen: format!("127.0.0.1:{DEFAULT_HTTP_PORT}"),
            },
            client: ClientConfig {
                server: format!("127.0.0.1:{DEFAULT_PORT}"),
                fingerprint: String::new(),
                auto_reconnect: true,
            },
            layout: LayoutConfig::default(),
            clipboard: true,
            audio: AudioConfig {
                enabled: false,
                sample_rate: 48_000,
                channels: 2,
                frame_ms: 20,
                bitrate_bps: 96_000,
                jitter_ms: 40,
                capture_device: String::new(),
                playback_device: String::new(),
            },
            video: VideoConfig {
                enabled: false,
                display_id: 0,
                fps: 30,
                bitrate_kbps: 8_000,
                mode: alfone_protocol::SessionMode::Mirror,
            },
            security: SecurityConfig {
                trust_on_first_use: true,
                trusted_peers: HashMap::new(),
                http_token: String::new(),
            },
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Config::default().server
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Config::default().client
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Config::default().audio
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Config::default().video
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Config::default().security
    }
}

impl Config {
    pub fn load_or_create(path: Option<&Path>) -> Result<(Self, PathBuf), ConfigError> {
        let path = path.map(Path::to_path_buf).unwrap_or_else(default_config_path);
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&text)?;
            if config.device_id.is_empty() {
                config.device_id = random_id();
            }
            Ok((config, path))
        } else {
            let config = Config::default();
            config.save_to(&path)?;
            Ok((config, path))
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn layout(&self) -> Result<Layout, Vec<LayoutError>> {
        Layout::new(&self.layout)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "alfone", "Alfone")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".alfone"))
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Random bearer token for the local HTTP control plane (64 hex chars).
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn default_device_name() -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(name) = std::env::var("COMPUTERNAME") {
            return name;
        }
    }
    if let Ok(name) = std::env::var("HOSTNAME") {
        if !name.is_empty() {
            return name;
        }
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    "alfone".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_screens() -> LayoutConfig {
        LayoutConfig {
            screens: vec![
                ScreenConfig {
                    id: "a".into(),
                    name: "A".into(),
                    width: 1920,
                    height: 1080,
                    right: Some("b".into()),
                    ..ScreenConfig::new("a", "A", 1920, 1080)
                },
                ScreenConfig {
                    id: "b".into(),
                    name: "B".into(),
                    width: 2560,
                    height: 1440,
                    left: Some("a".into()),
                    ..ScreenConfig::new("b", "B", 2560, 1440)
                },
            ],
            primary: "a".into(),
        }
    }

    #[test]
    fn valid_layout() {
        let layout = Layout::new(&two_screens()).unwrap();
        assert_eq!(layout.primary().id, "a");
        assert_eq!(layout.neighbor("a", Direction::Right), Some("b"));
    }

    #[test]
    fn snapshot_is_stable_and_complete() {
        let layout = Layout::new(&two_screens()).unwrap();
        let snapshot = layout.snapshot();
        assert_eq!(snapshot.primary, "a");
        assert_eq!(snapshot.screens.len(), 2);
        assert_eq!(snapshot.screens[0].id, "a");
        assert_eq!(snapshot.screens[1].id, "b");
        assert_eq!(snapshot.screens[0].right.as_deref(), Some("b"));
        // Calling it twice yields the same order (sorted by id).
        assert_eq!(layout.snapshot(), snapshot);
    }

    #[test]
    fn crossing_right_maps_proportionally() {
        let layout = Layout::new(&two_screens()).unwrap();
        let c = layout.cross("a", Direction::Right, 0.5).unwrap();
        assert_eq!(c.target, "b");
        assert_eq!(c.x, 1);
        assert_eq!(c.y, 720); // 50 % of 1440
    }

    #[test]
    fn crossing_left_enters_from_right_edge() {
        let layout = Layout::new(&two_screens()).unwrap();
        let c = layout.cross("b", Direction::Left, 0.25).unwrap();
        assert_eq!(c.target, "a");
        assert_eq!(c.x, 1918);
        assert_eq!(c.y, 270);
    }

    #[test]
    fn asymmetric_layout_is_rejected() {
        let mut config = two_screens();
        config.screens[1].left = None;
        let errors = Layout::new(&config).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, LayoutError::AsymmetricEdge { .. })));
    }

    #[test]
    fn unknown_neighbor_is_rejected() {
        let mut config = two_screens();
        config.screens[0].right = Some("ghost".into());
        let errors = Layout::new(&config).unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, LayoutError::UnknownNeighbor { .. })));
    }

    #[test]
    fn edge_detector_triggers_outward_only() {
        let detector = EdgeDetector::default();
        assert_eq!(
            detector.detect(0, 540, -3, 0, 1920, 1080).map(|c| c.0),
            Some(Direction::Left)
        );
        assert_eq!(detector.detect(0, 540, 3, 0, 1920, 1080), None);
        assert_eq!(
            detector.detect(1000, 1079, 0, 5, 1920, 1080).map(|c| c.0),
            Some(Direction::Bottom)
        );
    }

    #[test]
    fn config_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::default();
        config.save_to(&path).unwrap();
        let (loaded, _) = Config::load_or_create(Some(&path)).unwrap();
        assert_eq!(loaded.device_id, config.device_id);
        assert_eq!(loaded.layout, config.layout);
    }
}
