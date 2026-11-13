//! The pager: fixed-size pages on top of a [`Storage`], with a cache, a set
//! of uncommitted (dirty) pages, and a write-ahead log.
//!
//! Reading a page looks in three places, newest first: the dirty set of the
//! open transaction, the WAL (the newest committed frame for that page), and
//! finally the database file. Writers never modify a page in place: they
//! build a new image and hand it to [`Pager::write_page`], so a rollback is
//! simply forgetting the dirty set.

use crate::error::{corrupt, Error, Result};
use crate::record::TextEncoding;
use crate::storage::Storage;
use crate::wal::Wal;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// An immutable page image. Page numbers start at 1.
pub type Page = Rc<[u8]>;

pub const HEADER_SIZE: usize = 100;
const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// The fields of the 100-byte database header that the engine uses.
#[derive(Clone, Debug)]
pub struct DbHeader {
    pub page_size: u32,
    pub write_version: u8,
    pub read_version: u8,
    pub reserved: u8,
    pub change_counter: u32,
    pub db_size: u32,
    pub freelist_trunk: u32,
    pub freelist_count: u32,
    pub schema_cookie: u32,
    pub schema_format: u32,
    pub encoding: TextEncoding,
    pub version_valid_for: u32,
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl DbHeader {
    pub fn parse(b: &[u8]) -> Result<DbHeader> {
        if b.len() < HEADER_SIZE || &b[..16] != MAGIC {
            return Err(Error::Corrupt(
                "file is not a database (bad magic string)".into(),
            ));
        }
        let ps = u16::from_be_bytes([b[16], b[17]]) as u32;
        let page_size = if ps == 1 { 65536 } else { ps };
        if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
            return corrupt(format!("invalid page size {page_size}"));
        }
        let encoding = match be32(b, 56) {
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            e => return corrupt(format!("unknown text encoding {e}")),
        };
        Ok(DbHeader {
            page_size,
            write_version: b[18],
            read_version: b[19],
            reserved: b[20],
            change_counter: be32(b, 24),
            db_size: be32(b, 28),
            freelist_trunk: be32(b, 32),
            freelist_count: be32(b, 36),
            schema_cookie: be32(b, 40),
            schema_format: be32(b, 44),
            encoding,
            version_valid_for: be32(b, 92),
        })
    }
}

/// Counters the EXPLAIN view reports.
#[derive(Default)]
pub struct PagerStats {
    /// Pages fetched from storage or the WAL (cache misses).
    pub page_reads: Cell<u64>,
    /// Pages served from the cache.
    pub cache_hits: Cell<u64>,
}

pub struct Pager {
    db: RefCell<Box<dyn Storage>>,
    wal: RefCell<Option<Wal>>,
    page_size: u32,
    usable: u32,
    encoding: TextEncoding,
    cache: RefCell<HashMap<u32, Page>>,
    cache_limit: usize,
    dirty: RefCell<BTreeMap<u32, Page>>,
    /// Database size in pages, including uncommitted growth.
    n_pages: Cell<u32>,
    /// Database size in pages as of the last commit.
    committed_pages: Cell<u32>,
    writable: bool,
    pub stats: PagerStats,
}

/// Page size used for databases this engine creates.
pub const DEFAULT_PAGE_SIZE: u32 = 4096;

