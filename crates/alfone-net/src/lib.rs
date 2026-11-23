//! QUIC transport, device identity and session handshake.
//!
//! Design notes:
//! - One bidirectional stream carries control messages (postcard, length
//!   prefixed). Input never blocks on clipboard/video traffic because those use
//!   separate streams or datagrams.
//! - QUIC datagrams carry audio and video fragments.
//! - TLS 1.3 with self-signed certificates. The client pins the server
//!   fingerprint or uses TOFU + a short code the user compares.
//! - A per-session PIN is required before the server accepts input.

mod identity;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;

use alfone_protocol::{
    decode_body, encode_frame, announced_len, Capabilities, DeviceInfo, Hello, HelloBack,
    Message, PairRequest, PairResponse, SessionMode, PROTOCOL_VERSION,
};
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

pub use identity::{
    fingerprint_of, hex, normalize_code, parse_hex_fingerprint, short_code, Identity,
};
pub use tls::ALPN;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("quic connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("quic connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("quic write: {0}")]
    Write(#[from] quinn::WriteError),
    #[error("quic read: {0}")]
    Read(#[from] quinn::ReadError),
    #[error("quic read_exact: {0}")]
    ReadExact(#[from] quinn::ReadExactError),
    #[error("datagram send: {0}")]
    DatagramSend(#[from] quinn::SendDatagramError),
    #[error("protocol codec: {0}")]
    Codec(#[from] alfone_protocol::CodecError),
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    #[error("certificate: {0}")]
    Cert(String),
    #[error("protocol version {0} is not compatible with {PROTOCOL_VERSION}")]
    Version(u16),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("pairing rejected: {0}")]
    PairingRejected(String),
    #[error("connection closed")]
    Closed,
}

pub type Result<T, E = NetError> = std::result::Result<T, E>;

/// A connected peer plus the control link.
#[derive(Debug)]
pub struct Peer {
    pub link: Link,
    pub device: DeviceInfo,
    pub fingerprint: [u8; 32],
    pub remote: SocketAddr,
    pub mode: SessionMode,
}

/// Bidirectional control link with a background reader and writer.
#[derive(Debug)]
pub struct Link {
    conn: quinn::Connection,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    tasks: Vec<JoinHandle<()>>,
    /// Keeps the client endpoint alive for the lifetime of the connection.
    _endpoint: Option<quinn::Endpoint>,
}

impl Link {
    fn start(
        conn: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        endpoint: Option<quinn::Endpoint>,
    ) -> Self {
        let (out_tx, mut out_rx) = mpsc::channel::<Message>(512);
        let (in_tx, in_rx) = mpsc::channel::<Message>(512);

        let mut send = send;
        let writer = tokio::spawn(async move {
            while let Some(message) = out_rx.recv().await {
                match write_message(&mut send, &message).await {
                    Ok(()) => {}
                    Err(e) => {
                        debug!("control writer stopped: {e}");
                        break;
                    }
                }
            }
            let _ = send.finish();
        });

        let mut recv = recv;
        let reader = tokio::spawn(async move {
            loop {
                match read_message(&mut recv).await {
                    Ok(Some(message)) => {
                        if in_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        debug!("control reader stopped: {e}");
                        break;
                    }
                }
            }
        });

        Self {
            conn,
            tx: out_tx,
            rx: in_rx,
            tasks: vec![writer, reader],
            _endpoint: endpoint,
        }
    }

    pub async fn send(&self, message: Message) -> Result<()> {
        self.tx.send(message).await.map_err(|_| NetError::Closed)
    }

    /// Cloneable handle for sending control messages from other tasks.
    ///
    /// Useful inside `tokio::select!`: the receive half borrows the link while
    /// the sender handle stays independent.
    pub fn sender(&self) -> mpsc::Sender<Message> {
        self.tx.clone()
    }

    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Option<Message> {
        self.rx.try_recv().ok()
    }

    pub fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    pub fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        self.conn.send_datagram(Bytes::copy_from_slice(payload))?;
        Ok(())
    }

    pub async fn read_datagram(&self) -> Result<Bytes> {
        Ok(self.conn.read_datagram().await?)
    }

    pub fn close(&self, reason: &str) {
        self.conn.close(0u32.into(), reason.as_bytes());
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.conn
            .close(0u32.into(), b"alfone: link dropped");
    }
}

/// QUIC server. Accepts one connection per call to [`Server::accept`].
pub struct Server {
    endpoint: quinn::Endpoint,
    identity: Arc<Identity>,
    pin: Option<String>,
}

impl Server {
    pub fn bind(identity: Identity, addr: SocketAddr, pin: Option<String>) -> Result<Self> {
        let config = tls::server_quinn_config(identity.cert_der.clone(), identity.key_der.clone())?;
        let endpoint = quinn::Endpoint::server(config, addr)?;
        Ok(Self {
            endpoint,
            identity: Arc::new(identity),
            pin,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn pin(&self) -> Option<&str> {
        self.pin.as_deref()
    }

    pub fn short_code(&self) -> String {
        self.identity.short_code()
    }

    /// Wait for, handshake and (optionally) pair with the next client.
    pub async fn accept(
        &self,
        device: DeviceInfo,
        capabilities: Capabilities,
        mode: SessionMode,
    ) -> Result<Peer> {
        let incoming = self.endpoint.accept().await.ok_or(NetError::Closed)?;
        let remote = incoming.remote_address();
        debug!(%remote, "incoming QUIC connection");
        let conn = incoming.await?;

        // The server opens the control stream: QUIC only announces a stream to
        // the peer once data is written, so opening on the client and waiting
        // for `accept_bi` before writing would deadlock.
        let (mut send, mut recv) = conn.open_bi().await?;
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            device,
            capabilities,
            mode,
            pairing_required: self.pin.is_some(),
            nonce: random_nonce(),
        };
        write_message(&mut send, &Message::Hello(hello)).await?;

        let hello_back = match read_message(&mut recv).await? {
            Some(Message::HelloBack(hello_back)) => hello_back,
            Some(other) => {
                return Err(NetError::Protocol(format!(
                    "expected HelloBack, got {}",
                    other.kind()
                )))
            }
            None => return Err(NetError::Closed),
        };

        if !alfone_protocol::is_compatible(hello_back.protocol) {
            let _ = write_message(
                &mut send,
                &Message::Error {
                    code: 1,
                    message: format!("unsupported protocol {}", hello_back.protocol),
                },
            )
            .await;
            finish_and_wait(&mut send, &conn).await;
            return Err(NetError::Version(hello_back.protocol));
        }

        if hello_back.server_fingerprint != self.identity.fingerprint {
            return Err(NetError::Protocol(
                "client verified a different server certificate".into(),
            ));
        }

        if let Some(expected_pin) = &self.pin {
            let request = match read_message(&mut recv).await? {
                Some(Message::PairRequest(request)) => request,
                Some(other) => {
                    return Err(NetError::Protocol(format!(
                        "expected PairRequest, got {}",
                        other.kind()
                    )))
                }
                None => return Err(NetError::Closed),
            };
            if normalize_code(&request.code) != normalize_code(expected_pin) {
                let _ = write_message(
                    &mut send,
                    &Message::PairResponse(PairResponse {
                        accepted: false,
                        reason: "incorrect PIN".into(),
                    }),
                )
                .await;
                finish_and_wait(&mut send, &conn).await;
                return Err(NetError::PairingRejected("incorrect PIN".into()));
            }
            write_message(
                &mut send,
                &Message::PairResponse(PairResponse {
                    accepted: true,
                    reason: String::new(),
                }),
            )
            .await?;
        }

        Ok(Peer {
            link: Link::start(conn, send, recv, None),
            device: hello_back.device,
            fingerprint: hello_back.server_fingerprint,
            remote,
            mode: hello_back.mode,
        })
    }

    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"alfone: server stopped");
    }
}

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// TLS server name. Self-signed certs do not need a real DNS name; use
    /// `alfone` unless the certificate has a matching SAN.
    pub server_name: String,
    /// Pin the server certificate. `None` = TOFU; compare the short code.
    pub expected_fingerprint: Option<[u8; 32]>,
    /// PIN displayed by the server, when pairing is enabled.
    pub pin: Option<String>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            server_name: "alfone".into(),
            expected_fingerprint: None,
            pin: None,
        }
    }
}

