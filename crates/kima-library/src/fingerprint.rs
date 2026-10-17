//! Cheap content fingerprint: SHA-256 of (size as 8 LE bytes || first 64 KiB || last 64 KiB).

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK: u64 = 64 * 1024;

pub fn of(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = Sha256::new();
    hasher.update(size.to_le_bytes());

    let mut head = Vec::new();
    (&mut file).take(CHUNK).read_to_end(&mut head)?;
    hasher.update(&head);

    // Tail: the final 64 KiB, but never re-reading bytes already hashed in the head
    // (so a file under 128 KiB is hashed exactly once).
    let tail_start = size.saturating_sub(CHUNK).max(head.len() as u64);
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    hasher.update(&tail);

    Ok(hasher.finalize().into())
}
