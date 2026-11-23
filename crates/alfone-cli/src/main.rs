//! `alfone` command line interface.
//!
//! Two long-running commands make a session:
//!
//! - `alfone serve` runs on the computer that owns the keyboard/mouse. It
//!   listens for a QUIC connection, captures local input and forwards it when
//!   the pointer reaches the edge of the local screen.
//! - `alfone connect` runs on the other computer. It receives input events and
//!   injects them into the local desktop.
//!
//! `init`, `doctor` and `pair` are the setup/diagnostic commands.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use tracing::{debug, info, warn};

mod http;

use alfone_core::{Config, Direction, Layout, LayoutConfig, ScreenConfig, DEFAULT_HTTP_PORT, DEFAULT_PORT};
use alfone_core::random_token;
use alfone_discovery::{browse, Advertiser, Announcement, SERVICE_TYPE};
use alfone_input::{create_capture, create_inject, CaptureHandle, Inject, ScreenInfo};
use alfone_net::{
    connect, generate_pin, hex, parse_hex_fingerprint, ConnectOptions, Identity, NetError, Peer,
    Server,
};
use alfone_protocol::{
    key, Capabilities, DeviceInfo, DisplayInfo, EnterScreen, InputEvent, KeyAction, KeyEvent,
    LeaveScreen, Message, Modifiers, Os, SessionMode,
};

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "alfone",
    version,
    about = "Keyboard/mouse sharing, audio and extra screens between computers",
    long_about = None
)]
struct Cli {
    /// Configuration file (defaults to the per-user config directory).
    #[arg(long, global = true, env = "ALFONE_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the configuration and this device's identity (certificate).
    Init,
    /// Check the environment: config, identity, input backend and network.
    Doctor,
    /// List Alfone servers announced on the local network.
    Discover(DiscoverArgs),
    /// Connect once with a PIN and remember the server's fingerprint.
    Pair(PairArgs),
    /// Run the server: share this computer's keyboard and mouse.
    Serve(ServeArgs),
    /// Run the client: receive input from a server.
    Connect(ConnectArgs),
}

#[derive(clap::Args)]
struct PairArgs {
    /// Server address, e.g. 192.168.1.20:24800.
    #[arg(long, default_value = "127.0.0.1:24800")]
    server: String,
    /// PIN shown by `alfone serve` on the other computer.
    #[arg(long)]
    pin: String,
    /// Expected server certificate fingerprint (hex SHA-256).
    #[arg(long)]
    fingerprint: Option<String>,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// UDP address to listen on (default from the config, 0.0.0.0:24800).
    #[arg(long)]
    listen: Option<String>,
    /// Fixed PIN instead of a random one.
    #[arg(long, conflicts_with = "no_pin")]
    pin: Option<String>,
    /// Disable PIN pairing. Only safe together with pinned fingerprints.
    #[arg(long)]
    no_pin: bool,
    /// Where the remote screen sits relative to this one.
    #[arg(long, value_enum, default_value_t = Side::Right)]
    side: Side,
}

#[derive(clap::Args)]
struct DiscoverArgs {
    /// Seconds to listen for announcements.
    #[arg(long, default_value_t = 3)]
    timeout: u64,
}