/// Connect to a server and complete the Alfone handshake.
pub async fn connect(
    addr: SocketAddr,
    options: ConnectOptions,
    device: DeviceInfo,
    capabilities: Capabilities,
    mode: SessionMode,
) -> Result<Peer> {
    let verifier = Arc::new(tls::FingerprintVerifier::new(options.expected_fingerprint));
    let config = tls::client_quinn_config(verifier.clone())?;

    let bind: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0".parse().expect("valid IPv4 bind")
    } else {
        "[::]:0".parse().expect("valid IPv6 bind")
    };
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(config);

    let conn = endpoint.connect(addr, &options.server_name)?.await?;
    let fingerprint = verifier
        .observed()
        .ok_or_else(|| NetError::Cert("server did not present a certificate".into()))?;

    let (mut send, mut recv) = conn.accept_bi().await?;
    let hello = match read_message(&mut recv).await? {
        Some(Message::Hello(hello)) => hello,
        Some(other) => {
            return Err(NetError::Protocol(format!(
                "expected Hello, got {}",
                other.kind()
            )))
        }
        None => return Err(NetError::Closed),
    };
    if !alfone_protocol::is_compatible(hello.protocol) {
        return Err(NetError::Version(hello.protocol));
    }

    let hello_back = HelloBack {
        protocol: PROTOCOL_VERSION,
        device,
        capabilities,
        mode,
        nonce: random_nonce(),
        server_fingerprint: fingerprint,
    };
    write_message(&mut send, &Message::HelloBack(hello_back.clone())).await?;

    if hello.pairing_required {
        let Some(pin) = options.pin else {
            return Err(NetError::PairingRejected(
                "server requires a PIN (use --pin)".into(),
            ));
        };
        write_message(
            &mut send,
            &Message::PairRequest(PairRequest {
                code: pin,
                device: hello_back.device.clone(),
            }),
        )
        .await?;
        match read_message(&mut recv).await? {
            Some(Message::PairResponse(response)) if response.accepted => {}
            Some(Message::PairResponse(response)) => {
                return Err(NetError::PairingRejected(response.reason))
            }
            Some(other) => {
                return Err(NetError::Protocol(format!(
                    "expected PairResponse, got {}",
                    other.kind()
                )))
            }
            None => return Err(NetError::Closed),
        }
    }

    Ok(Peer {
        link: Link::start(conn, send, recv, Some(endpoint)),
        device: hello.device,
        fingerprint,
        remote: addr,
        mode: hello.mode,
    })
}

