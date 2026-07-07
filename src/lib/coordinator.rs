use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rand::thread_rng;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

use crate::error::Result;
use crate::metainfo::MetaInfo;
use crate::peer::{run_peer, CmdMsg, PeerId, PeerMsg};
use crate::piece::{Bitfield, BlockRequest, PieceError, PieceManager, PieceState};
use crate::storage::Storage;

pub const MAX_INFLIGHT_PER_PEER: usize = 12;
pub const MAX_DOWNLOADING_PEERS: usize = 8;
pub const UNCHOKE_SLOTS: usize = 3;
pub const CHOKE_INTERVAL: Duration = Duration::from_secs(10);
pub const OPTIMISTIC_EVERY: u32 = 3; // every 30s = 3 choke ticks

struct PeerState {
    cmd_tx: mpsc::Sender<CmdMsg>,
    bitfield: Bitfield,
    peer_choking: bool,
    peer_interested: bool,
    am_choking: bool,
    downloaded_window: u64,
    /// Outstanding block requests: (index, begin).
    inflight: HashSet<(u32, u32)>,
    assigned_piece: Option<u32>,
}

pub struct Coordinator {
    meta: MetaInfo,
    pieces: PieceManager,
    storage: Storage,
    peers: HashMap<PeerId, PeerState>,
    next_peer_id: PeerId,
    our_peer_id: [u8; 20],
    listen_port: u16,
    peer_tx: mpsc::Sender<PeerMsg>,
    peer_rx: mpsc::Receiver<PeerMsg>,
    choke_ticks: u32,
    optimistic_unchoke: Option<PeerId>,
    uploaded: u64,
}

impl Coordinator {
    pub fn new(
        meta: MetaInfo,
        output_dir: &Path,
        our_peer_id: [u8; 20],
        listen_port: u16,
    ) -> Result<Self> {
        let pieces = PieceManager::new(
            meta.info.piece_length,
            meta.total_length,
            meta.info.piece_hashes.clone(),
        );
        let storage = Storage::new(&meta, output_dir)?;
        let (peer_tx, peer_rx) = mpsc::channel(1024);
        Ok(Self {
            meta,
            pieces,
            storage,
            peers: HashMap::new(),
            next_peer_id: 1,
            our_peer_id,
            listen_port,
            peer_tx,
            peer_rx,
            choke_ticks: 0,
            optimistic_unchoke: None,
            uploaded: 0,
        })
    }

    pub fn downloaded(&self) -> u64 {
        self.pieces.downloaded_bytes()
    }

    pub fn uploaded(&self) -> u64 {
        self.uploaded
    }

    fn spawn_peer(&mut self, stream: TcpStream) {
        let id = self.next_peer_id;
        self.next_peer_id += 1;
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        self.peers.insert(
            id,
            PeerState {
                cmd_tx: cmd_tx.clone(),
                bitfield: Bitfield::new(self.meta.num_pieces),
                peer_choking: true,
                peer_interested: false,
                am_choking: true,
                downloaded_window: 0,
                inflight: HashSet::new(),
                assigned_piece: None,
            },
        );
        let info_hash = self.meta.info_hash;
        let our_peer_id = self.our_peer_id;
        let num_pieces = self.meta.num_pieces;
        let peer_tx = self.peer_tx.clone();
        tokio::spawn(async move {
            run_peer(id, stream, info_hash, our_peer_id, num_pieces, cmd_rx, peer_tx).await;
        });
        let bits = self.pieces.have_bitfield().as_bytes().to_vec();
        let _ = cmd_tx.try_send(CmdMsg::SendBitfield(bits));
    }

