use std::path::PathBuf;

use clap::Parser;
use rand::RngCore;

use tempest::coordinator::Coordinator;
use tempest::metainfo::MetaInfo;
use tempest::tracker::{self, AnnounceEvent};

#[derive(Parser, Debug)]
#[command(name = "tempest", about = "A BitTorrent CLI client")]
struct Args {
    /// Path to a .torrent file
    torrent: PathBuf,

    /// Directory to write downloaded files into
    #[arg(long, short = 'o', default_value = ".")]
    output: PathBuf,

    /// TCP port to listen on for incoming peers
    #[arg(long, default_value_t = 6881)]
    port: u16,

    /// Maximum number of simultaneous peer connections
    #[arg(long, default_value_t = 40)]
    max_peers: usize,
}

fn generate_peer_id() -> [u8; 20] {
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(b"-TE0001-");
    rand::thread_rng().fill_bytes(&mut id[8..]);
    id
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let raw = std::fs::read(&args.torrent)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", args.torrent.display()))?;
    let meta = MetaInfo::parse(raw).map_err(|e| anyhow::anyhow!("parse torrent: {e:?}"))?;

    eprintln!(
        "torrent: {} ({} pieces, {:.1} MiB)",
        meta.info.name,
        meta.num_pieces,
        meta.total_length as f64 / (1024.0 * 1024.0)
    );

    let peer_id = generate_peer_id();
    let peers = tracker::announce(
        &meta,
        &peer_id,
        args.port,
        0,
        0,
        AnnounceEvent::Started,
    )
    .await
    .map_err(|e| anyhow::anyhow!("announce: {e}"))?;

    eprintln!("got {} peers from tracker", peers.len());

    let coordinator = Coordinator::new(meta, &args.output, peer_id, args.port)?;
    coordinator.run(peers, args.max_peers).await?;
    Ok(())
}