impl Pager {
    /// Open a pager over `db`. `wal`, when given, is the storage for the
    /// `-wal` file; committed frames found there are replayed logically
    /// (readers see them) and new commits are appended to it.
    pub fn open(
        db: Box<dyn Storage>,
        wal: Option<Box<dyn Storage>>,
        writable: bool,
    ) -> Result<Pager> {
        let len = db.len()?;
        let mut page_size = DEFAULT_PAGE_SIZE;
        let mut encoding = TextEncoding::Utf8;
        let mut reserved = 0u32;
        let mut n_pages = 0u32;
        if len > 0 {
            let mut hdr = [0u8; HEADER_SIZE];
            if db.read_at(0, &mut hdr)? < HEADER_SIZE {
                return Err(Error::Corrupt("file is not a database (too short)".into()));
            }
            let h = DbHeader::parse(&hdr)?;
            if h.read_version > 2 {
                return Err(Error::Unsupported(format!(
                    "file format read version {}",
                    h.read_version
                )));
            }
            page_size = h.page_size;
            encoding = h.encoding;
            reserved = h.reserved as u32;
            let by_len = len.div_ceil(page_size as u64) as u32;
            n_pages = if h.db_size != 0 && h.change_counter == h.version_valid_for {
                h.db_size
            } else {
                by_len
            };
        }
        let wal = match wal {
            Some(storage) => Some(Wal::open(storage, page_size)?),
            None => None,
        };
        if let Some(w) = &wal {
            if let Some(sz) = w.committed_db_size() {
                n_pages = sz;
                // The page size and encoding may themselves live in the WAL
                // if the database was created and never checkpointed.
                if len == 0 {
                    if let Some(p1) = w.read_page(1)? {
                        let h = DbHeader::parse(&p1)?;
                        encoding = h.encoding;
                        reserved = h.reserved as u32;
                    }
                }
            }
        }
        Ok(Pager {
            db: RefCell::new(db),
            wal: RefCell::new(wal),
            page_size,
            usable: page_size - reserved,
            encoding,
            cache: RefCell::new(HashMap::new()),
            cache_limit: ((64 << 20) / page_size as usize).max(64),
            dirty: RefCell::new(BTreeMap::new()),
            n_pages: Cell::new(n_pages),
            committed_pages: Cell::new(n_pages),
            writable,
            stats: PagerStats::default(),
        })
    }

    pub fn page_size(&self) -> usize {
        self.page_size as usize
    }

    /// Bytes per page available to the b-tree (page size minus the reserved
    /// region at the end of each page).
    pub fn usable_size(&self) -> usize {
        self.usable as usize
    }

