use std::net::SocketAddr;
use std::time::Duration;

use crate::bencoding::Bencodable;
use crate::error::{Error, Result};
use crate::tracker::{parse_compact_peers, AnnounceEvent};

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
    // info_hash and peer_id must be raw-byte URL-encoded, not UTF-8.
    let mut query = String::new();
    query.push_str("info_hash=");
    query.push_str(&percent_encode_bytes(info_hash));
    query.push_str("&peer_id=");
    query.push_str(&percent_encode_bytes(peer_id));
    query.push_str(&format!(
        "&port={port}&uploaded={uploaded}&downloaded={downloaded}&left={left}&compact=1&numwant=50"
    ));
    if let Some(ev) = event.as_http_str() {
        query.push_str("&event=");
        query.push_str(ev);
    }

    let sep = if announce_url.contains('?') { '&' } else { '?' };
    let url = format!("{announce_url}{sep}{query}");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::Tracker(e.to_string()))?;

    let bytes = client
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Tracker(e.to_string()))?
        .error_for_status()
        .map_err(|e| Error::Tracker(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| Error::Tracker(e.to_string()))?;

    let decoded =
        Bencodable::decode(bytes.to_vec()).map_err(|e| Error::Tracker(format!("{e:?}")))?;

    if let Some(msg) = decoded.get("failure reason").and_then(|v| v.as_str()) {
        return Err(Error::Tracker(msg));
    }

    let peers_val = decoded
        .get("peers")
        .ok_or_else(|| Error::Tracker("missing peers".into()))?;

    if let Some(bytes) = peers_val.as_bytes() {
        return Ok(parse_compact_peers(bytes));
    }

    // Non-compact: list of dicts with ip/port.
    if let Some(list) = peers_val.as_list() {
        let mut out = Vec::new();
        for p in list {
            let ip = p
                .get("ip")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tracker("peer missing ip".into()))?;
            let port = p
                .get("port")
                .and_then(|v| v.as_int())
                .ok_or_else(|| Error::Tracker("peer missing port".into()))? as u16;
            if let Ok(addr) = format!("{ip}:{port}").parse() {
                out.push(addr);
            }
        }
        return Ok(out);
    }

    Err(Error::Tracker("unrecognized peers format".into()))
}

fn percent_encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}