    pub async fn run(mut self, peer_addrs: Vec<SocketAddr>, max_peers: usize) -> Result<()> {
        let listener = TcpListener::bind(("0.0.0.0", self.listen_port)).await?;
        eprintln!(
            "listening on {}, dialing up to {max_peers} of {} peers",
            listener.local_addr()?,
            peer_addrs.len()
        );

        let mut pending = peer_addrs;
        pending.truncate(max_peers);
        let mut dial_idx = 0usize;
        let mut in_flight_dials = 0usize;
        const MAX_DIALS: usize = 20;

        let (conn_tx, mut conn_rx) = mpsc::channel::<TcpStream>(max_peers.max(1));

        let spawn_dial = |addr: SocketAddr, tx: mpsc::Sender<TcpStream>| {
            tokio::spawn(async move {
                if let Ok(Ok(stream)) =
                    tokio::time::timeout(Duration::from_secs(8), TcpStream::connect(addr)).await
                {
                    let _ = tx.send(stream).await;
                }
            });
        };

        while dial_idx < pending.len() && in_flight_dials < MAX_DIALS {
            spawn_dial(pending[dial_idx], conn_tx.clone());
            dial_idx += 1;
            in_flight_dials += 1;
        }

        let mut choke_tick = interval(CHOKE_INTERVAL);
        choke_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut progress_tick = interval(Duration::from_secs(1));

        let start = Instant::now();
        let mut last_have = 0usize;

        loop {
            if self.pieces.is_complete() {
                eprintln!(
                    "download complete in {:.1}s → {}",
                    start.elapsed().as_secs_f64(),
                    self.storage.root().display()
                );
                for peer in self.peers.values() {
                    let _ = peer.cmd_tx.try_send(CmdMsg::Shutdown);
                }
                return Ok(());
            }

            tokio::select! {
                accept = listener.accept() => {
                    if let Ok((stream, _)) = accept {
                        if self.peers.len() < max_peers {
                            self.spawn_peer(stream);
                        }
                    }
                }
                stream = conn_rx.recv() => {
                    in_flight_dials = in_flight_dials.saturating_sub(1);
                    if let Some(stream) = stream {
                        if self.peers.len() < max_peers {
                            self.spawn_peer(stream);
                        }
                    }
                    while dial_idx < pending.len()
                        && in_flight_dials < MAX_DIALS
                        && self.peers.len() + in_flight_dials < max_peers
                    {
                        spawn_dial(pending[dial_idx], conn_tx.clone());
                        dial_idx += 1;
                        in_flight_dials += 1;
                    }
                }
                msg = self.peer_rx.recv() => {
                    match msg {
                        None => return Ok(()),
                        Some(msg) => self.handle_peer_msg(msg),
                    }
                }
                _ = choke_tick.tick() => {
                    self.run_choke_algorithm();
                }
                _ = progress_tick.tick() => {
                    let (have, total) = self.pieces.progress();
                    if have != last_have {
                        let bytes = self.pieces.downloaded_bytes();
                        let elapsed = start.elapsed().as_secs_f64().max(0.001);
                        let rate = bytes as f64 / elapsed / (1024.0 * 1024.0);
                        eprintln!(
                            "progress: {have}/{total} pieces ({:.1} MiB, {:.2} MiB/s, {} peers)",
                            bytes as f64 / (1024.0 * 1024.0),
                            rate,
                            self.peers.len()
                        );
                        last_have = have;
                    }
                }
            }
        }
    }

