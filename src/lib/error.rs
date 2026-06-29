use std::io;

use thiserror::Error;

use crate::bencoding::ParsingError;
use crate::metainfo::MetaError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("bencode error: {0:?}")]
    Bencode(ParsingError),
    #[error("metainfo error: {0:?}")]
    Metainfo(MetaError),
    #[error("tracker error: {0}")]
    Tracker(String),
    #[error("peer error: {0}")]
    Peer(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("hash mismatch for piece {index}")]
    HashMismatch { index: u32 },
    #[error("{0}")]
    Other(String),
}

impl From<ParsingError> for Error {
    fn from(e: ParsingError) -> Self {
        Error::Bencode(e)
    }
}

impl From<MetaError> for Error {
    fn from(e: MetaError) -> Self {
        Error::Metainfo(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
