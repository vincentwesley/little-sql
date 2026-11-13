//! B-tree pages and read cursors.
//!
//! SQLite stores every table and every index as a b-tree of pages:
//!
//! * a **table b-tree** maps a 64-bit rowid to a record. All records live in
//!   leaf pages; interior pages only hold child pointers and divider keys.
//! * an **index b-tree** stores records (indexed columns followed by the
//!   rowid) in sorted order. Interior pages hold real entries too, so an
//!   in-order walk alternates child, entry, child, entry, ..., last child.
//!
//! A [`Cursor`] keeps the path from the root to its current position as a
//! stack of `(page, index)` frames, which is all that is needed to step
//! forwards or backwards without parent pointers.

use crate::error::{corrupt, Result};
use crate::pager::{Page, Pager, HEADER_SIZE};
use crate::record::read_varint;
use std::cmp::Ordering;
use std::rc::Rc;

pub const INTERIOR_INDEX: u8 = 2;
pub const INTERIOR_TABLE: u8 = 5;
pub const LEAF_INDEX: u8 = 10;
pub const LEAF_TABLE: u8 = 13;

/// A page together with its number (page 1 has a 100-byte file header in
/// front of the b-tree header).
#[derive(Clone)]
pub struct PageRef {
    pub pgno: u32,
    pub data: Page,
}

#[inline]
pub fn be16(b: &[u8], off: usize) -> usize {
    u16::from_be_bytes([b[off], b[off + 1]]) as usize
}

#[inline]
pub fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl PageRef {
    pub fn load(pager: &Pager, pgno: u32) -> Result<PageRef> {
        let data = pager.get(pgno)?;
        let p = PageRef { pgno, data };
        match p.kind() {
            INTERIOR_INDEX | INTERIOR_TABLE | LEAF_INDEX | LEAF_TABLE => {}
            k => return corrupt(format!("page {pgno} has invalid b-tree page type {k}")),
        }
        if p.cell_array() + 2 * p.ncells() > p.data.len() {
            return corrupt(format!("page {pgno} cell pointer array overflows the page"));
        }
        Ok(p)
    }

    #[inline]
    pub fn hdr(&self) -> usize {
        if self.pgno == 1 {
            HEADER_SIZE
        } else {
            0
        }
    }

    #[inline]
    pub fn kind(&self) -> u8 {
        self.data[self.hdr()]
    }

    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.kind() & 8 != 0
    }

    #[inline]
    pub fn is_index(&self) -> bool {
        matches!(self.kind(), INTERIOR_INDEX | LEAF_INDEX)
    }

    #[inline]
    pub fn ncells(&self) -> usize {
        be16(&self.data, self.hdr() + 3)
    }

    /// Offset of the cell pointer array.
    #[inline]
    pub fn cell_array(&self) -> usize {
        self.hdr() + if self.is_leaf() { 8 } else { 12 }
    }

    #[inline]
    pub fn right_child(&self) -> u32 {
        be32(&self.data, self.hdr() + 8)
    }

    /// Offset of cell `i` within the page.
    #[inline]
    pub fn cell_offset(&self, i: usize) -> Result<usize> {
        let off = be16(&self.data, self.cell_array() + 2 * i);
        if off + 4 > self.data.len() {
            return corrupt(format!(
                "page {} cell {i} points outside the page",
                self.pgno
            ));
        }
        Ok(off)
    }

    /// Child page for position `i` of an interior page: the left child of
    /// cell `i`, or the right-most child when `i == ncells`.
    pub fn child(&self, i: usize) -> Result<u32> {
        if i < self.ncells() {
            Ok(be32(&self.data, self.cell_offset(i)?))
        } else {
            Ok(self.right_child())
        }
    }

    /// Parse cell `i`.
    pub fn cell(&self, i: usize, usable: usize) -> Result<Cell> {
        parse_cell(&self.data, self.cell_offset(i)?, self.kind(), usable)
    }
}

