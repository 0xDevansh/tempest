use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::tracker::{parse_compact_peers, AnnounceEvent};

const PROTOCOL_ID: u64 = 0x4172_7101_980;
const ACTION_CONNECT: u32 = 0;
const ACTION_ANNOUNCE: u32 = 1;
const MAX_ATTEMPTS: u32 = 3;

fn retry_wait(attempt: u32) -> Duration {
    // 3s, 6s, 12s
    Duration::from_secs(3u64 << attempt.min(2))
}

pub async fn announce(
    announce_url: &str,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    port: u16,
    downloaded: u64,
    left: u64,
    uploaded: u64,
    event: AnnounceEvent,
) -> Result<Vec<SocketAddr>> {
    let host_port = announce_url
        .strip_prefix("udp://")
        .ok_or_else(|| Error::Tracker(format!("not a udp url: {announce_url}")))?;
    let host_port = host_port.split('/').next().unwrap_or(host_port);
    let addrs: Vec<SocketAddr> = host_port
        .to_socket_addrs()
        .map_err(|e| Error::Tracker(format!("resolve {host_port}: {e}")))?
        .collect();
    // Prefer IPv4 — dual-stack bind mismatches are a common source of EINVAL/EAFNOSUPPORT.
    let addr = addrs
        .iter()
        .copied()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first().copied())
        .ok_or_else(|| Error::Tracker(format!("no addrs for {host_port}")))?;

    let bind = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = UdpSocket::bind(bind)
        .await
        .map_err(|e| Error::Tracker(e.to_string()))?;
    sock.connect(addr)
        .await
        .map_err(|e| Error::Tracker(e.to_string()))?;

    let connection_id = connect(&sock).await?;
    announce_with(
        &sock,
        connection_id,
        info_hash,
        peer_id,
        port,
        downloaded,
        left,
        uploaded,
        event,
    )
    .await
}

async fn connect(sock: &UdpSocket) -> Result<u64> {
    for attempt in 0..MAX_ATTEMPTS {
        let transaction_id: u32 = rand::random();
        let mut req = [0u8; 16];
        req[0..8].copy_from_slice(&PROTOCOL_ID.to_be_bytes());
        req[8..12].copy_from_slice(&ACTION_CONNECT.to_be_bytes());
        req[12..16].copy_from_slice(&transaction_id.to_be_bytes());

        sock.send(&req)
            .await
            .map_err(|e| Error::Tracker(e.to_string()))?;

        let wait = retry_wait(attempt);
        let mut buf = [0u8; 16];
        match timeout(wait, sock.recv(&mut buf)).await {
            Ok(Ok(n)) if n >= 16 => {
                let action = u32::from_be_bytes(buf[0..4].try_into().unwrap());
                let tid = u32::from_be_bytes(buf[4..8].try_into().unwrap());
                if action == ACTION_CONNECT && tid == transaction_id {
                    return Ok(u64::from_be_bytes(buf[8..16].try_into().unwrap()));
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(Error::Tracker(e.to_string())),
            Err(_) => continue, // timeout, retry
        }
    }
    Err(Error::Tracker("udp connect timed out".into()))
}

async fn announce_with(
    sock: &UdpSocket,
    connection_id: u64,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    port: u16,
    downloaded: u64,
    left: u64,
    uploaded: u64,
    event: AnnounceEvent,
) -> Result<Vec<SocketAddr>> {
    let key: u32 = rand::random();

    for attempt in 0..MAX_ATTEMPTS {
        let transaction_id: u32 = rand::random();
        let mut req = [0u8; 98];
        req[0..8].copy_from_slice(&connection_id.to_be_bytes());
        req[8..12].copy_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        req[12..16].copy_from_slice(&transaction_id.to_be_bytes());
        req[16..36].copy_from_slice(info_hash);
        req[36..56].copy_from_slice(peer_id);
        req[56..64].copy_from_slice(&downloaded.to_be_bytes());
        req[64..72].copy_from_slice(&left.to_be_bytes());
        req[72..80].copy_from_slice(&uploaded.to_be_bytes());
        req[80..84].copy_from_slice(&event.as_udp().to_be_bytes());
        // ip = 0 (default)
        req[84..88].copy_from_slice(&0u32.to_be_bytes());
        req[88..92].copy_from_slice(&key.to_be_bytes());
        // num_want = -1 (default)
        req[92..96].copy_from_slice(&(-1i32 as u32).to_be_bytes());
        req[96..98].copy_from_slice(&port.to_be_bytes());

        sock.send(&req)
            .await
            .map_err(|e| Error::Tracker(e.to_string()))?;

        let wait = retry_wait(attempt);
        let mut buf = vec![0u8; 2048];
        match timeout(wait, sock.recv(&mut buf)).await {
            Ok(Ok(n)) if n >= 20 => {
                let action = u32::from_be_bytes(buf[0..4].try_into().unwrap());
                let tid = u32::from_be_bytes(buf[4..8].try_into().unwrap());
                if action == 3 {
                    let msg = String::from_utf8_lossy(&buf[8..n]).into_owned();
                    return Err(Error::Tracker(format!("udp tracker error: {msg}")));
                }
                if action == ACTION_ANNOUNCE && tid == transaction_id {
                    return Ok(parse_compact_peers(&buf[20..n]));
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(Error::Tracker(e.to_string())),
            Err(_) => continue,
        }
    }
    Err(Error::Tracker("udp announce timed out".into()))
}