    fn handle_peer_msg(&mut self, msg: PeerMsg) {
        match msg {
            PeerMsg::Connected { id, .. } => {
                if let Some(peer) = self.peers.get(&id) {
                    let _ = peer.cmd_tx.try_send(CmdMsg::Interested);
                }
            }
            PeerMsg::Disconnected { id } => {
                if let Some(peer) = self.peers.remove(&id) {
                    self.pieces.on_peer_disconnect(&peer.bitfield);
                    if let Some(piece) = peer.assigned_piece {
                        self.release_piece_if_orphaned(piece);
                    }
                }
                self.fill_requests();
            }
            PeerMsg::Bitfield { id, bits } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    self.pieces.on_peer_bitfield(&bits);
                    peer.bitfield = bits;
                    let _ = peer.cmd_tx.try_send(CmdMsg::Interested);
                }
                self.fill_requests();
            }
            PeerMsg::Have { id, index } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.bitfield.set(index as usize);
                    self.pieces.on_peer_have(index);
                    let _ = peer.cmd_tx.try_send(CmdMsg::Interested);
                }
                self.fill_requests();
            }
            PeerMsg::Choked { id } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_choking = true;
                    peer.inflight.clear();
                    if let Some(piece) = peer.assigned_piece.take() {
                        self.release_piece_if_orphaned(piece);
                    }
                }
                self.fill_requests();
            }
            PeerMsg::Unchoked { id } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_choking = false;
                }
                self.fill_requests();
            }
            PeerMsg::Interested { id } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_interested = true;
                }
            }
            PeerMsg::NotInterested { id } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_interested = false;
                }
            }
            PeerMsg::BlockReceived {
                id,
                index,
                begin,
                data,
            } => {
                let len = data.len() as u64;
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.inflight.remove(&(index, begin));
                    peer.downloaded_window += len;
                }
                match self.pieces.on_block(index, begin, &data) {
                    Ok(Some(piece_data)) => {
                        if let Err(e) = self.storage.write_piece(index, &piece_data) {
                            eprintln!("write piece {index}: {e}");
                        }
                        for peer in self.peers.values_mut() {
                            if peer.assigned_piece == Some(index) {
                                peer.assigned_piece = None;
                                peer.inflight.retain(|&(i, _)| i != index);
                            }
                        }
                        for peer in self.peers.values() {
                            let _ = peer.cmd_tx.try_send(CmdMsg::Have(index));
                        }
                    }
                    Ok(None) => {}
                    Err(PieceError::HashMismatch { index }) => {
                        eprintln!("hash mismatch piece {index}, re-downloading");
                        for peer in self.peers.values_mut() {
                            if peer.assigned_piece == Some(index) {
                                peer.assigned_piece = None;
                                peer.inflight.clear();
                            }
                        }
                    }
                    Err(PieceError::BadBlock) => {
                        eprintln!("bad block from peer {id}");
                    }
                }
                self.fill_requests();
            }
            PeerMsg::RequestFromPeer {
                id,
                index,
                begin,
                length,
            } => {
                let am_choking = self.peers.get(&id).map(|p| p.am_choking).unwrap_or(true);
                if am_choking || !self.pieces.has_piece(index) {
                    return;
                }
                match self.storage.read_block(index, begin, length) {
                    Ok(block) => {
                        self.uploaded += block.len() as u64;
                        if let Some(peer) = self.peers.get(&id) {
                            let _ = peer.cmd_tx.try_send(CmdMsg::SendPiece {
                                index,
                                begin,
                                block,
                            });
                        }
                    }
                    Err(e) => eprintln!("read block for upload: {e}"),
                }
            }
        }
    }

    fn release_piece_if_orphaned(&mut self, piece: u32) {
        let still = self
            .peers
            .values()
            .any(|p| p.assigned_piece == Some(piece));
        if !still && self.pieces.state(piece) == PieceState::InFlight {
            self.pieces.abandon_piece(piece);
        }
    }

    fn downloading_peer_count(&self) -> usize {
        self.peers
            .values()
            .filter(|p| !p.peer_choking && (!p.inflight.is_empty() || p.assigned_piece.is_some()))
            .count()
    }

    fn fill_requests(&mut self) {
        let mut candidates: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| !p.peer_choking)
            .map(|(&id, _)| id)
            .collect();

        candidates.sort_by_key(|id| {
            let p = &self.peers[id];
            (p.inflight.is_empty(), p.assigned_piece.is_none())
        });

        for id in candidates {
            let downloading = self.downloading_peer_count();
            let (is_active, choking) = match self.peers.get(&id) {
                Some(p) => (
                    !p.inflight.is_empty() || p.assigned_piece.is_some(),
                    p.peer_choking,
                ),
                None => continue,
            };
            if choking {
                continue;
            }
            if !is_active && downloading >= MAX_DOWNLOADING_PEERS {
                continue;
            }

            let cmds = self.plan_requests_for(id);
            if let Some(peer) = self.peers.get_mut(&id) {
                for cmd in cmds {
                    if let CmdMsg::RequestBlock { index, begin, .. } = &cmd {
                        peer.inflight.insert((*index, *begin));
                    }
                    let _ = peer.cmd_tx.try_send(cmd);
                }
            }
        }
    }

    fn plan_requests_for(&mut self, id: PeerId) -> Vec<CmdMsg> {
        let cmds = Vec::new();
        let (room, bitfield, assigned, inflight) = {
            let peer = match self.peers.get(&id) {
                Some(p) => p,
                None => return cmds,
            };
            let room = MAX_INFLIGHT_PER_PEER.saturating_sub(peer.inflight.len());
            if room == 0 {
                return cmds;
            }
            (
                room,
                peer.bitfield.clone(),
                peer.assigned_piece,
                peer.inflight.clone(),
            )
        };

        let mut piece = assigned;
        if piece.is_none() {
            piece = self.pieces.pick_piece(&bitfield);
            if let Some(p) = piece {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.assigned_piece = Some(p);
                }
            }
        }
        let Some(index) = piece else {
            return cmds;
        };

        if self.pieces.has_piece(index) {
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.assigned_piece = None;
            }
            return cmds;
        }

        // Re-claim if the piece was abandoned back to Missing while we still hold it.
        if self.pieces.state(index) == PieceState::Missing {
            self.pieces.reclaim_piece(index);
        }

        self.plan_blocks(id, index, room, &inflight)
    }

    fn plan_blocks(
        &mut self,
        id: PeerId,
        index: u32,
        room: usize,
        inflight: &HashSet<(u32, u32)>,
    ) -> Vec<CmdMsg> {
        let needed = self.pieces.blocks_still_needed(index);
        let to_request: Vec<BlockRequest> = needed
            .into_iter()
            .filter(|b| !inflight.contains(&(b.index, b.begin)))
            .take(room)
            .collect();

        if to_request.is_empty() {
            let peer_inflight = inflight.iter().any(|(i, _)| *i == index);
            if !peer_inflight {
                // Nothing left for us on this piece; free the slot.
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.assigned_piece = None;
                }
                self.release_piece_if_orphaned(index);
            }
            return vec![];
        }

        to_request
            .into_iter()
            .map(|b| CmdMsg::RequestBlock {
                index: b.index,
                begin: b.begin,
                length: b.length,
            })
            .collect()
    }

    fn run_choke_algorithm(&mut self) {
        self.choke_ticks += 1;
        let do_optimistic = self.choke_ticks % OPTIMISTIC_EVERY == 0;

        let mut ranked: Vec<(PeerId, u64)> = self
            .peers
            .iter()
            .filter(|(_, p)| p.peer_interested)
            .map(|(&id, p)| (id, p.downloaded_window))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));

        let mut unchoke: HashSet<PeerId> = ranked
            .iter()
            .take(UNCHOKE_SLOTS)
            .map(|(id, _)| *id)
            .collect();

        if do_optimistic {
            let choked: Vec<PeerId> = self
                .peers
                .iter()
                .filter(|(id, p)| p.peer_interested && !unchoke.contains(id))
                .map(|(&id, _)| id)
                .collect();
            if let Some(&pick) = choked.choose(&mut thread_rng()) {
                self.optimistic_unchoke = Some(pick);
                unchoke.insert(pick);
            }
        } else if let Some(opt) = self.optimistic_unchoke {
            if self.peers.contains_key(&opt) {
                unchoke.insert(opt);
            }
        }

        for (id, peer) in self.peers.iter_mut() {
            let should_unchoke = unchoke.contains(id);
            if should_unchoke && peer.am_choking {
                peer.am_choking = false;
                let _ = peer.cmd_tx.try_send(CmdMsg::Unchoke);
            } else if !should_unchoke && !peer.am_choking {
                peer.am_choking = true;
                let _ = peer.cmd_tx.try_send(CmdMsg::Choke);
            }
            peer.downloaded_window = 0;
        }
    }
}