/// A parsed cell. Fields that a page kind does not have are zero.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub left_child: u32,
    pub rowid: i64,
    /// Total payload size in bytes, including what is on overflow pages.
    pub payload_size: usize,
    /// Where the locally stored part of the payload starts in the page.
    pub local_off: usize,
    pub local_len: usize,
    /// First overflow page, or 0.
    pub overflow: u32,
    /// Size of the whole cell on the page.
    pub size: usize,
}

/// How many payload bytes are stored on the b-tree page itself.
pub fn local_payload(usable: usize, payload: usize, is_index: bool) -> usize {
    let max_local = if is_index {
        (usable - 12) * 64 / 255 - 23
    } else {
        usable - 35
    };
    if payload <= max_local {
        return payload;
    }
    let min_local = (usable - 12) * 32 / 255 - 23;
    let k = min_local + (payload - min_local) % (usable - 4);
    if k <= max_local {
        k
    } else {
        min_local
    }
}

pub fn parse_cell(page: &[u8], off: usize, kind: u8, usable: usize) -> Result<Cell> {
    let mut p = off;
    let mut cell = Cell {
        left_child: 0,
        rowid: 0,
        payload_size: 0,
        local_off: 0,
        local_len: 0,
        overflow: 0,
        size: 0,
    };
    if kind == INTERIOR_INDEX || kind == INTERIOR_TABLE {
        cell.left_child = be32(page, p);
        p += 4;
    }
    if kind == INTERIOR_TABLE {
        let (rowid, n) = read_varint(&page[p..]);
        cell.rowid = rowid as i64;
        cell.size = p + n - off;
        return Ok(cell);
    }
    let (size, n) = read_varint(&page[p..]);
    p += n;
    cell.payload_size = size as usize;
    if kind == LEAF_TABLE {
        let (rowid, n) = read_varint(&page[p..]);
        cell.rowid = rowid as i64;
        p += n;
    }
    cell.local_off = p;
    cell.local_len = local_payload(usable, cell.payload_size, kind != LEAF_TABLE);
    let mut end = p + cell.local_len;
    if cell.local_len < cell.payload_size {
        if end + 4 > page.len() {
            return corrupt("cell overflow pointer outside the page");
        }
        cell.overflow = be32(page, end);
        end += 4;
    }
    if end > page.len() {
        return corrupt("cell payload extends past the end of the page");
    }
    cell.size = end - off;
    Ok(cell)
}

/// A payload: either a slice of a page, or an owned buffer assembled from
/// the page and its overflow chain.
pub enum Payload {
    Local { page: Page, off: usize, len: usize },
    Owned(Vec<u8>),
}

impl Payload {
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Payload::Local { page, off, len } => &page[*off..*off + *len],
            Payload::Owned(v) => v,
        }
    }
}

/// Read the full payload of a cell, following the overflow chain if any.
pub fn read_payload(pager: &Pager, page: &PageRef, cell: &Cell) -> Result<Payload> {
    if cell.overflow == 0 {
        return Ok(Payload::Local {
            page: page.data.clone(),
            off: cell.local_off,
            len: cell.local_len,
        });
    }
    let mut out = Vec::with_capacity(cell.payload_size);
    out.extend_from_slice(&page.data[cell.local_off..cell.local_off + cell.local_len]);
    let usable = pager.usable_size();
    let mut next = cell.overflow;
    let mut hops = 0;
    while out.len() < cell.payload_size {
        if next == 0 {
            return corrupt("overflow chain ends before the payload is complete");
        }
        hops += 1;
        if hops > pager.page_count() {
            return corrupt("overflow chain loops");
        }
        let ov = pager.get(next)?;
        let take = (cell.payload_size - out.len()).min(usable - 4);
        out.extend_from_slice(&ov[4..4 + take]);
        next = be32(&ov, 0);
    }
    Ok(Payload::Owned(out))
}

