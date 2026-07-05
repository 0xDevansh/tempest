use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

pub const PSTR: &[u8] = b"BitTorrent protocol";
pub const HANDSHAKE_LEN: usize = 68;

pub fn build_handshake(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> [u8; HANDSHAKE_LEN] {
    let mut buf = [0u8; HANDSHAKE_LEN];
    buf[0] = 19;
    buf[1..20].copy_from_slice(PSTR);
    // reserved bytes 20..28 already zero
    buf[28..48].copy_from_slice(info_hash);
    buf[48..68].copy_from_slice(peer_id);
    buf
}

pub fn parse_handshake(buf: &[u8; HANDSHAKE_LEN]) -> Result<([u8; 20], [u8; 20])> {
    if buf[0] != 19 || &buf[1..20] != PSTR {
        return Err(Error::Peer("bad handshake pstr".into()));
    }
    let mut info_hash = [0u8; 20];
    let mut peer_id = [0u8; 20];
    info_hash.copy_from_slice(&buf[28..48]);
    peer_id.copy_from_slice(&buf[48..68]);
    Ok((info_hash, peer_id))
}

pub async fn perform_handshake(
    stream: &mut TcpStream,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
) -> Result<[u8; 20]> {
    let ours = build_handshake(info_hash, peer_id);
    stream.write_all(&ours).await?;

    let mut theirs = [0u8; HANDSHAKE_LEN];
    stream.read_exact(&mut theirs).await?;
    let (their_hash, their_id) = parse_handshake(&theirs)?;
    if &their_hash != info_hash {
        return Err(Error::Peer("info_hash mismatch in handshake".into()));
    }
    Ok(their_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_round_trip() {
        let info = [1u8; 20];
        let pid = [2u8; 20];
        let buf = build_handshake(&info, &pid);
        let (h, p) = parse_handshake(&buf).unwrap();
        assert_eq!(h, info);
        assert_eq!(p, pid);
    }
}
