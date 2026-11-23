//! Wire format shared by every Alfone component.
//!
//! This crate is deliberately dependency-light: no async runtime, no
//! platform code. That keeps the protocol testable and lets the CLI, the
//! daemon and (later) a mobile client share exactly the same types.

pub mod codec;
pub mod key;
pub mod media;
pub mod message;

pub use codec::{
    announced_len, decode_body, decode_frame, encode_body, encode_frame, CodecError,
    MAX_FRAME_BYTES,
};
pub use message::*;

/// Protocol version. Bump the major number for breaking changes.
pub const PROTOCOL_MAJOR: u16 = 10;
pub const PROTOCOL_MINOR: u16 = 0;
pub const PROTOCOL_VERSION: u16 = PROTOCOL_MAJOR * 100 + PROTOCOL_MINOR;

/// Returns true when a peer speaking `remote` can talk to us.
///
/// Minor versions are backward compatible; a different major version is not.
pub fn is_compatible(remote: u16) -> bool {
    remote / 100 == PROTOCOL_MAJOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_compatible() {
        assert_eq!(PROTOCOL_VERSION, 1000);
        assert!(is_compatible(PROTOCOL_VERSION));
        assert!(is_compatible(1007));
        assert!(!is_compatible(1100));
        assert!(!is_compatible(2000));
    }
}