struct Frame {
    page: PageRef,
    idx: usize,
}

/// A read cursor over one b-tree.
pub struct Cursor {
    pager: Rc<Pager>,
    root: u32,
    stack: Vec<Frame>,
    usable: usize,
    /// Number of seeks performed (for EXPLAIN statistics).
    pub seeks: u64,
}

const MAX_DEPTH: usize = 40;

impl Cursor {
    pub fn new(pager: Rc<Pager>, root: u32) -> Cursor {
        let usable = pager.usable_size();
        Cursor {
            pager,
            root,
            stack: Vec::new(),
            usable,
            seeks: 0,
        }
    }

    /// Whether the cursor points at an entry.
    #[inline]
    pub fn valid(&self) -> bool {
        !self.stack.is_empty()
    }

    fn push(&mut self, page: PageRef, idx: usize) -> Result<()> {
        if self.stack.len() >= MAX_DEPTH {
            return corrupt("b-tree is deeper than any valid database allows");
        }
        self.stack.push(Frame { page, idx });
        Ok(())
    }

    fn descend_leftmost(&mut self, mut pgno: u32) -> Result<()> {
        loop {
            let page = PageRef::load(&self.pager, pgno)?;
            if page.is_leaf() {
                let empty = page.ncells() == 0;
                self.push(page, 0)?;
                if empty {
                    self.stack.pop();
                    self.climb()?;
                }
                return Ok(());
            }
            pgno = page.child(0)?;
            self.push(page, 0)?;
        }
    }

    fn descend_rightmost(&mut self, mut pgno: u32) -> Result<()> {
        loop {
            let page = PageRef::load(&self.pager, pgno)?;
            let n = page.ncells();
            if page.is_leaf() {
                if n == 0 {
                    return self.climb_back();
                }
                return self.push(page, n - 1);
            }
            pgno = page.right_child();
            self.push(page, n)?;
        }
    }

    /// After finishing the subtree under the top frame's child `idx`, move
    /// to whatever comes next in key order.
    fn climb(&mut self) -> Result<()> {
        loop {
            let Some(top) = self.stack.last_mut() else {
                return Ok(());
            };
            let n = top.page.ncells();
            if top.idx < n {
                if top.page.is_index() {
                    // The divider entry itself is next.
                    return Ok(());
                }
                top.idx += 1;
                let child = top.page.child(top.idx)?;
                return self.descend_leftmost(child);
            }
            self.stack.pop();
        }
    }

    fn climb_back(&mut self) -> Result<()> {
        loop {
            let Some(top) = self.stack.last_mut() else {
                return Ok(());
            };
            if top.idx > 0 {
                top.idx -= 1;
                if top.page.is_index() {
                    return Ok(());
                }
                let child = top.page.child(top.idx)?;
                return self.descend_rightmost(child);
            }
            self.stack.pop();
        }
    }

    /// Position at the first entry. The cursor is invalid if the tree is
    /// empty.
    pub fn first(&mut self) -> Result<()> {
        self.stack.clear();
        self.descend_leftmost(self.root)
    }

    /// Position at the last entry.
    pub fn last(&mut self) -> Result<()> {
        self.stack.clear();
        self.descend_rightmost(self.root)
    }

    /// Step to the next entry in key order.
    pub fn next(&mut self) -> Result<()> {
        let Some(top) = self.stack.last_mut() else {
            return Ok(());
        };
        if top.page.is_leaf() {
            if top.idx + 1 < top.page.ncells() {
                top.idx += 1;
                return Ok(());
            }
            self.stack.pop();
            self.climb()
        } else {
            // On an interior index entry: continue into the next child.
            top.idx += 1;
            let child = top.page.child(top.idx)?;
            self.descend_leftmost(child)
        }
    }

