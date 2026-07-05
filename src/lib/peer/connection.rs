use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::error::{Error, Result};
use crate::peer::handshake;
use crate::peer::message::{Message, MessageCodec};
use crate::piece::Bitfield;

/// Opaque handle for a connected peer (assigned by the coordinator).
pub type PeerId = u64;

/// Coordinator → peer task.
#[derive(Debug)]
pub enum CmdMsg {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    RequestBlock {
        index: u32,
        begin: u32,
        length: u32,
    },
    CancelBlock {
        index: u32,
        begin: u32,
        length: u32,
    },
    Have(u32),
    SendBitfield(Vec<u8>),
    /// Upload: send a piece block to the remote peer.
    SendPiece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
    Shutdown,
}

/// Peer task → coordinator.
#[derive(Debug)]
pub enum PeerMsg {
    Connected {
        id: PeerId,
        peer_id: [u8; 20],
    },
    Disconnected {
        id: PeerId,
    },
    Bitfield {
        id: PeerId,
        bits: Bitfield,
    },
    Have {
        id: PeerId,
        index: u32,
    },
    BlockReceived {
        id: PeerId,
        index: u32,
        begin: u32,
        data: Vec<u8>,
    },
    RequestFromPeer {
        id: PeerId,
        index: u32,
        begin: u32,
        length: u32,
    },
    Choked {
        id: PeerId,
    },
    Unchoked {
        id: PeerId,
    },
    Interested {
        id: PeerId,
    },
    NotInterested {
        id: PeerId,
    },
}

pub async fn run_peer(
    id: PeerId,
    mut stream: TcpStream,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    num_pieces: usize,
    mut cmd_rx: mpsc::Receiver<CmdMsg>,
    peer_tx: mpsc::Sender<PeerMsg>,
) {
    let result = async {
        let their_id = handshake::perform_handshake(&mut stream, &info_hash, &our_peer_id).await?;
        peer_tx
            .send(PeerMsg::Connected {
                id,
                peer_id: their_id,
            })
            .await
            .map_err(|_| Error::Peer("coordinator gone".into()))?;

        let mut framed = Framed::new(stream, MessageCodec);
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        None | Some(CmdMsg::Shutdown) => return Ok(()),
                        Some(CmdMsg::Choke) => framed.send(Message::Choke).await?,
                        Some(CmdMsg::Unchoke) => framed.send(Message::Unchoke).await?,
                        Some(CmdMsg::Interested) => framed.send(Message::Interested).await?,
                        Some(CmdMsg::NotInterested) => framed.send(Message::NotInterested).await?,
                        Some(CmdMsg::RequestBlock { index, begin, length }) => {
                            framed.send(Message::Request { index, begin, length }).await?;
                        }
                        Some(CmdMsg::CancelBlock { index, begin, length }) => {
                            framed.send(Message::Cancel { index, begin, length }).await?;
                        }
                        Some(CmdMsg::Have(index)) => {
                            framed.send(Message::Have(index)).await?;
                        }
                        Some(CmdMsg::SendBitfield(bits)) => {
                            framed.send(Message::Bitfield(bits)).await?;
                        }
                        Some(CmdMsg::SendPiece { index, begin, block }) => {
                            framed.send(Message::Piece { index, begin, block }).await?;
                        }
                    }
                }
                frame = framed.next() => {
                    match frame {
                        None => return Ok(()),
                        Some(Err(e)) => return Err(e),
                        Some(Ok(msg)) => {
                            handle_message(id, num_pieces, msg, &peer_tx).await?;
                        }
                    }
                }
            }
        }
    }
    .await;

    if let Err(e) = result {
        eprintln!("peer {id}: {e}");
    }
    let _ = peer_tx.send(PeerMsg::Disconnected { id }).await;
}

async fn handle_message(
    id: PeerId,
    num_pieces: usize,
    msg: Message,
    peer_tx: &mpsc::Sender<PeerMsg>,
) -> Result<()> {
    match msg {
        Message::KeepAlive | Message::Port(_) => {}
        Message::Choke => {
            peer_tx
                .send(PeerMsg::Choked { id })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Unchoke => {
            peer_tx
                .send(PeerMsg::Unchoked { id })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Interested => {
            peer_tx
                .send(PeerMsg::Interested { id })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::NotInterested => {
            peer_tx
                .send(PeerMsg::NotInterested { id })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Have(index) => {
            peer_tx
                .send(PeerMsg::Have { id, index })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Bitfield(bits) => {
            let bf = Bitfield::from_bytes(bits, num_pieces);
            peer_tx
                .send(PeerMsg::Bitfield { id, bits: bf })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Piece {
            index,
            begin,
            block,
        } => {
            peer_tx
                .send(PeerMsg::BlockReceived {
                    id,
                    index,
                    begin,
                    data: block,
                })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Request {
            index,
            begin,
            length,
        } => {
            peer_tx
                .send(PeerMsg::RequestFromPeer {
                    id,
                    index,
                    begin,
                    length,
                })
                .await
                .map_err(|_| Error::Peer("coordinator gone".into()))?;
        }
        Message::Cancel { .. } => {}
    }
    Ok(())
}
