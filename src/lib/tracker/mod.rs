mod http;
mod udp;

use std::net::SocketAddr;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::metainfo::MetaInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceEvent {
    None,
    Started,
    Completed,
    Stopped,
}

impl AnnounceEvent {
    pub fn as_http_str(self) -> Option<&'static str> {
        match self {
            AnnounceEvent::None => None,
            AnnounceEvent::Started => Some("started"),
            AnnounceEvent::Completed => Some("completed"),
            AnnounceEvent::Stopped => Some("stopped"),
        }
    }

    pub fn as_udp(self) -> u32 {
        match self {
            AnnounceEvent::None => 0,
            AnnounceEvent::Completed => 1,
            AnnounceEvent::Started => 2,
            AnnounceEvent::Stopped => 3,
        }
    }
}

/// Announce to trackers in BEP 12 tier order; return peers from the first that answers.
/// Within a tier, URLs are raced in parallel.
pub async fn announce(
    meta: &MetaInfo,
    peer_id: &[u8; 20],
    port: u16,
    downloaded: u64,
    uploaded: u64,
    event: AnnounceEvent,
) -> Result<Vec<SocketAddr>> {
    let left = meta.total_length.saturating_sub(downloaded);

    let tiers: Vec<Vec<String>> = if meta.announce_list.is_empty() {
        vec![vec![meta.announce.clone()]]
    } else {
        let mut tiers = meta.announce_list.clone();
        if !tiers
            .iter()
            .any(|t| t.iter().any(|u| u == &meta.announce))
        {
            tiers.insert(0, vec![meta.announce.clone()]);
        }
        tiers
    };

    let mut last_err = Error::Tracker("no usable trackers".into());

    for tier in tiers {
        let urls: Vec<String> = tier
            .into_iter()
            .filter(|u| {
                u.starts_with("udp://")
                    || u.starts_with("http://")
                    || u.starts_with("https://")
            })
            .collect();
        if urls.is_empty() {
            continue;
        }

        let mut handles = Vec::new();
        for url in urls {
            let info_hash = meta.info_hash;
            let peer_id = *peer_id;
            handles.push(tokio::spawn(async move {
                if url.starts_with("udp://") {
                    udp::announce(
                        &url,
                        &info_hash,
                        &peer_id,
                        port,
                        downloaded,
                        left,
                        uploaded,
                        event,
                    )
                    .await
                } else {
                    http::announce(
                        &url,
                        &info_hash,
                        &peer_id,
                        port,
                        downloaded,
                        left,
                        uploaded,
                        event,
                    )
                    .await
                }
            }));
        }

        // Wait until one returns a non-empty peer list, or all finish.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<SocketAddr>>>(handles.len());
        for h in handles {
            let tx = tx.clone();
            tokio::spawn(async move {
                match h.await {
                    Ok(res) => {
                        let _ = tx.send(res).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Error::Tracker(format!("join: {e}")))).await;
                    }
                }
            });
        }
        drop(tx);

        let deadline = tokio::time::sleep(Duration::from_secs(25));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = &mut deadline => break,
                msg = rx.recv() => {
                    match msg {
                        None => break,
                        Some(Ok(peers)) if !peers.is_empty() => return Ok(peers),
                        Some(Ok(_)) => {
                            last_err = Error::Tracker("tracker returned no peers".into());
                        }
                        Some(Err(e)) => {
                            last_err = e;
                        }
                    }
                }
            }
        }
    }

    Err(last_err)
}

/// Parse compact peer list: repeated 6-byte groups (IPv4 + port).
pub fn parse_compact_peers(data: &[u8]) -> Vec<SocketAddr> {
    data.chunks_exact(6)
        .map(|c| {
            let ip = std::net::Ipv4Addr::new(c[0], c[1], c[2], c[3]);
            let port = u16::from_be_bytes([c[4], c[5]]);
            SocketAddr::from((ip, port))
        })
        .filter(|a| a.port() != 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo::MetaInfo;
    use std::fs;

    #[test]
    fn parse_compact_peers_basic() {
        let data = [127, 0, 0, 1, 0x1A, 0xE1, 10, 0, 0, 2, 0x1A, 0xE0];
        let peers = parse_compact_peers(&data);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].to_string(), "127.0.0.1:6881");
        assert_eq!(peers[1].to_string(), "10.0.0.2:6880");
    }

    #[tokio::test]
    #[ignore = "network; run with: cargo test -- --ignored"]
    async fn announce_udp_returns_peers() {
        let raw = fs::read("test.torrent").expect("test.torrent present");
        let meta = MetaInfo::parse(raw).expect("parse");
        let mut peer_id = [0u8; 20];
        peer_id[..8].copy_from_slice(b"-TE0001-");
        let peers = announce(&meta, &peer_id, 6881, 0, 0, AnnounceEvent::Started)
            .await
            .expect("announce");
        assert!(!peers.is_empty(), "expected at least one peer");
    }
}
