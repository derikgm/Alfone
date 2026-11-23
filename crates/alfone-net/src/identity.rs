//! Device identity: a self-signed certificate and its SHA-256 fingerprint.
//!
//! Every Alfone device generates this once and keeps it. The fingerprint is the
//! long-term identity; the short code is what a human compares the first time.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::NetError;

pub const CERT_FILE: &str = "cert.der";
pub const KEY_FILE: &str = "key.der";

#[derive(Debug, Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

impl Identity {
    /// Load the identity from `dir`, generating it on first run.
    pub fn load_or_create(dir: impl AsRef<Path>) -> Result<Self, NetError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);

        let (cert_der, key_der) = if cert_path.exists() && key_path.exists() {
            (fs::read(&cert_path)?, fs::read(&key_path)?)
        } else {
            let generated = rcgen::generate_simple_self_signed(vec!["alfone".to_string()])
                .map_err(|e| NetError::Cert(e.to_string()))?;
            let cert_der = generated.cert.der().to_vec();
            let key_der = generated.key_pair.serialize_der();
            // key.der is a private key: restrict it on Unix.
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&key_path)?;
                file.write_all(&key_der)?;
                let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));
            }
            #[cfg(not(unix))]
            fs::write(&key_path, &key_der)?;
            fs::write(&cert_path, &cert_der)?;
            (cert_der, key_der)
        };

        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    pub fn fingerprint_hex(&self) -> String {
        hex(&self.fingerprint)
    }

    /// Eight characters designed to be read aloud and compared, e.g.
    /// `7Q2M-4XKD`.
    pub fn short_code(&self) -> String {
        short_code(&self.fingerprint)
    }

    pub fn cert_path(dir: impl AsRef<Path>) -> PathBuf {
        dir.as_ref().join(CERT_FILE)
    }
}

pub fn fingerprint_of(cert_der: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hasher.finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex_fingerprint(text: &str) -> Option<[u8; 32]> {
    let clean: String = text
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if clean.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Crockford base32 of the first 40 bits, grouped as `XXXX-XXXX`.
pub fn short_code(fingerprint: &[u8; 32]) -> String {
    let mut bits: u64 = 0;
    let mut available = 0u32;
    let mut out = String::with_capacity(9);
    for &byte in fingerprint.iter().take(5) {
        bits = (bits << 8) | u64::from(byte);
        available += 8;
        while available >= 5 {
            available -= 5;
            let index = ((bits >> available) & 0x1f) as usize;
            out.push(CODE_ALPHABET[index] as char);
        }
    }
    out.insert(4, '-');
    out
}

pub fn normalize_code(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip() {
        let fp = [0xabu8; 32];
        let hex = hex(&fp);
        assert_eq!(parse_hex_fingerprint(&hex), Some(fp));
        assert_eq!(parse_hex_fingerprint(&hex[..10]), None);
    }

    #[test]
    fn short_code_shape() {
        let fp = [0u8; 32];
        let code = short_code(&fp);
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert_eq!(normalize_code(&code).len(), 8);
    }

    #[test]
    fn identity_persists() {
        let dir = std::env::temp_dir().join(format!("alfone-identity-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.cert_der, b.cert_der);
        let _ = fs::remove_dir_all(&dir);
    }
}