    /// Step to the previous entry in key order.
    pub fn prev(&mut self) -> Result<()> {
        let Some(top) = self.stack.last_mut() else {
            return Ok(());
        };
        if top.page.is_leaf() {
            if top.idx > 0 {
                top.idx -= 1;
                return Ok(());
            }
            self.stack.pop();
            self.climb_back()
        } else {
            let child = top.page.child(top.idx)?;
            self.descend_rightmost(child)
        }
    }

    /// Table b-tree: position at the first row whose rowid is `>= rowid`.
    pub fn seek_rowid(&mut self, rowid: i64) -> Result<()> {
        self.seeks += 1;
        self.stack.clear();
        let mut pgno = self.root;
        loop {
            let page = PageRef::load(&self.pager, pgno)?;
            let n = page.ncells();
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if page.cell(mid, self.usable)?.rowid < rowid {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if page.is_leaf() {
                self.push(page, lo)?;
                if lo >= n {
                    self.stack.pop();
                    self.climb()?;
                }
                return Ok(());
            }
            pgno = page.child(lo)?;
            self.push(page, lo)?;
        }
    }

    /// Index b-tree: position at the first entry for which `cmp` (the
    /// ordering of the entry's record relative to the search key) is not
    /// `Less`.
    pub fn seek(&mut self, cmp: &mut dyn FnMut(&[u8]) -> Result<Ordering>) -> Result<()> {
        self.seeks += 1;
        self.stack.clear();
        let mut pgno = self.root;
        loop {
            let page = PageRef::load(&self.pager, pgno)?;
            let n = page.ncells();
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let cell = page.cell(mid, self.usable)?;
                let payload = read_payload(&self.pager, &page, &cell)?;
                if cmp(payload.as_slice())? == Ordering::Less {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if page.is_leaf() {
                self.push(page, lo)?;
                if lo >= n {
                    self.stack.pop();
                    self.climb()?;
                }
                return Ok(());
            }
            pgno = page.child(lo)?;
            self.push(page, lo)?;
        }
    }

    fn current(&self) -> Result<(&PageRef, Cell)> {
        match self.stack.last() {
            Some(top) => Ok((&top.page, top.page.cell(top.idx, self.usable)?)),
            None => corrupt("cursor is not positioned on a row"),
        }
    }

    /// Rowid of the current table row.
    pub fn rowid(&self) -> Result<i64> {
        Ok(self.current()?.1.rowid)
    }

    /// Record of the current entry.
    pub fn payload(&self) -> Result<Payload> {
        let (page, cell) = self.current()?;
        read_payload(&self.pager, page, &cell)
    }
}

/// Count the entries of a b-tree without decoding any payload.
pub fn count_entries(pager: &Pager, root: u32) -> Result<u64> {
    fn walk(pager: &Pager, pgno: u32, depth: usize) -> Result<u64> {
        if depth > MAX_DEPTH {
            return corrupt("b-tree is deeper than any valid database allows");
        }
        let page = PageRef::load(pager, pgno)?;
        let n = page.ncells();
        if page.is_leaf() {
            return Ok(n as u64);
        }
        let mut total = if page.is_index() { n as u64 } else { 0 };
        for i in 0..=n {
            total += walk(pager, page.child(i)?, depth + 1)?;
        }
        Ok(total)
    }
    walk(pager, root, 0)
}

/// Cheap row estimate for the planner: fan-out along the left edge of the
/// tree multiplied out. Exact for single-page tables.
pub fn estimate_rows(pager: &Pager, root: u32) -> u64 {
    let mut est = 1u64;
    let mut pgno = root;
    for _ in 0..MAX_DEPTH {
        let Ok(page) = PageRef::load(pager, pgno) else {
            return est;
        };
        let n = page.ncells() as u64;
        if page.is_leaf() {
            return est.saturating_mul(n);
        }
        est = est.saturating_mul(n + 1);
        pgno = match page.child(0) {
            Ok(c) => c,
            Err(_) => return est,
        };
    }
    est
}
