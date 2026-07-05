use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
    Cancel {
        index: u32,
        begin: u32,
        length: u32,
    },
    Port(u16),
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();
        self.write_to(&mut buf);
        buf.to_vec()
    }

    fn write_to(&self, buf: &mut BytesMut) {
        match self {
            Message::KeepAlive => {
                buf.put_u32(0);
            }
            Message::Choke => {
                buf.put_u32(1);
                buf.put_u8(0);
            }
            Message::Unchoke => {
                buf.put_u32(1);
                buf.put_u8(1);
            }
            Message::Interested => {
                buf.put_u32(1);
                buf.put_u8(2);
            }
            Message::NotInterested => {
                buf.put_u32(1);
                buf.put_u8(3);
            }
            Message::Have(index) => {
                buf.put_u32(5);
                buf.put_u8(4);
                buf.put_u32(*index);
            }
            Message::Bitfield(bits) => {
                buf.put_u32(1 + bits.len() as u32);
                buf.put_u8(5);
                buf.extend_from_slice(bits);
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                buf.put_u32(13);
                buf.put_u8(6);
                buf.put_u32(*index);
                buf.put_u32(*begin);
                buf.put_u32(*length);
            }
            Message::Piece {
                index,
                begin,
                block,
            } => {
                buf.put_u32(9 + block.len() as u32);
                buf.put_u8(7);
                buf.put_u32(*index);
                buf.put_u32(*begin);
                buf.extend_from_slice(block);
            }
            Message::Cancel {
                index,
                begin,
                length,
            } => {
                buf.put_u32(13);
                buf.put_u8(8);
                buf.put_u32(*index);
                buf.put_u32(*begin);
                buf.put_u32(*length);
            }
            Message::Port(port) => {
                buf.put_u32(3);
                buf.put_u8(9);
                buf.put_u16(*port);
            }
        }
    }

    pub fn decode(payload_with_id: &[u8]) -> Result<Message> {
        if payload_with_id.is_empty() {
            return Ok(Message::KeepAlive);
        }
        let id = payload_with_id[0];
        let rest = &payload_with_id[1..];
        match id {
            0 => Ok(Message::Choke),
            1 => Ok(Message::Unchoke),
            2 => Ok(Message::Interested),
            3 => Ok(Message::NotInterested),
            4 => {
                if rest.len() < 4 {
                    return Err(Error::Peer("short have".into()));
                }
                Ok(Message::Have(u32::from_be_bytes(rest[0..4].try_into().unwrap())))
            }
            5 => Ok(Message::Bitfield(rest.to_vec())),
            6 | 8 => {
                if rest.len() < 12 {
                    return Err(Error::Peer("short request/cancel".into()));
                }
                let index = u32::from_be_bytes(rest[0..4].try_into().unwrap());
                let begin = u32::from_be_bytes(rest[4..8].try_into().unwrap());
                let length = u32::from_be_bytes(rest[8..12].try_into().unwrap());
                if id == 6 {
                    Ok(Message::Request {
                        index,
                        begin,
                        length,
                    })
                } else {
                    Ok(Message::Cancel {
                        index,
                        begin,
                        length,
                    })
                }
            }
            7 => {
                if rest.len() < 8 {
                    return Err(Error::Peer("short piece".into()));
                }
                let index = u32::from_be_bytes(rest[0..4].try_into().unwrap());
                let begin = u32::from_be_bytes(rest[4..8].try_into().unwrap());
                Ok(Message::Piece {
                    index,
                    begin,
                    block: rest[8..].to_vec(),
                })
            }
            9 => {
                if rest.len() < 2 {
                    return Err(Error::Peer("short port".into()));
                }
                Ok(Message::Port(u16::from_be_bytes(rest[0..2].try_into().unwrap())))
            }
            _ => Err(Error::Peer(format!("unknown message id {id}"))),
        }
    }
}

pub struct MessageCodec;

impl Decoder for MessageCodec {
    type Item = Message;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> std::result::Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;
        if src.len() < 4 + length {
            return Ok(None);
        }
        src.advance(4);
        let payload = src.split_to(length);
        Message::decode(&payload).map(Some)
    }
}

impl Encoder<Message> for MessageCodec {
    type Error = Error;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> std::result::Result<(), Self::Error> {
        item.write_to(dst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_messages() {
        let msgs = vec![
            Message::KeepAlive,
            Message::Choke,
            Message::Unchoke,
            Message::Interested,
            Message::NotInterested,
            Message::Have(42),
            Message::Bitfield(vec![0b1010_0000, 0x00]),
            Message::Request {
                index: 1,
                begin: 0,
                length: 16384,
            },
            Message::Piece {
                index: 1,
                begin: 0,
                block: vec![1, 2, 3, 4],
            },
            Message::Cancel {
                index: 1,
                begin: 0,
                length: 16384,
            },
            Message::Port(6881),
        ];
        for m in msgs {
            let enc = m.encode();
            // Strip length prefix for decode helper, or use codec.
            let len = u32::from_be_bytes(enc[0..4].try_into().unwrap()) as usize;
            let decoded = Message::decode(&enc[4..4 + len]).unwrap();
            assert_eq!(decoded, m);
        }
    }
}
