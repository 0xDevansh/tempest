use std::collections::HashMap;

use rand::seq::SliceRandom;
use rand::thread_rng;
use sha1::{Digest, Sha1};

pub const BLOCK_SIZE: u32 = 16_384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitfield {
    bits: Vec<u8>,
    num_pieces: usize,
}

impl Bitfield {
    pub fn new(num_pieces: usize) -> Self {
        let nbytes = num_pieces.div_ceil(8);
        Self {
            bits: vec![0; nbytes],
            num_pieces,
        }
    }

    pub fn from_bytes(bits: Vec<u8>, num_pieces: usize) -> Self {
        let mut bf = Self { bits, num_pieces };
        // Clear unused trailing bits so count() is accurate.
        if num_pieces % 8 != 0 {
            let mask = 0xFFu8 << (8 - (num_pieces % 8));
            if let Some(last) = bf.bits.last_mut() {
                *last &= mask;
            }
        }
        bf
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn has(&self, index: usize) -> bool {
        if index >= self.num_pieces {
            return false;
        }
        let byte = self.bits[index / 8];
        (byte & (1 << (7 - (index % 8)))) != 0
    }

    pub fn set(&mut self, index: usize) {
        if index >= self.num_pieces {
            return;
        }
        self.bits[index / 8] |= 1 << (7 - (index % 8));
    }

    pub fn clear(&mut self, index: usize) {
        if index >= self.num_pieces {
            return;
        }
        self.bits[index / 8] &= !(1 << (7 - (index % 8)));
    }

    pub fn count(&self) -> usize {
        (0..self.num_pieces).filter(|&i| self.has(i)).count()
    }

    pub fn num_pieces(&self) -> usize {
        self.num_pieces
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceState {
    Missing,
    InFlight,
    Have,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRequest {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

struct PartialPiece {
    buf: Vec<u8>,
    received: Bitfield,
    blocks_left: u32,
}

pub struct PieceManager {
    piece_length: u64,
    total_length: u64,
    hashes: Vec<[u8; 20]>,
    state: Vec<PieceState>,
    availability: Vec<u16>,
    partial: HashMap<u32, PartialPiece>,
    have_bitfield: Bitfield,
    /// used to prefer random selection for the first few
    completed_count: usize,
}

impl PieceManager {
    pub fn new(piece_length: u64, total_length: u64, hashes: Vec<[u8; 20]>) -> Self {
        let num_pieces = hashes.len();
        Self {
            piece_length,
            total_length,
            hashes,
            state: vec![PieceState::Missing; num_pieces],
            availability: vec![0; num_pieces],
            partial: HashMap::new(),
            have_bitfield: Bitfield::new(num_pieces),
            completed_count: 0,
        }
    }

    pub fn num_pieces(&self) -> usize {
        self.hashes.len()
    }

    pub fn have_bitfield(&self) -> &Bitfield {
        &self.have_bitfield
    }

    pub fn is_complete(&self) -> bool {
        self.completed_count == self.hashes.len()
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.completed_count, self.hashes.len())
    }

    pub fn downloaded_bytes(&self) -> u64 {
        let mut bytes = 0u64;
        for i in 0..self.hashes.len() {
            if self.state[i] == PieceState::Have {
                bytes += self.piece_len(i as u32) as u64;
            }
        }
        bytes
    }

    pub fn piece_len(&self, index: u32) -> u32 {
        let index = index as usize;
        let start = index as u64 * self.piece_length;
        let end = (start + self.piece_length).min(self.total_length);
        (end - start) as u32
    }

    pub fn block_count(&self, index: u32) -> u32 {
        let len = self.piece_len(index);
        len.div_ceil(BLOCK_SIZE)
    }

    pub fn on_peer_bitfield(&mut self, peer_has: &Bitfield) {
        for i in 0..self.num_pieces() {
            if peer_has.has(i) {
                self.availability[i] = self.availability[i].saturating_add(1);
            }
        }
    }

    pub fn on_peer_have(&mut self, index: u32) {
        let i = index as usize;
        if i < self.availability.len() {
            self.availability[i] = self.availability[i].saturating_add(1);
        }
    }

    pub fn on_peer_disconnect(&mut self, peer_has: &Bitfield) {
        for i in 0..self.num_pieces() {
            if peer_has.has(i) {
                self.availability[i] = self.availability[i].saturating_sub(1);
            }
        }
    }

    /// Rarest-first among `Missing` pieces the peer has. Random among ties.
    /// For the first few pieces, pick randomly among available Missing pieces.
    pub fn pick_piece(&mut self, peer_has: &Bitfield) -> Option<u32> {
        let mut candidates: Vec<u32> = (0..self.num_pieces() as u32)
            .filter(|&i| {
                self.state[i as usize] == PieceState::Missing && peer_has.has(i as usize)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }

        let mut rng = thread_rng();
        // Random-first for the opening pieces to get a Have out quickly.
        if self.completed_count < 4 {
            candidates.shuffle(&mut rng);
            let picked = candidates[0];
            self.state[picked as usize] = PieceState::InFlight;
            return Some(picked);
        }

        let min_avail = candidates
            .iter()
            .map(|&i| self.availability[i as usize])
            .min()
            .unwrap_or(0);
        candidates.retain(|&i| self.availability[i as usize] == min_avail);
        candidates.shuffle(&mut rng);
        let picked = candidates[0];
        self.state[picked as usize] = PieceState::InFlight;
        Some(picked)
    }

    /// Release an InFlight piece back to Missing, keeping any partial blocks so a
    /// later peer can resume.
    pub fn abandon_piece(&mut self, index: u32) {
        let i = index as usize;
        if i < self.state.len() && self.state[i] == PieceState::InFlight {
            self.state[i] = PieceState::Missing;
        }
    }

    /// Mark a Missing piece as InFlight without rarest-first selection.
    pub fn reclaim_piece(&mut self, index: u32) {
        let i = index as usize;
        if i < self.state.len() && self.state[i] == PieceState::Missing {
            self.state[i] = PieceState::InFlight;
        }
    }

    /// Next outstanding block requests for a piece, up to `max`.
    pub fn next_blocks(&mut self, index: u32, max: usize) -> Vec<BlockRequest> {
        if max == 0 {
            return vec![];
        }
        let piece_len = self.piece_len(index);
        let nblocks = self.block_count(index) as usize;

        let partial = self.partial.entry(index).or_insert_with(|| PartialPiece {
            buf: vec![0; piece_len as usize],
            received: Bitfield::new(nblocks),
            blocks_left: nblocks as u32,
        });

        let mut out = Vec::new();
        for bi in 0..nblocks {
            if out.len() >= max {
                break;
            }
            if partial.received.has(bi) {
                continue;
            }
            // Mark as "requested" by setting the bit optimistically? No — only mark on
            // receipt. Track in-flight separately in the coordinator. Here we just
            // enumerate missing blocks; the coordinator must not re-request the same
            // block while it's outstanding. So we need a "requested" set.
            //
            // Simpler approach: clear bits mean not-yet-received. Coordinator tracks
            // which begins are in-flight per peer. This method returns blocks that
            // haven't been received yet; caller filters against its own in-flight set.
            let begin = bi as u32 * BLOCK_SIZE;
            let length = (piece_len - begin).min(BLOCK_SIZE);
            out.push(BlockRequest {
                index,
                begin,
                length,
            });
        }
        out
    }

    /// Mark a block as still needed after a failed/cancelled request so it can be
    /// re-requested. (No-op on the bitfield since we only set bits on receipt.)
    pub fn blocks_still_needed(&self, index: u32) -> Vec<BlockRequest> {
        let piece_len = self.piece_len(index);
        let nblocks = self.block_count(index) as usize;
        let received = self.partial.get(&index).map(|p| &p.received);
        let mut out = Vec::new();
        for bi in 0..nblocks {
            if received.is_some_and(|r| r.has(bi)) {
                continue;
            }
            let begin = bi as u32 * BLOCK_SIZE;
            let length = (piece_len - begin).min(BLOCK_SIZE);
            out.push(BlockRequest {
                index,
                begin,
                length,
            });
        }
        out
    }

    /// Returns `Some(piece_bytes)` when the piece is complete and SHA-1 verified.
    /// Returns `Err(HashMismatch)` when full but corrupt (resets to Missing).
    /// Returns `Ok(None)` while still assembling.
    pub fn on_block(
        &mut self,
        index: u32,
        begin: u32,
        data: &[u8],
    ) -> Result<Option<Vec<u8>>, PieceError> {
        let piece_len = self.piece_len(index) as usize;
        if begin as usize + data.len() > piece_len {
            return Err(PieceError::BadBlock);
        }
        let nblocks = self.block_count(index) as usize;
        let block_index = (begin / BLOCK_SIZE) as usize;
        if block_index >= nblocks {
            return Err(PieceError::BadBlock);
        }

        let partial = self.partial.entry(index).or_insert_with(|| PartialPiece {
            buf: vec![0; piece_len],
            received: Bitfield::new(nblocks),
            blocks_left: nblocks as u32,
        });

        if partial.received.has(block_index) {
            return Ok(None); // duplicate
        }

        partial.buf[begin as usize..begin as usize + data.len()].copy_from_slice(data);
        partial.received.set(block_index);
        partial.blocks_left = partial.blocks_left.saturating_sub(1);

        if partial.blocks_left > 0 {
            return Ok(None);
        }

        let buf = self.partial.remove(&index).unwrap().buf;
        let mut hasher = Sha1::new();
        hasher.update(&buf);
        let digest: [u8; 20] = hasher.finalize().into();
        if digest != self.hashes[index as usize] {
            self.state[index as usize] = PieceState::Missing;
            return Err(PieceError::HashMismatch { index });
        }

        self.state[index as usize] = PieceState::Have;
        self.have_bitfield.set(index as usize);
        self.completed_count += 1;
        Ok(Some(buf))
    }

    pub fn has_piece(&self, index: u32) -> bool {
        self.state
            .get(index as usize)
            .copied()
            .unwrap_or(PieceState::Missing)
            == PieceState::Have
    }

    pub fn state(&self, index: u32) -> PieceState {
        self.state
            .get(index as usize)
            .copied()
            .unwrap_or(PieceState::Missing)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PieceError {
    BadBlock,
    HashMismatch { index: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitfield_get_set_count() {
        let mut bf = Bitfield::new(10);
        assert_eq!(bf.count(), 0);
        bf.set(0);
        bf.set(9);
        assert!(bf.has(0));
        assert!(bf.has(9));
        assert!(!bf.has(1));
        assert_eq!(bf.count(), 2);
    }

    #[test]
    fn rarest_first_picks_lowest_availability() {
        let hashes = vec![[0u8; 20]; 5];
        let mut pm = PieceManager::new(16_384, 16_384 * 5, hashes);
        // Pretend we've completed enough pieces to leave random-first mode.
        pm.completed_count = 4;
        pm.availability = vec![5, 1, 3, 1, 4];
        let mut peer = Bitfield::new(5);
        for i in 0..5 {
            peer.set(i);
        }
        // Both index 1 and 3 have availability 1; either is fine.
        let picked = pm.pick_piece(&peer).unwrap();
        assert!(picked == 1 || picked == 3);
        assert_eq!(pm.state[picked as usize], PieceState::InFlight);
    }

    #[test]
    fn piece_verification_accepts_correct_and_rejects_corrupt() {
        let data = vec![7u8; 16_384];
        let mut hasher = Sha1::new();
        hasher.update(&data);
        let hash: [u8; 20] = hasher.finalize().into();

        let mut pm = PieceManager::new(16_384, 16_384, vec![hash]);
        pm.state[0] = PieceState::InFlight;
        let result = pm.on_block(0, 0, &data).unwrap();
        assert!(result.is_some());
        assert!(pm.is_complete());

        let mut pm2 = PieceManager::new(16_384, 16_384, vec![hash]);
        pm2.state[0] = PieceState::InFlight;
        let mut bad = data.clone();
        bad[0] ^= 0xff;
        let err = pm2.on_block(0, 0, &bad).unwrap_err();
        assert_eq!(err, PieceError::HashMismatch { index: 0 });
        assert_eq!(pm2.state[0], PieceState::Missing);
    }
}