    pub fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    pub fn page_count(&self) -> u32 {
        self.n_pages.get()
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn in_transaction(&self) -> bool {
        !self.dirty.borrow().is_empty() || self.n_pages.get() != self.committed_pages.get()
    }

    /// Fetch a page image.
    pub fn get(&self, pgno: u32) -> Result<Page> {
        if pgno == 0 || pgno > self.n_pages.get() {
            return corrupt(format!(
                "page {pgno} is out of range (database has {} pages)",
                self.n_pages.get()
            ));
        }
        if let Some(p) = self.dirty.borrow().get(&pgno) {
            return Ok(p.clone());
        }
        if let Some(p) = self.cache.borrow().get(&pgno) {
            self.stats.cache_hits.set(self.stats.cache_hits.get() + 1);
            return Ok(p.clone());
        }
        self.stats.page_reads.set(self.stats.page_reads.get() + 1);
        let from_wal = match self.wal.borrow().as_ref() {
            Some(w) => w.read_page(pgno)?,
            None => None,
        };
        let page: Page = match from_wal {
            Some(p) => p,
            None => {
                let mut buf = vec![0u8; self.page_size as usize];
                let off = (pgno as u64 - 1) * self.page_size as u64;
                // A short read happens for pages allocated but not yet
                // written (the tail stays zero), which is valid.
                self.db.borrow().read_at(off, &mut buf)?;
                Rc::from(buf)
            }
        };
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= self.cache_limit {
            cache.clear();
        }
        cache.insert(pgno, page.clone());
        Ok(page)
    }

    fn require_writable(&self) -> Result<()> {
        if self.writable {
            Ok(())
        } else {
            Err(Error::Runtime(
                "attempt to write a readonly database".into(),
            ))
        }
    }

    /// Replace the image of `pgno` within the current transaction.
    pub fn write_page(&self, pgno: u32, data: Vec<u8>) -> Result<()> {
        self.require_writable()?;
        debug_assert_eq!(data.len(), self.page_size as usize);
        if pgno == 0 || pgno > self.n_pages.get() {
            return corrupt(format!("write to page {pgno} out of range"));
        }
        self.dirty.borrow_mut().insert(pgno, Rc::from(data));
        Ok(())
    }

    /// Extend the database by one page and return its number. The page that
    /// would hold the lock byte (at the 1 GiB mark) is never used.
    pub fn append_page(&self) -> Result<u32> {
        self.require_writable()?;
        let mut pgno = self.n_pages.get() + 1;
        let lock_page = (0x4000_0000u64 / self.page_size as u64) as u32 + 1;
        if pgno == lock_page {
            self.n_pages.set(pgno);
            self.dirty
                .borrow_mut()
                .insert(pgno, Rc::from(vec![0u8; self.page_size as usize]));
            pgno += 1;
        }
        self.n_pages.set(pgno);
        self.dirty
            .borrow_mut()
            .insert(pgno, Rc::from(vec![0u8; self.page_size as usize]));
        Ok(pgno)
    }

    /// Shrink the database to `n` pages within the current transaction.
    pub fn truncate_pages(&self, n: u32) {
        self.n_pages.set(n);
        self.dirty.borrow_mut().retain(|&p, _| p <= n);
    }

    /// Discard every change made since the last commit.
    pub fn rollback(&self) {
        self.dirty.borrow_mut().clear();
        self.n_pages.set(self.committed_pages.get());
    }

    /// Make the current transaction durable: append its pages to the WAL as
    /// one committed group of frames. Without a WAL (in-memory databases
    /// opened without one) pages are written straight to storage.
    pub fn commit(&self) -> Result<()> {
        let dirty = std::mem::take(&mut *self.dirty.borrow_mut());
        let n_pages = self.n_pages.get();
        if dirty.is_empty() && n_pages == self.committed_pages.get() {
            return Ok(());
        }
        let mut wal = self.wal.borrow_mut();
        match wal.as_mut() {
            Some(w) => {
                let pages: Vec<(u32, Page)> = if dirty.is_empty() {
                    // A pure truncation still needs a commit frame.
                    vec![(1, self.get_committed(1, w)?)]
                } else {
                    dirty.iter().map(|(k, v)| (*k, v.clone())).collect()
                };
                w.append_commit(&pages, n_pages)?;
            }
            None => {
                let mut db = self.db.borrow_mut();
                for (pgno, page) in &dirty {
                    db.write_at((*pgno as u64 - 1) * self.page_size as u64, page)?;
                }
                db.truncate(n_pages as u64 * self.page_size as u64)?;
                db.sync()?;
            }
        }
        let mut cache = self.cache.borrow_mut();
        for (pgno, page) in dirty {
            cache.insert(pgno, page);
        }
        cache.retain(|&p, _| p <= n_pages);
        self.committed_pages.set(n_pages);
        Ok(())
    }

    fn get_committed(&self, pgno: u32, wal: &Wal) -> Result<Page> {
        if let Some(p) = wal.read_page(pgno)? {
            return Ok(p);
        }
        let mut buf = vec![0u8; self.page_size as usize];
        self.db
            .borrow()
            .read_at((pgno as u64 - 1) * self.page_size as u64, &mut buf)?;
        Ok(Rc::from(buf))
    }

    /// Copy every committed WAL frame back into the database file and reset
    /// the WAL. Returns the number of pages copied.
    pub fn checkpoint(&self) -> Result<usize> {
        if self.in_transaction() {
            return Err(Error::Runtime(
                "cannot checkpoint inside a transaction".into(),
            ));
        }
        let mut wal = self.wal.borrow_mut();
        let Some(w) = wal.as_mut() else { return Ok(0) };
        let Some(db_size) = w.committed_db_size() else {
            return Ok(0);
        };
        let mut db = self.db.borrow_mut();
        let mut n = 0;
        for pgno in w.pages() {
            if pgno > db_size {
                continue;
            }
            if let Some(page) = w.read_page(pgno)? {
                db.write_at((pgno as u64 - 1) * self.page_size as u64, &page)?;
                n += 1;
            }
        }
        db.truncate(db_size as u64 * self.page_size as u64)?;
        db.sync()?;
        w.reset()?;
        Ok(n)
    }

    /// Number of frames currently in the WAL (0 without one).
    pub fn wal_frames(&self) -> usize {
        self.wal.borrow().as_ref().map_or(0, |w| w.frame_count())
    }

    pub fn has_wal(&self) -> bool {
        self.wal.borrow().is_some()
    }
}
