//! The write-ahead log, in SQLite's own `-wal` file format
//! (<https://www.sqlite.org/walformat.html>).
//!
//! A WAL is a 32-byte header followed by frames; each frame is a 24-byte
//! header and one page image. A transaction is a run of frames whose last
//! frame records the database size in pages (non-zero marks a commit).
//! Every frame carries a checksum that chains from the frame before it, so
//! a torn or stale tail is detected and ignored: recovery is simply
//! "read frames until one fails to verify, keep everything up to the last
//! commit frame".

use crate::error::Result;
use crate::pager::Page;
use crate::storage::Storage;
use std::collections::HashMap;
use std::rc::Rc;

const WAL_HEADER: u64 = 32;
const FRAME_HEADER: usize = 24;
const MAGIC_LE: u32 = 0x377f_0682;
const MAGIC_BE: u32 = 0x377f_0683;
const WAL_VERSION: u32 = 3_007_000;

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// The WAL checksum: two running 32-bit sums over the data taken as 32-bit
/// words (byte order chosen by the magic number). `data.len()` must be a
/// multiple of 8.
fn checksum(big_endian: bool, data: &[u8], seed: (u32, u32)) -> (u32, u32) {
    let (mut s0, mut s1) = seed;
    for chunk in data.chunks_exact(8) {
        let (a, b) = if big_endian {
            (be32(chunk, 0), be32(chunk, 4))
        } else {
            (
                u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
            )
        };
        s0 = s0.wrapping_add(a).wrapping_add(s1);
        s1 = s1.wrapping_add(b).wrapping_add(s0);
    }
    (s0, s1)
}

pub struct Wal {
    storage: Box<dyn Storage>,
    page_size: u32,
    /// Newest committed frame (0-based) for each page.
    index: HashMap<u32, u32>,
    /// Number of valid frames that belong to committed transactions.
    n_frames: u32,
    /// Database size recorded by the last commit frame.
    db_size: Option<u32>,
    big_endian: bool,
    salt: (u32, u32),
    /// Running checksum after the last committed frame.
    cksum: (u32, u32),
    ckpt_seq: u32,
    /// Whether the storage starts with a header matching the fields above.
    has_header: bool,
}

impl Wal {
    /// Open a WAL and recover its committed frames.
    pub fn open(storage: Box<dyn Storage>, page_size: u32) -> Result<Wal> {
        let mut wal = Wal {
            storage,
            page_size,
            index: HashMap::new(),
            n_frames: 0,
            db_size: None,
            big_endian: false,
            salt: (0, 0),
            cksum: (0, 0),
            ckpt_seq: 0,
            has_header: false,
        };
        wal.recover()?;
        Ok(wal)
    }

    fn frame_size(&self) -> u64 {
        FRAME_HEADER as u64 + self.page_size as u64
    }

