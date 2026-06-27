mod lib;
use std::fs;

use crate::lib::bencoding;

fn main() {
    let bytes: Vec<u8> = fs::read("test.torrent").unwrap();
    let _ = bencoding::Bencodable::decode(bytes).unwrap();
}