/// Generates a 6-digit pairing PIN for one server session.
pub fn generate_pin() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

fn random_nonce() -> [u8; 16] {
    rand::random()
}

async fn write_message(send: &mut quinn::SendStream, message: &Message) -> Result<()> {
    let frame = encode_frame(message)?;
    send.write_all(&frame).await?;
    Ok(())
}

/// Deliver the final message before closing: finish the stream and give the
/// peer a moment to read it. Without this the connection drop can discard the
/// rejection reason.
async fn finish_and_wait(send: &mut quinn::SendStream, conn: &quinn::Connection) {
    let _ = send.finish();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), conn.closed()).await;
}

async fn read_message(recv: &mut quinn::RecvStream) -> Result<Option<Message>> {
    let mut header = [0u8; 4];
    match recv.read_exact(&mut header).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(quinn::ReadExactError::FinishedEarly(n)) => {
            return Err(NetError::Protocol(format!(
                "truncated frame header ({n} bytes)"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let len = announced_len(&header)?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok(Some(decode_body(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alfone_protocol::{InputEvent, Message};
    use std::time::Duration;

    #[tokio::test]
    async fn handshake_pairing_and_input_roundtrip() {
        let dir = std::env::temp_dir().join(format!("alfone-net-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = Identity::load_or_create(&dir).unwrap();
        let pin = "123456".to_string();

        let server = Server::bind(
            identity.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Some(pin.clone()),
        )
        .unwrap();
        let addr = server.local_addr().unwrap();
        let expected = identity.fingerprint;

        let server_task = tokio::spawn(async move {
            let peer = server
                .accept(
                    DeviceInfo::new("server-id", "server"),
                    Capabilities::full(),
                    SessionMode::Kvm,
                )
                .await
                .unwrap();
            let mut peer = peer;
            let message = peer.link.recv().await.unwrap();
            peer.link.send(Message::Pong { id: 99 }).await.unwrap();
            // Keep the link alive until the client acknowledges the pong,
            // otherwise the connection (and the queued message) is closed.
            let _ = peer.link.recv().await;
            (peer.device.name, message)
        });

        let options = ConnectOptions {
            server_name: "alfone".into(),
            expected_fingerprint: Some(expected),
            pin: Some(pin),
        };
        let mut client = connect(
            addr,
            options,
            DeviceInfo::new("client-id", "client"),
            Capabilities::input_only(),
            SessionMode::Kvm,
        )
        .await
        .unwrap();

        client
            .link
            .send(Message::Input(InputEvent::PointerAbs { x: 10, y: 20 }))
            .await
            .unwrap();

        let reply = tokio::time::timeout(Duration::from_secs(5), client.link.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply, Message::Pong { id: 99 });
        client
            .link
            .send(Message::Disconnect {
                reason: "test done".into(),
            })
            .await
            .unwrap();

        let (name, message) = server_task.await.unwrap();
        assert_eq!(name, "client");
        assert_eq!(
            message,
            Message::Input(InputEvent::PointerAbs { x: 10, y: 20 })
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wrong_pin_is_rejected() {
        let dir = std::env::temp_dir().join(format!("alfone-net-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = Identity::load_or_create(&dir).unwrap();
        let server = Server::bind(
            identity.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Some("111111".into()),
        )
        .unwrap();
        let addr = server.local_addr().unwrap();
        let task = tokio::spawn(async move {
            server
                .accept(
                    DeviceInfo::new("s", "s"),
                    Capabilities::input_only(),
                    SessionMode::Kvm,
                )
                .await
        });

        let result = connect(
            addr,
            ConnectOptions {
                expected_fingerprint: Some(identity.fingerprint),
                pin: Some("999999".into()),
                ..Default::default()
            },
            DeviceInfo::new("c", "c"),
            Capabilities::input_only(),
            SessionMode::Kvm,
        )
        .await;
        assert!(
            matches!(result, Err(NetError::PairingRejected(_))),
            "unexpected result: {result:?}"
        );
        let _ = task.await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
