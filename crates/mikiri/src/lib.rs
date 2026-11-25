//! Mikiri: daily US equity research and execution.

pub mod tiingo;

pub mod h001;
pub mod h002;
pub mod h003;
pub mod h004;

pub mod h005;

pub mod h006;

pub mod paper_recorder;

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex_lower(Sha256::digest(bytes.as_ref()))
}

/// Lowercase hex SHA-256 of a reader, hashed in chunks.
pub fn sha256_hex_reader(mut reader: impl std::io::Read) -> std::io::Result<String> {
    let mut hash = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex_lower(hash.finalize()))
}

fn hex_lower(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = digest.as_ref();
    let mut out = String::with_capacity(digest.len() * 2);
    for &b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