    fn recover(&mut self) -> Result<()> {
        let len = self.storage.len()?;
        let mut hdr = [0u8; WAL_HEADER as usize];
        if len < WAL_HEADER || self.storage.read_at(0, &mut hdr)? < hdr.len() {
            return Ok(());
        }
        let magic = be32(&hdr, 0);
        if magic != MAGIC_LE && magic != MAGIC_BE {
            return Ok(());
        }
        let big_endian = magic == MAGIC_BE;
        if be32(&hdr, 4) != WAL_VERSION || be32(&hdr, 8) != self.page_size {
            return Ok(());
        }
        let ck = checksum(big_endian, &hdr[..24], (0, 0));
        if ck != (be32(&hdr, 24), be32(&hdr, 28)) {
            return Ok(());
        }
        self.big_endian = big_endian;
        self.ckpt_seq = be32(&hdr, 12);
        self.salt = (be32(&hdr, 16), be32(&hdr, 20));
        self.cksum = ck;
        self.has_header = true;

        // Walk the frames. Entries of an unfinished transaction stay in
        // `pending` and are dropped if no commit frame follows.
        let mut pending: Vec<(u32, u32)> = Vec::new();
        let mut running = ck;
        let mut frame = vec![0u8; self.frame_size() as usize];
        let mut n: u32 = 0;
        loop {
            let off = WAL_HEADER + n as u64 * self.frame_size();
            if off + self.frame_size() > len {
                break;
            }
            if self.storage.read_at(off, &mut frame)? < frame.len() {
                break;
            }
            if (be32(&frame, 8), be32(&frame, 12)) != self.salt {
                break;
            }
            let pgno = be32(&frame, 0);
            if pgno == 0 {
                break;
            }
            let c = checksum(big_endian, &frame[..8], running);
            let c = checksum(big_endian, &frame[FRAME_HEADER..], c);
            if c != (be32(&frame, 16), be32(&frame, 20)) {
                break;
            }
            running = c;
            pending.push((pgno, n));
            n += 1;
            let commit_size = be32(&frame, 4);
            if commit_size != 0 {
                for (p, f) in pending.drain(..) {
                    self.index.insert(p, f);
                }
                self.n_frames = n;
                self.db_size = Some(commit_size);
                self.cksum = running;
            }
        }
        Ok(())
    }

    /// Database size in pages after the last committed transaction in the
    /// WAL, or `None` if the WAL holds no committed frames.
    pub fn committed_db_size(&self) -> Option<u32> {
        self.db_size
    }

    pub fn frame_count(&self) -> usize {
        self.n_frames as usize
    }

