use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::metainfo::{Layout, MetaInfo};

struct FileSpan {
    path: PathBuf,
    /// Absolute offset of this file within the torrent byte stream.
    offset: u64,
    length: u64,
}

pub struct Storage {
    files: Vec<FileSpan>,
    piece_length: u64,
    root: PathBuf,
}

impl Storage {
    pub fn new(meta: &MetaInfo, output_dir: &Path) -> Result<Self> {
        let root = output_dir.join(&meta.info.name);
        let mut files = Vec::new();
        let mut offset = 0u64;

        match &meta.info.layout {
            Layout::Single { length } => {
                if let Some(parent) = root.parent() {
                    fs::create_dir_all(parent)?;
                }
                preallocate(&root, *length)?;
                files.push(FileSpan {
                    path: root.clone(),
                    offset: 0,
                    length: *length,
                });
            }
            Layout::Multi { files: tfiles } => {
                fs::create_dir_all(&root)?;
                for tf in tfiles {
                    let path = root.join(&tf.path);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    preallocate(&path, tf.length)?;
                    files.push(FileSpan {
                        path,
                        offset,
                        length: tf.length,
                    });
                    offset += tf.length;
                }
            }
        }

        Ok(Self {
            files,
            piece_length: meta.info.piece_length,
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_piece(&self, index: u32, data: &[u8]) -> Result<()> {
        let start = index as u64 * self.piece_length;
        let end = start + data.len() as u64;
        let mut offset_in_data = 0usize;
        self.map_range(start, end, |path, file_off, take| {
            let mut file = open_rw(path)?;
            file.seek(SeekFrom::Start(file_off))?;
            let slice = &data[offset_in_data..offset_in_data + take as usize];
            file.write_all(slice)?;
            offset_in_data += take as usize;
            Ok(())
        })
    }

    pub fn read_block(&self, index: u32, begin: u32, length: u32) -> Result<Vec<u8>> {
        let start = index as u64 * self.piece_length + begin as u64;
        let end = start + length as u64;
        let mut out = vec![0u8; length as usize];
        let mut cursor = 0usize;
        self.map_range(start, end, |path, file_off, take| {
            let mut file = open_rw(path)?;
            file.seek(SeekFrom::Start(file_off))?;
            file.read_exact(&mut out[cursor..cursor + take as usize])?;
            cursor += take as usize;
            Ok(())
        })?;
        Ok(out)
    }

    fn span_at(&self, abs: u64) -> Result<(&FileSpan, u64)> {
        for span in &self.files {
            if abs >= span.offset && abs < span.offset + span.length {
                return Ok((span, abs - span.offset));
            }
        }
        Err(Error::Other(format!("byte offset {abs} outside torrent")))
    }

    fn map_range<F>(&self, start: u64, end: u64, mut op: F) -> Result<()>
    where
        F: FnMut(&Path, u64, u64) -> Result<()>,
    {
        let mut abs = start;
        while abs < end {
            let (span, file_off) = self.span_at(abs)?;
            let remaining = end - abs;
            let in_file = span.length - file_off;
            let take = remaining.min(in_file);
            op(&span.path, file_off, take)?;
            abs += take;
        }
        Ok(())
    }
}

fn open_rw(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

fn preallocate(path: &Path, length: u64) -> Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.set_len(length)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo::{Info, Layout, MetaInfo, TorrentFile};

    fn multi_meta() -> MetaInfo {
        MetaInfo {
            announce: "http://example.com/announce".into(),
            announce_list: vec![],
            info: Info {
                name: "testset".into(),
                piece_length: 16,
                piece_hashes: vec![[0u8; 20]; 2],
                layout: Layout::Multi {
                    files: vec![
                        TorrentFile {
                            length: 10,
                            path: PathBuf::from("a.txt"),
                        },
                        TorrentFile {
                            length: 14,
                            path: PathBuf::from("b.txt"),
                        },
                        TorrentFile {
                            length: 8,
                            path: PathBuf::from("c.txt"),
                        },
                    ],
                },
            },
            info_hash: [0u8; 20],
            total_length: 32,
            num_pieces: 2,
        }
    }

    #[test]
    fn write_piece_splits_across_files() {
        let dir = std::env::temp_dir().join(format!("tempest-storage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let meta = multi_meta();
        let storage = Storage::new(&meta, &dir).unwrap();

        let piece0: Vec<u8> = (0u8..16).collect();
        storage.write_piece(0, &piece0).unwrap();
        let piece1: Vec<u8> = (16u8..32).collect();
        storage.write_piece(1, &piece1).unwrap();

        let a = fs::read(dir.join("testset/a.txt")).unwrap();
        let b = fs::read(dir.join("testset/b.txt")).unwrap();
        let c = fs::read(dir.join("testset/c.txt")).unwrap();
        assert_eq!(a, (0u8..10).collect::<Vec<_>>());
        assert_eq!(b, (10u8..24).collect::<Vec<_>>());
        assert_eq!(c, (24u8..32).collect::<Vec<_>>());

        let block = storage.read_block(0, 8, 8).unwrap();
        assert_eq!(block, (8u8..16).collect::<Vec<_>>());

        let _ = fs::remove_dir_all(&dir);
    }
}
