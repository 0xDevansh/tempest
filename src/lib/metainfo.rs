use std::ops::Range;
use std::path::PathBuf;

use sha1::{Digest, Sha1};

use crate::bencoding::{Bencodable, ParsingError};

/// Errors produced while interpreting a `.torrent` file.
#[derive(Debug)]
pub enum MetaError {
    Bencode(ParsingError),
    /// A required key was missing or had the wrong type.
    MissingField(&'static str),
    /// `info` dict could not be located in the raw bytes (needed for info_hash).
    NoInfoDict,
    /// `pieces` length was not a multiple of 20.
    BadPieces,
}

impl From<ParsingError> for MetaError {
    fn from(e: ParsingError) -> Self {
        MetaError::Bencode(e)
    }
}

/// One file in a multi-file torrent.
#[derive(Debug, PartialEq)]
pub struct TorrentFile {
    pub length: u64,
    pub path: PathBuf,
}

/// A torrent is either one file or a directory of files.
#[derive(Debug, PartialEq)]
pub enum Layout {
    Single { length: u64 },
    Multi { files: Vec<TorrentFile> },
}

/// The contents of the `info` dictionary.
#[derive(Debug)]
pub struct Info {
    pub name: String,
    pub piece_length: u64,
    /// SHA-1 hash of every piece, in order.
    pub piece_hashes: Vec<[u8; 20]>,
    pub layout: Layout,
}

/// Everything parsed out of a `.torrent` file.
#[derive(Debug)]
pub struct MetaInfo {
    pub announce: String,
    /// BEP 12 tiers; empty if the torrent has no announce-list.
    pub announce_list: Vec<Vec<String>>,
    pub info: Info,
    /// SHA-1 over the *raw bytes* of the info dictionary.
    pub info_hash: [u8; 20],
    pub total_length: u64,
    pub num_pieces: usize,
}

impl MetaInfo {
    pub fn parse(raw: Vec<u8>) -> Result<MetaInfo, MetaError> {
        // Compute the info_hash from the original bytes before we touch the
        // decoded structure — re-encoding is not guaranteed to be byte-identical.
        let span = info_dict_span(&raw).ok_or(MetaError::NoInfoDict)?;
        let mut hasher = Sha1::new();
        hasher.update(&raw[span]);
        let info_hash: [u8; 20] = hasher.finalize().into();

        let root = Bencodable::decode(raw)?;

        let announce = root
            .get("announce")
            .and_then(|v| v.as_str())
            .ok_or(MetaError::MissingField("announce"))?;

        let announce_list = root
            .get("announce-list")
            .and_then(|v| v.as_list())
            .map(|tiers| {
                tiers
                    .iter()
                    .filter_map(|tier| tier.as_list())
                    .map(|urls| urls.iter().filter_map(|u| u.as_str()).collect())
                    .collect()
            })
            .unwrap_or_default();

        let info_val = root.get("info").ok_or(MetaError::MissingField("info"))?;
        let info = Info::from_bencodable(info_val)?;

        let total_length = match &info.layout {
            Layout::Single { length } => *length,
            Layout::Multi { files } => files.iter().map(|f| f.length).sum(),
        };
        let num_pieces = info.piece_hashes.len();

        Ok(MetaInfo {
            announce,
            announce_list,
            info,
            info_hash,
            total_length,
            num_pieces,
        })
    }
}

impl Info {
    fn from_bencodable(info: &Bencodable) -> Result<Info, MetaError> {
        let name = info
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(MetaError::MissingField("name"))?;

        let piece_length = info
            .get("piece length")
            .and_then(|v| v.as_int())
            .ok_or(MetaError::MissingField("piece length"))? as u64;

        let pieces = info
            .get("pieces")
            .and_then(|v| v.as_bytes())
            .ok_or(MetaError::MissingField("pieces"))?;
        if pieces.len() % 20 != 0 {
            return Err(MetaError::BadPieces);
        }
        let piece_hashes: Vec<[u8; 20]> = pieces
            .chunks_exact(20)
            .map(|c| c.try_into().unwrap())
            .collect();

        let layout = if let Some(files) = info.get("files").and_then(|v| v.as_list()) {
            let mut out = Vec::with_capacity(files.len());
            for f in files {
                let length = f
                    .get("length")
                    .and_then(|v| v.as_int())
                    .ok_or(MetaError::MissingField("files.length"))? as u64;
                let path_parts = f
                    .get("path")
                    .and_then(|v| v.as_list())
                    .ok_or(MetaError::MissingField("files.path"))?;
                let mut path = PathBuf::new();
                for part in path_parts {
                    path.push(part.as_str().ok_or(MetaError::MissingField("files.path"))?);
                }
                out.push(TorrentFile { length, path });
            }
            Layout::Multi { files: out }
        } else {
            let length = info
                .get("length")
                .and_then(|v| v.as_int())
                .ok_or(MetaError::MissingField("length"))? as u64;
            Layout::Single { length }
        };

        Ok(Info {
            name,
            piece_length,
            piece_hashes,
            layout,
        })
    }
}

/// Locate the byte range of the top-level `info` dictionary's *value* within the
/// raw torrent bytes, so its SHA-1 can be taken over the exact original bytes.
fn info_dict_span(raw: &[u8]) -> Option<Range<usize>> {
    let mut i = 0;
    if raw.get(i)? != &b'd' {
        return None;
    }
    i += 1;
    while raw.get(i)? != &b'e' {
        // Every dict key is a byte string.
        let key = read_string(raw, &mut i)?;
        let value_start = i;
        skip_value(raw, &mut i)?;
        if key == b"info" {
            return Some(value_start..i);
        }
    }
    None
}

/// Read a bencoded byte string at `i`, advancing past it; returns the contents.
fn read_string<'a>(raw: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    let colon = raw[*i..].iter().position(|&b| b == b':')? + *i;
    let len: usize = std::str::from_utf8(&raw[*i..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    let end = start + len;
    if end > raw.len() {
        return None;
    }
    *i = end;
    Some(&raw[start..end])
}

/// Advance `i` past exactly one bencoded value.
fn skip_value(raw: &[u8], i: &mut usize) -> Option<()> {
    match raw.get(*i)? {
        b'i' => {
            let e = raw[*i..].iter().position(|&b| b == b'e')? + *i;
            *i = e + 1;
            Some(())
        }
        b'l' | b'd' => {
            *i += 1;
            while raw.get(*i)? != &b'e' {
                skip_value(raw, i)?;
            }
            *i += 1;
            Some(())
        }
        b'0'..=b'9' => {
            read_string(raw, i)?;
            Some(())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_info_hash_matches_known_value() {
        let raw = fs::read("test.torrent").expect("test.torrent present");
        let meta = MetaInfo::parse(raw).expect("parse");
        let hex: String = meta.info_hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c");
    }

    #[test]
    fn test_metadata_fields() {
        let raw = fs::read("test.torrent").expect("test.torrent present");
        let meta = MetaInfo::parse(raw).expect("parse");
        assert_eq!(meta.info.name, "Big Buck Bunny");
        assert_eq!(meta.info.piece_length, 262144);
        assert_eq!(meta.num_pieces, 1055);
        assert!(matches!(meta.info.layout, Layout::Multi { .. }));
        assert_eq!(meta.total_length, 276445467);
    }

    #[test]
    fn test_skip_value_nested() {
        let raw = b"d3:fooli1ei2ee3:bari42ee".to_vec();
        let span = info_dict_span(&raw);
        // no "info" key present
        assert!(span.is_none());
    }
}