    /// Page numbers that have at least one committed frame, ascending.
    pub fn pages(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.index.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// The newest committed image of `pgno`, if the WAL has one.
    pub fn read_page(&self, pgno: u32) -> Result<Option<Page>> {
        let Some(&frame) = self.index.get(&pgno) else {
            return Ok(None);
        };
        let off = WAL_HEADER + frame as u64 * self.frame_size() + FRAME_HEADER as u64;
        let mut buf = vec![0u8; self.page_size as usize];
        self.storage.read_at(off, &mut buf)?;
        Ok(Some(Rc::from(buf)))
    }

    fn write_header(&mut self) -> Result<()> {
        self.ckpt_seq = self.ckpt_seq.wrapping_add(1);
        self.salt = (self.salt.0.wrapping_add(1), new_salt(self.salt.1));
        self.big_endian = false;
        let mut hdr = [0u8; WAL_HEADER as usize];
        hdr[0..4].copy_from_slice(&MAGIC_LE.to_be_bytes());
        hdr[4..8].copy_from_slice(&WAL_VERSION.to_be_bytes());
        hdr[8..12].copy_from_slice(&self.page_size.to_be_bytes());
        hdr[12..16].copy_from_slice(&self.ckpt_seq.to_be_bytes());
        hdr[16..20].copy_from_slice(&self.salt.0.to_be_bytes());
        hdr[20..24].copy_from_slice(&self.salt.1.to_be_bytes());
        let ck = checksum(false, &hdr[..24], (0, 0));
        hdr[24..28].copy_from_slice(&ck.0.to_be_bytes());
        hdr[28..32].copy_from_slice(&ck.1.to_be_bytes());
        self.storage.truncate(0)?;
        self.storage.write_at(0, &hdr)?;
        self.cksum = ck;
        self.has_header = true;
        self.n_frames = 0;
        self.index.clear();
        Ok(())
    }

    /// Append one transaction: every page in `pages`, the last frame marked
    /// as the commit with the new database size, then sync.
    pub fn append_commit(&mut self, pages: &[(u32, Page)], db_size: u32) -> Result<()> {
        if !self.has_header {
            self.write_header()?;
        }
        let fs = self.frame_size();
        let start = self.n_frames;
        let mut buf = Vec::with_capacity(pages.len() * fs as usize);
        let mut running = self.cksum;
        for (i, (pgno, page)) in pages.iter().enumerate() {
            let commit = if i + 1 == pages.len() { db_size } else { 0 };
            let mut fh = [0u8; FRAME_HEADER];
            fh[0..4].copy_from_slice(&pgno.to_be_bytes());
            fh[4..8].copy_from_slice(&commit.to_be_bytes());
            fh[8..12].copy_from_slice(&self.salt.0.to_be_bytes());
            fh[12..16].copy_from_slice(&self.salt.1.to_be_bytes());
            running = checksum(self.big_endian, &fh[..8], running);
            running = checksum(self.big_endian, page, running);
            fh[16..20].copy_from_slice(&running.0.to_be_bytes());
            fh[20..24].copy_from_slice(&running.1.to_be_bytes());
            buf.extend_from_slice(&fh);
            buf.extend_from_slice(page);
        }
        let off = WAL_HEADER + start as u64 * fs;
        // Anything after the last committed frame is a dead tail.
        self.storage.truncate(off)?;
        self.storage.write_at(off, &buf)?;
        self.storage.sync()?;
        // Only now, with the frames durable, does the transaction exist.
        for (i, (pgno, _)) in pages.iter().enumerate() {
            self.index.insert(*pgno, start + i as u32);
        }
        self.n_frames = start + pages.len() as u32;
        self.cksum = running;
        self.db_size = Some(db_size);
        Ok(())
    }

    /// Empty the WAL after a checkpoint.
    pub fn reset(&mut self) -> Result<()> {
        self.storage.truncate(0)?;
        self.storage.sync()?;
        self.index.clear();
        self.n_frames = 0;
        self.db_size = None;
        self.has_header = false;
        Ok(())
    }
}

/// Salts only need to differ between WAL generations; they are not secrets.
fn new_salt(prev: u32) -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
        .unwrap_or(0);
    #[cfg(target_arch = "wasm32")]
    let t = 0x9e37_79b9u32;
    prev.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) ^ t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemStorage;

    fn page(fill: u8) -> Page {
        Rc::from(vec![fill; 512])
    }

    #[test]
    fn committed_frames_survive_reopen_and_torn_tail_is_ignored() {
        let mem = MemStorage::default();
        let mut wal = Wal::open(Box::new(mem.clone()), 512).unwrap();
        wal.append_commit(&[(1, page(1)), (2, page(2))], 2).unwrap();
        wal.append_commit(&[(2, page(3))], 2).unwrap();
        drop(wal);

        let wal = Wal::open(Box::new(mem.clone()), 512).unwrap();
        assert_eq!(wal.frame_count(), 3);
        assert_eq!(wal.read_page(2).unwrap().unwrap()[0], 3);
        assert_eq!(wal.read_page(1).unwrap().unwrap()[0], 1);
        assert!(wal.read_page(9).unwrap().is_none());
        drop(wal);

        // Chop the last frame in half: the second transaction disappears,
        // the first is intact.
        let mut bytes = mem.bytes();
        bytes.truncate(bytes.len() - 100);
        let wal = Wal::open(Box::new(MemStorage::new(bytes)), 512).unwrap();
        assert_eq!(wal.frame_count(), 2);
        assert_eq!(wal.read_page(2).unwrap().unwrap()[0], 2);
    }

    #[test]
    fn corrupted_frame_stops_recovery() {
        let mem = MemStorage::default();
        let mut wal = Wal::open(Box::new(mem.clone()), 512).unwrap();
        wal.append_commit(&[(1, page(1))], 1).unwrap();
        wal.append_commit(&[(1, page(7))], 1).unwrap();
        let mut bytes = mem.bytes();
        let second = 32 + 24 + 512 + 24 + 10;
        bytes[second] ^= 0xff;
        let wal = Wal::open(Box::new(MemStorage::new(bytes)), 512).unwrap();
        assert_eq!(wal.frame_count(), 1);
        assert_eq!(wal.read_page(1).unwrap().unwrap()[0], 1);
    }
}