#[derive(clap::Args)]
struct ConnectArgs {
    /// Server address, e.g. 192.168.1.20:24800 (default from the config).
    #[arg(long, conflicts_with = "name")]
    server: Option<String>,
    /// Server name announced via mDNS (instead of an address).
    #[arg(long)]
    name: Option<String>,
    /// Seconds to wait for mDNS when using --name.
    #[arg(long, default_value_t = 3)]
    discover_timeout: u64,
    /// PIN shown by `alfone serve`.
    #[arg(long)]
    pin: Option<String>,
    /// Expected server fingerprint; overrides the saved one.
    #[arg(long)]
    fingerprint: Option<String>,
    /// Reconnect automatically when the session drops (default from the config).
    #[arg(long, conflicts_with = "no_reconnect")]
    reconnect: bool,
    /// Exit when the session drops instead of reconnecting.
    #[arg(long)]
    no_reconnect: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl From<Side> for Direction {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => Direction::Left,
            Side::Right => Direction::Right,
            Side::Top => Direction::Top,
            Side::Bottom => Direction::Bottom,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Init => cmd_init(cli.config.as_deref()),
        Command::Doctor => cmd_doctor(cli.config.as_deref()).await,
        Command::Discover(args) => cmd_discover(args),
        Command::Pair(args) => cmd_pair(cli.config.as_deref(), args).await,
        Command::Serve(args) => cmd_serve(cli.config.as_deref(), args).await,
        Command::Connect(args) => cmd_connect(cli.config.as_deref(), args).await,
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

// ---------------------------------------------------------------------------
// init / doctor
// ---------------------------------------------------------------------------

fn cmd_init(explicit: Option<&Path>) -> Result<()> {
    let (config, path) = Config::load_or_create(explicit)?;
    let identity = Identity::load_or_create(identity_dir(&path))?;

    println!("config:      {}", path.display());
    println!("identity:    {}", identity_dir(&path).display());
    println!("device:      {} ({})", config.device_name, config.device_id);
    println!("platform:    {:?}", Os::current());
    println!("fingerprint: {}", identity.fingerprint_hex());
    println!("short code:  {}", identity.short_code());
    println!();
    println!("Next: run `alfone serve` on this computer and `alfone connect` on the other.");
    Ok(())
}

async fn cmd_doctor(explicit: Option<&Path>) -> Result<()> {
    println!("alfone doctor");
    println!("=============");

    let (config, path) = Config::load_or_create(explicit)?;
    println!("config:      {}", path.display());
    println!("device:      {} ({})", config.device_name, config.device_id);
    println!("platform:    {:?}", Os::current());

    if matches!(Os::current(), Os::LinuxWayland) {
        warn!("a Wayland session was detected; the current input backend only supports X11");
    }

    let dir = identity_dir(&path);
    if Identity::cert_path(&dir).exists() {
        let identity = Identity::load_or_create(&dir)?;
        println!("identity:    {}", dir.display());
        println!("fingerprint: {}", identity.fingerprint_hex());
        println!("short code:  {}", identity.short_code());
    } else {
        println!("identity:    not created yet (run `alfone init`)");
    }

    match create_capture() {
        Ok(capture) => {
            let screen = capture.screen();
            println!(
                "capture:     ok ({}x{})",
                screen.width, screen.height
            );
        }
        Err(e) => warn!("capture:     unavailable: {e:#}"),
    }
    match create_inject() {
        Ok(inject) => {
            let screen = inject.screen();
            println!("injection:   ok ({}x{})", screen.width, screen.height);
        }
        Err(e) => warn!("injection:   unavailable: {e:#}"),
    }

    let listen = &config.server.listen;
    match std::net::UdpSocket::bind(listen) {
        Ok(_) => println!("listener:    can bind {listen}"),
        Err(e) => warn!("listener:    cannot bind {listen}: {e}"),
    }

    Ok(())
}

fn cmd_discover(args: DiscoverArgs) -> Result<()> {
    println!(
        "searching for Alfone servers on the LAN ({SERVICE_TYPE}, {}s) ...",
        args.timeout
    );
    let peers = browse(Duration::from_secs(args.timeout))?;
    if peers.is_empty() {
        println!("no servers found. Start `alfone serve` on the other computer.");
        return Ok(());
    }
    println!("{} server(s):", peers.len());
    for peer in peers {
        let addr = peer
            .socket_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "no address".into());
        println!("  {}", peer.name);
        println!("    address: {addr}");
        println!("    id:      {}", peer.id);
        println!("    code:    {}", peer.fingerprint);
        println!("    version: {}", peer.version);
    }
    println!();
    println!("Connect with: alfone connect --name <name>");
    Ok(())
}

// ---------------------------------------------------------------------------
// pair
// ---------------------------------------------------------------------------

async fn cmd_pair(explicit: Option<&Path>, args: PairArgs) -> Result<()> {
    let (mut config, path) = Config::load_or_create(explicit)?;
    let _identity = Identity::load_or_create(identity_dir(&path))?;

    let addr = parse_endpoint(&args.server, DEFAULT_PORT)?;
    let expected = match &args.fingerprint {
        Some(text) => Some(
            parse_hex_fingerprint(text)
                .with_context(|| format!("invalid fingerprint: {text}"))?,
        ),
        None => None,
    };

    println!("connecting to {addr} ...");
    let peer = connect(
        addr,
        ConnectOptions {
            expected_fingerprint: expected,
            pin: Some(args.pin.clone()),
            ..ConnectOptions::default()
        },
        device_info(&config),
        capabilities(&config),
        SessionMode::Kvm,
    )
    .await
    .context("pairing failed")?;

    let fingerprint = hex(&peer.fingerprint);
    println!("paired with '{}' ({:?})", peer.device.name, peer.device.os);
    println!("fingerprint: {fingerprint}");
    println!("short code:  {}", alfone_net::short_code(&peer.fingerprint));

    config.client.server = args.server;
    config.client.fingerprint = fingerprint.clone();
    config
        .security
        .trusted_peers
        .insert(fingerprint, peer.device.name.clone());
    config.save_to(&path)?;
    println!("saved to {}", path.display());

    peer.link
        .send(Message::Disconnect {
            reason: "pairing complete".into(),
        })
        .await
        .ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

async fn cmd_serve(explicit: Option<&Path>, args: ServeArgs) -> Result<()> {
    let (mut config, path) = Config::load_or_create(explicit)?;
    let identity = Identity::load_or_create(identity_dir(&path))?;

    let listen = args
        .listen
        .clone()
        .unwrap_or_else(|| config.server.listen.clone());
    let addr = parse_endpoint(&listen, DEFAULT_PORT)?;
    let pin = if args.no_pin {
        None
    } else {
        Some(args.pin.clone().unwrap_or_else(generate_pin))
    };

    let server = Server::bind(identity.clone(), addr, pin.clone())?;
    let local_addr = server.local_addr()?;

    // Optional LAN discovery so clients can use `--name` instead of an IP.
    // The advertiser lives until `serve` exits (Drop unregisters it).
    let _advertiser = if config.server.discovery {
        match Advertiser::start(Announcement {
            device_name: config.device_name.clone(),
            id: config.device_id.clone(),
            fingerprint: identity.short_code(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            port: local_addr.port(),
        }) {
            Ok(advertiser) => {
                println!("  discovery:    mDNS {SERVICE_TYPE}");
                Some(advertiser)
            }
            Err(e) => {
                warn!("mDNS discovery disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Local HTTP control plane for the desktop UI (`GET /health`, `/status`,
    // `/layout`, `PUT /layout`). The daemon keeps owning the QUIC session.
    let token = if config.security.http_token.trim().is_empty() {
        random_token()
    } else {
        config.security.http_token.clone()
    };
    if config.security.http_token.trim().is_empty() {
        config.security.http_token = token.clone();
        config.save_to(&path)?;
    }
    let http_state = http::HttpState {
        token: token.clone(),
        config_path: path.clone(),
        state: Arc::new(AtomicU8::new(http::STATE_IDLE)),
    };
    let http_state_task = http_state.clone();
    let session_state = http_state.state.clone();
    let mut http_addr = parse_endpoint(&config.server.http_listen, DEFAULT_HTTP_PORT)?;
    // The control plane is for the local UI only. Never expose it on the LAN,
    // even if an old config says so.
    if !http_addr.ip().is_loopback() {
        warn!(
            "http_listen {} is not loopback; forcing 127.0.0.1",
            http_addr
        );
        http_addr.set_ip(std::net::IpAddr::from([127, 0, 0, 1]));
    }
    println!("  http control:  http://{http_addr} (token required)");
    println!("  http token:    {token}");

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(http_addr).await {
            Ok(listener) => listener,
            Err(e) => {
                warn!("http control plane bind failed: {e:#}");
                return;
            }
        };
        if let Err(e) = http::serve(listener, http_state_task).await {
            warn!("http control plane stopped: {e:#}");
        }
    });
    let side: Direction = args.side.into();

    println!("alfone server");
    println!("  listening:   {local_addr}");
    println!("  device:      {}", config.device_name);
    println!("  fingerprint: {}", identity.fingerprint_hex());
    println!("  short code:  {}", identity.short_code());
    match &pin {
        Some(pin) => println!("  pairing PIN: {pin}"),
        None => println!("  pairing PIN: disabled (--no-pin)"),
    }

    let mut inject = create_inject().context("creating the input injection backend")?;
    // Capture starts before the first client so the screen size is known.
    let mut capture = create_capture().context("creating the input capture backend")?;
    let local_screen = capture.screen();
    let capture_handle = capture.start()?;
    let mut capture_rx = bridge_capture(capture_handle);
    let local_name = config.device_name.clone();

    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
    let mut capture_alive = true;

    loop {
        println!("waiting for a client...");
        let accept = server.accept({
            device_info(&config)
        }, capabilities(&config), SessionMode::Kvm);
        tokio::pin!(accept);

        // While nobody is connected, discard captured input instead of letting
        // the bridge queue grow without bound.
        let peer = loop {
            tokio::select! {
                result = &mut accept => break result,
                event = capture_rx.recv(), if capture_alive => {
                    if event.is_none() {
                        capture_alive = false;
                        warn!("input capture stopped; no new sessions can start");
                    }
                }
                _ = &mut ctrl_c => {
                    println!("bye");
                    return Ok(());
                }
            }
        };

        match peer {
            Ok(peer) => {
                session_state.store(http::STATE_ACTIVE, Ordering::SeqCst);
                match run_server_session(
                    peer,
                    &mut *inject,
                    &mut capture_rx,
                    local_screen,
                    side,
                    &local_name,
                )
                .await
                {
                    Ok(SessionEnd::Closed) => {}
                    Ok(SessionEnd::Shutdown) => {
                        println!("bye");
                        return Ok(());
                    }
                    Err(e) => warn!("session ended: {e:#}"),
                }
                // The remote screen is no longer active.
                if let Err(e) = inject.set_remote_mode(false) {
                    debug!("ungrab after session: {e:#}");
                }
                session_state.store(http::STATE_IDLE, Ordering::SeqCst);
            }
            Err(NetError::PairingRejected(reason)) => {
                warn!("pairing rejected: {reason}");
            }
            Err(e) => warn!("connection failed: {e:#}"),
        }
    }
}

enum SessionEnd {
    /// Client disappeared; keep listening.
    Closed,
    /// Ctrl+C on the server; stop the process.
    Shutdown,
}

async fn run_server_session(
    peer: Peer,
    inject: &mut dyn Inject,
    capture_rx: &mut tokio::sync::mpsc::Receiver<alfone_input::CapturedEvent>,
    local_screen: ScreenInfo,
    side: Direction,
    local_name: &str,
) -> Result<SessionEnd> {
    let Peer {
        mut link,
        device,
        fingerprint,
        remote: remote_addr,
        ..
    } = peer;

    println!(
        "client connected: '{}' ({:?}) at {remote_addr}",
        device.name, device.os
    );
    println!("  client fingerprint: {}", hex(&fingerprint));

    // The client announces its display size right after the handshake.
    let mut remote_screen = ScreenInfo {
        width: 1920,
        height: 1080,
    };
    match tokio::time::timeout(Duration::from_secs(2), link.recv()).await {
        Ok(Some(Message::Displays(info))) => {
            if let Some(display) = primary_display(&info) {
                remote_screen = ScreenInfo {
                    width: display.width.max(1),
                    height: display.height.max(1),
                };
            }
        }
        Ok(Some(message)) => debug!("expected Displays, got {}", message.kind()),
        Ok(None) => return Ok(SessionEnd::Closed),
        Err(_) => debug!("client did not send Displays; assuming 1920x1080"),
    }

    let layout = build_layout(local_screen, remote_screen, side, local_name, &device.name);
    let tx = link.sender();

    tx.send(Message::Displays(DisplayInfo::single(
        local_screen.width,
        local_screen.height,
    )))
    .await?;
    tx.send(Message::Layout(layout.snapshot())).await?;

    println!(
        "  remote screen: {}x{} on the {}",
        remote_screen.width,
        remote_screen.height,
        side.label()
    );
    println!("  move the pointer past the {} edge; Ctrl+Alt+Esc returns", side.label());

    let local_w = local_screen.width.max(1) as i32;
    let local_h = local_screen.height.max(1) as i32;
    let mut last = inject.pointer().unwrap_or((local_w / 2, local_h / 2));
    let mut remote = false;
    let mut remote_pos = (0i32, 0i32);
    let mut entry = side;
    let mut sequence = 0u64;

    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            captured = capture_rx.recv() => {
                let Some(captured) = captured else {
                    return Ok(SessionEnd::Shutdown);
                };
                match captured.event {
                    InputEvent::PointerAbs { x, y } => {
                        if !remote {
                            last = (x, y);
                            if let Some(direction) = edge_at(x, y, local_w, local_h) {
                                if layout.neighbor("local", direction).is_some() {
                                    let ratio = ratio_along(direction, x, y, local_w, local_h);
                                    let Some(crossing) = layout.cross("local", direction, ratio) else {
                                        continue;
                                    };
                                    sequence += 1;
                                    tx.send(Message::EnterScreen(EnterScreen {
                                        x: crossing.x,
                                        y: crossing.y,
                                        sequence,
                                        modifiers: Modifiers::empty(),
                                    })).await?;
                                    remote = true;
                                    entry = direction;
                                    remote_pos = (crossing.x, crossing.y);
                                    inject.set_remote_mode(true)?;
                                    // The local cursor is clamped at the screen edge,
                                    // so move it to the middle to keep receiving
                                    // relative motion. The warp-generated event
                                    // reports the centre and yields a zero delta.
                                    let center = (local_w / 2, local_h / 2);
                                    inject.warp(center.0, center.1)?;
                                    last = center;
                                    info!(?direction, "pointer moved to the remote screen");
                                }
                            }
                        } else {
                            let dx = x - last.0;
                            let dy = y - last.1;
                            last = (x, y);
                            if dx != 0 || dy != 0 {
                                remote_pos.0 = (remote_pos.0 + dx)
                                    .clamp(0, remote_screen.width.saturating_sub(1) as i32);
                                remote_pos.1 = (remote_pos.1 + dy)
                                    .clamp(0, remote_screen.height.saturating_sub(1) as i32);
                                tx.send(Message::Input(InputEvent::PointerRel { dx, dy })).await?;
                            }
                            // Keep the local cursor away from the borders so it
                            // can always keep moving.
                            if near_border(x, y, local_w, local_h) {
                                let center = (local_w / 2, local_h / 2);
                                inject.warp(center.0, center.1)?;
                                last = center;
                            }
                        }
                    }
                    InputEvent::Key(event) => {
                        if remote {
                            let release = key::ESCAPE == event.keysym
                                && event.action == KeyAction::Press
                                && event.modifiers.contains(Modifiers::CONTROL | Modifiers::ALT);
                            if release {
                                release_remote_keys(&tx).await;
                                return_to_local(
                                    inject,
                                    &layout,
                                    entry,
                                    remote_pos,
                                    remote_screen,
                                )?;
                                remote = false;
                                last = inject.pointer().unwrap_or((local_w / 2, local_h / 2));
                                info!("returned to the local screen (Ctrl+Alt+Esc)");
                            } else {
                                tx.send(Message::Input(InputEvent::Key(event))).await?;
                            }
                        }
                    }
                    other => {
                        if remote {
                            tx.send(Message::Input(other)).await?;
                        }
                    }
                }
            }

            message = link.recv() => {
                let Some(message) = message else { break };
                match message {
                    Message::LeaveScreen(LeaveScreen { sequence: seq }) => {
                        if remote {
                            debug!(seq, "client left the remote screen");
                            return_to_local(inject, &layout, entry, remote_pos, remote_screen)?;
                            remote = false;
                            last = inject.pointer().unwrap_or((local_w / 2, local_h / 2));
                        }
                    }
                    Message::Ping { id } => {
                        tx.send(Message::Pong { id }).await?;
                    }
                    Message::Disconnect { reason } => {
                        println!("client disconnected: {reason}");
                        break;
                    }
                    Message::Error { code, message } => {
                        warn!("client error {code}: {message}");
                        break;
                    }
                    Message::Displays(info) => {
                        if let Some(display) = primary_display(&info) {
                            remote_screen = ScreenInfo {
                                width: display.width.max(1),
                                height: display.height.max(1),
                            };
                        }
                    }
                    other => debug!("ignoring {}", other.kind()),
                }
            }

            _ = &mut ctrl_c => {
                let _ = tx.send(Message::Disconnect {
                    reason: "server shutting down".into(),
                }).await;
                if remote {
                    let _ = inject.set_remote_mode(false);
                }
                return Ok(SessionEnd::Shutdown);
            }
        }
    }

    if remote {
        let _ = inject.set_remote_mode(false);
    }
    Ok(SessionEnd::Closed)
}

async fn release_remote_keys(tx: &tokio::sync::mpsc::Sender<Message>) {
    for keysym in [
        key::CONTROL_L,
        key::CONTROL_R,
        key::ALT_L,
        key::ALT_R,
        key::ESCAPE,
    ] {
        let _ = tx
            .send(Message::Input(InputEvent::Key(KeyEvent {
                action: KeyAction::Release,
                keysym,
                hid: 0,
                native: 0,
                modifiers: Modifiers::empty(),
            })))
            .await;
    }
}

fn return_to_local(
    inject: &mut dyn Inject,
    layout: &Layout,
    entry: Direction,
    remote_pos: (i32, i32),
    remote_screen: ScreenInfo,
) -> Result<()> {
    inject.set_remote_mode(false)?;
    let exit = entry.opposite();
    let ratio = ratio_along(
        exit,
        remote_pos.0,
        remote_pos.1,
        remote_screen.width.max(1) as i32,
        remote_screen.height.max(1) as i32,
    );
    if let Some(crossing) = layout.cross("remote", exit, ratio) {
        inject.warp(crossing.x, crossing.y)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------------

async fn cmd_connect(explicit: Option<&Path>, args: ConnectArgs) -> Result<()> {
    let (config, path) = Config::load_or_create(explicit)?;
    let _identity = Identity::load_or_create(identity_dir(&path))?;

    let addr = if let Some(name) = &args.name {
        println!(
            "searching for '{name}' on the LAN ({}s) ...",
            args.discover_timeout
        );
        let timeout = Duration::from_secs(args.discover_timeout);
        let peers = tokio::task::spawn_blocking(move || browse(timeout))
            .await
            .context("the mDNS discovery task failed")??;
        let peer = peers
            .into_iter()
            .find(|peer| {
                peer.name.eq_ignore_ascii_case(name) || peer.id.eq_ignore_ascii_case(name)
            })
            .with_context(|| format!("no Alfone server named '{name}' found on the LAN"))?;
        let addr = peer
            .socket_addr()
            .with_context(|| format!("'{name}' did not announce a usable address"))?;
        println!(
            "found '{}' at {addr} (code {})",
            peer.name, peer.fingerprint
        );
        addr
    } else {
        let server_text = args
            .server
            .clone()
            .unwrap_or_else(|| config.client.server.clone());
        parse_endpoint(&server_text, DEFAULT_PORT)?
    };

    let expected = match &args.fingerprint {
        Some(text) => Some(
            parse_hex_fingerprint(text)
                .with_context(|| format!("invalid fingerprint: {text}"))?,
        ),
        None => {
            if config.client.fingerprint.is_empty() {
                None
            } else {
                Some(
                    parse_hex_fingerprint(&config.client.fingerprint)
                        .context("the saved server fingerprint is invalid")?,
                )
            }
        }
    };
    // A previously paired device still needs the per-session PIN unless the
    // server disabled pairing.
    let pin = args.pin.clone();

    let mut inject = create_inject().context("creating the input injection backend")?;
    let screen = inject.screen();

    let auto_reconnect = if args.no_reconnect {
        false
    } else {
        args.reconnect || config.client.auto_reconnect
    };

    let mut expected = expected;
    let mut delay = Duration::from_secs(1);
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());

    loop {
        println!("connecting to {addr} ...");
        let connecting = connect(
            addr,
            ConnectOptions {
                expected_fingerprint: expected,
                pin: pin.clone(),
                ..ConnectOptions::default()
            },
            device_info(&config),
            capabilities(&config),
            SessionMode::Kvm,
        );
        tokio::pin!(connecting);

        let connected = tokio::select! {
            result = &mut connecting => result,
            _ = &mut ctrl_c => {
                println!("bye");
                return Ok(());
            }
        };

        match connected {
            Ok(peer) => {
                // TOFU: pin the fingerprint observed on the first connection so
                // later reconnects cannot be redirected to another server.
                if expected.is_none() {
                    expected = Some(peer.fingerprint);
                    println!(
                        "pinned server fingerprint for this run: {}",
                        hex(&peer.fingerprint)
                    );
                }
                delay = Duration::from_secs(1);
                match run_client_session(peer, &mut *inject, screen).await {
                    Ok(SessionEnd::Shutdown) => {
                        println!("disconnected");
                        return Ok(());
                    }
                    Ok(SessionEnd::Closed) => println!("server disconnected"),
                    Err(e) => warn!("session ended: {e:#}"),
                }
            }
            Err(e) => {
                if !auto_reconnect {
                    return Err(anyhow::Error::new(e))
                        .context("could not connect (check the address, firewall and PIN)");
                }
                warn!("connection failed: {e:#}");
            }
        }

        if !auto_reconnect {
            println!("disconnected");
            return Ok(());
        }
        println!("reconnecting in {}s (Ctrl+C to quit) ...", delay.as_secs());
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = &mut ctrl_c => {
                println!("bye");
                return Ok(());
            }
        }
        delay = (delay * 2).min(Duration::from_secs(10));
    }
}

/// Runs one client session. Returns `Shutdown` when the user pressed Ctrl+C
/// and `Closed` when the server went away (the caller may reconnect).
async fn run_client_session(
    peer: Peer,
    inject: &mut dyn Inject,
    screen: ScreenInfo,
) -> Result<SessionEnd> {
    let Peer {
        mut link,
        device,
        fingerprint,
        ..
    } = peer;
    println!("connected to '{}' ({:?})", device.name, device.os);
    println!(
        "server fingerprint: {} (short code {})",
        hex(&fingerprint),
        alfone_net::short_code(&fingerprint)
    );
    println!(
        "screen: {}x{}; this window can be controlled by the server",
        screen.width, screen.height
    );

    let tx = link.sender();
    tx.send(Message::Displays(DisplayInfo::single(
        screen.width,
        screen.height,
    )))
    .await?;

    let mut left_sent = false;
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            message = link.recv() => {
                let Some(message) = message else { return Ok(SessionEnd::Closed) };
                match message {
                    Message::Input(event) => {
                        inject.inject(&event)?;
                        if let InputEvent::PointerRel { dx, dy } = event {
                            if !left_sent {
                                let (x, y) = inject.pointer()?;
                                if edge_exit(x, y, dx, dy, screen) {
                                    tx.send(Message::LeaveScreen(LeaveScreen { sequence: 0 })).await?;
                                    left_sent = true;
                                }
                            }
                        }
                    }
                    Message::EnterScreen(EnterScreen { x, y, .. }) => {
                        inject.warp(x, y)?;
                        left_sent = false;
                    }
                    Message::LeaveScreen(_) => {}
                    Message::Ping { id } => {
                        tx.send(Message::Pong { id }).await?;
                    }
                    received @ (Message::Displays(_) | Message::Layout(_)) => {
                        debug!("received {}", received.kind());
                    }
                    Message::Mode(mode) => {
                        info!("session mode: {mode:?}");
                    }
                    Message::Disconnect { reason } => {
                        println!("server disconnected: {reason}");
                        return Ok(SessionEnd::Closed);
                    }
                    Message::Error { code, message } => {
                        warn!("server error {code}: {message}");
                        return Ok(SessionEnd::Closed);
                    }
                    other => debug!("ignoring {}", other.kind()),
                }
            }
            _ = &mut ctrl_c => {
                let _ = tx.send(Message::Disconnect {
                    reason: "client shutting down".into(),
                }).await;
                return Ok(SessionEnd::Shutdown);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bridge_capture(handle: CaptureHandle) -> tokio::sync::mpsc::Receiver<alfone_input::CapturedEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(512);
    std::thread::Builder::new()
        .name("alfone-capture-bridge".into())
        .spawn(move || {
            while let Ok(event) = handle.events.recv() {
                if tx.blocking_send(event).is_err() {
                    break;
                }
            }
        })
        .expect("spawning capture bridge");
    rx
}

fn device_info(config: &Config) -> DeviceInfo {
    DeviceInfo::new(config.device_id.clone(), config.device_name.clone())
}

fn capabilities(config: &Config) -> Capabilities {
    Capabilities {
        input: true,
        clipboard: config.clipboard,
        audio: config.audio.enabled,
        video: config.video.enabled,
        file_transfer: false,
    }
}

fn identity_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("identity")
}

fn parse_endpoint(text: &str, default_port: u16) -> Result<SocketAddr> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let candidate = if text.contains(':') {
        text.to_string()
    } else {
        format!("{text}:{default_port}")
    };
    if let Ok(addr) = candidate.parse::<SocketAddr>() {
        return Ok(addr);
    }
    // Allow hostnames; this blocks briefly, which is fine at startup.
    use std::net::ToSocketAddrs;
    candidate
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .with_context(|| format!("cannot resolve '{text}' (expected host:port)"))
}

fn primary_display(info: &DisplayInfo) -> Option<&alfone_protocol::Display> {
    info.displays
        .iter()
        .find(|display| display.id == info.primary)
        .or_else(|| info.displays.first())
}

fn build_layout(
    local: ScreenInfo,
    remote: ScreenInfo,
    side: Direction,
    local_name: &str,
    remote_name: &str,
) -> Layout {
    let mut local_screen = ScreenConfig::new(
        "local",
        local_name,
        local.width.max(1),
        local.height.max(1),
    );
    let mut remote_screen = ScreenConfig::new(
        "remote",
        remote_name,
        remote.width.max(1),
        remote.height.max(1),
    );

    match side {
        Direction::Right => {
            local_screen.right = Some("remote".into());
            remote_screen.left = Some("local".into());
        }
        Direction::Left => {
            local_screen.left = Some("remote".into());
            remote_screen.right = Some("local".into());
        }
        Direction::Top => {
            local_screen.top = Some("remote".into());
            remote_screen.bottom = Some("local".into());
        }
        Direction::Bottom => {
            local_screen.bottom = Some("remote".into());
            remote_screen.top = Some("local".into());
        }
    }

    Layout::new(&LayoutConfig {
        screens: vec![local_screen, remote_screen],
        primary: "local".into(),
    })
    .expect("generated layout is valid")
}

/// Which border, if any, the pointer is touching. X11 cannot report motion past
/// the screen border, so touching it is enough to start a crossing.
fn edge_at(x: i32, y: i32, width: i32, height: i32) -> Option<Direction> {
    if x <= 0 {
        Some(Direction::Left)
    } else if x >= width - 1 {
        Some(Direction::Right)
    } else if y <= 0 {
        Some(Direction::Top)
    } else if y >= height - 1 {
        Some(Direction::Bottom)
    } else {
        None
    }
}

fn near_border(x: i32, y: i32, width: i32, height: i32) -> bool {
    const MARGIN: i32 = 2;
    x <= MARGIN || y <= MARGIN || x >= width - 1 - MARGIN || y >= height - 1 - MARGIN
}

/// Client side: did this relative motion try to leave the screen?
fn edge_exit(x: i32, y: i32, dx: i32, dy: i32, screen: ScreenInfo) -> bool {
    const MARGIN: i32 = 2;
    let w = screen.width.max(1) as i32;
    let h = screen.height.max(1) as i32;
    (x <= MARGIN && dx < 0)
        || (x >= w - 1 - MARGIN && dx > 0)
        || (y <= MARGIN && dy < 0)
        || (y >= h - 1 - MARGIN && dy > 0)
}

/// Position along the shared edge, as a ratio in `0..=1`.
fn ratio_along(direction: Direction, x: i32, y: i32, width: i32, height: i32) -> f32 {
    let value = match direction {
        Direction::Left | Direction::Right => y as f32 / height.max(1) as f32,
        Direction::Top | Direction::Bottom => x as f32 / width.max(1) as f32,
    };
    value.clamp(0.0, 1.0)
}


