//! Changing b-trees: insert, delete, split, free pages.
//!
//! The approach favours being obviously correct over being clever:
//!
//! * A page being changed is read into a [`Node`]: its type, its cells as
//!   raw byte strings and its right-most child. Edits happen on the cell
//!   list; writing a node lays the page out again from scratch.
//! * Inserts descend from the root recording the path. If the changed page
//!   no longer fits, its cells are packed greedily into as many pages as
//!   needed; the last group stays on the original page (so the parent's
//!   pointer to it stays valid) and a divider for each new page goes into
//!   the parent, which may split in turn. A root that splits keeps its page
//!   number and becomes an interior page over new children.
//! * Deletes remove the cell. A page left empty is freed and unlinked from
//!   its parent; an interior page left with no cells is replaced by its only
//!   child. Pages are allowed to be under-full (SQLite allows that too).
//!   Deleting an index entry that sits on an interior page swaps in its
//!   in-order predecessor from the leaves first.
//! * Freed pages go on the freelist (trunk pages listing leaf pages, as in
//!   the file format) and are reused before the file grows.

use crate::btree::{
    be16, be32, local_payload, parse_cell, read_payload, PageRef, INTERIOR_INDEX, INTERIOR_TABLE,
    LEAF_INDEX, LEAF_TABLE,
};
use crate::error::{corrupt, Error, Result};
use crate::pager::{Pager, HEADER_SIZE};
use crate::record::{decode_all, read_varint, write_varint, TextEncoding};
use crate::value::{Collation, Value};
use std::cmp::Ordering;

/// One b-tree page held in memory while it changes.
struct Node {
    pgno: u32,
    kind: u8,
    /// Raw cells, exactly as stored on the page.
    cells: Vec<Vec<u8>>,
    /// Right-most child (interior pages only).
    right: u32,
}

impl Node {
    fn is_leaf(&self) -> bool {
        self.kind & 8 != 0
    }

    fn is_index(&self) -> bool {
        matches!(self.kind, INTERIOR_INDEX | LEAF_INDEX)
    }

    fn child(&self, i: usize) -> u32 {
        if i < self.cells.len() {
            be32(&self.cells[i], 0)
        } else {
            self.right
        }
    }
}

/// How index entries are ordered: per leading field, its collation and
/// whether it sorts descending. Fields past the list compare as BINARY,
/// ascending (the rowid, or a WITHOUT ROWID table's other key columns).
#[derive(Clone, Debug, Default)]
pub struct KeyOrder {
    pub fields: Vec<(Collation, bool)>,
}

impl KeyOrder {
    fn compare(&self, a: &[Value], b: &[Value], n: usize) -> Ordering {
        for i in 0..n {
            let (x, y) = (a.get(i), b.get(i));
            let (x, y) = match (x, y) {
                (Some(x), Some(y)) => (x, y),
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
            };
            let (coll, desc) = self
                .fields
                .get(i)
                .copied()
                .unwrap_or((Collation::Binary, false));
            let o = x.compare(y, coll);
            if o != Ordering::Equal {
                return if desc { o.reverse() } else { o };
            }
        }
        Ordering::Equal
    }
}

/// What an insert or delete is looking for.
pub enum Key<'k> {
    Rowid(i64),
    /// An index entry (or WITHOUT ROWID row): its decoded fields, compared
    /// on the first `n` of them.
    Entry {
        fields: &'k [Value],
        order: &'k KeyOrder,
        n: usize,
    },
}

/// The b-tree writer for one pager.
pub struct Writer<'p> {
    pager: &'p Pager,
    usable: usize,
    enc: TextEncoding,
}

/// Bytes of b-tree page header for a page kind.
fn header_len(kind: u8) -> usize {
    if kind & 8 != 0 {
        8
    } else {
        12
    }
}

impl<'p> Writer<'p> {
    pub fn new(pager: &'p Pager) -> Writer<'p> {
        Writer {
            pager,
            usable: pager.usable_size(),
            enc: pager.encoding(),
        }
    }

    // ------------------------------------------------------------ pages

    fn page_offset(pgno: u32) -> usize {
        if pgno == 1 {
            HEADER_SIZE
        } else {
            0
        }
    }

    /// Room for cells and their pointers on a page of this kind.
    fn capacity(&self, pgno: u32, kind: u8) -> usize {
        self.usable - Self::page_offset(pgno) - header_len(kind)
    }

    fn fits(&self, node: &Node) -> bool {
        let used: usize = node.cells.iter().map(|c| c.len() + 2).sum();
        used <= self.capacity(node.pgno, node.kind)
    }

    fn read_node(&self, pgno: u32) -> Result<Node> {
        let page = PageRef::load(self.pager, pgno)?;
        let kind = page.kind();
        let mut cells = Vec::with_capacity(page.ncells());
        for i in 0..page.ncells() {
            let off = page.cell_offset(i)?;
            let cell = parse_cell(&page.data, off, kind, self.usable)?;
            cells.push(page.data[off..off + cell.size].to_vec());
        }
        Ok(Node {
            pgno,
            kind,
            cells,
            right: if page.is_leaf() {
                0
            } else {
                page.right_child()
            },
        })
    }

    fn write_node(&self, node: &Node) -> Result<()> {
        let size = self.pager.page_size();
        let mut buf = vec![0u8; size];
        let hdr = Self::page_offset(node.pgno);
        if node.pgno == 1 {
            let old = self.pager.get(1)?;
            buf[..HEADER_SIZE].copy_from_slice(&old[..HEADER_SIZE]);
        }
        let mut content = self.usable;
        let ptrs = hdr + header_len(node.kind);
        for (i, c) in node.cells.iter().enumerate() {
            content -= c.len();
            buf[content..content + c.len()].copy_from_slice(c);
            buf[ptrs + 2 * i..ptrs + 2 * i + 2].copy_from_slice(&(content as u16).to_be_bytes());
        }
        if ptrs + 2 * node.cells.len() > content {
            return corrupt(format!("internal error: page {} overfilled", node.pgno));
        }
        buf[hdr] = node.kind;
        buf[hdr + 3..hdr + 5].copy_from_slice(&(node.cells.len() as u16).to_be_bytes());
        // A content start of 65536 is stored as 0.
        let start = if content == 65536 { 0 } else { content as u16 };
        buf[hdr + 5..hdr + 7].copy_from_slice(&start.to_be_bytes());
        if !node.is_leaf() {
            buf[hdr + 8..hdr + 12].copy_from_slice(&node.right.to_be_bytes());
        }
        self.pager.write_page(node.pgno, buf)
    }

    /// A page for new content: from the freelist if possible, else new.
    pub fn alloc_page(&self) -> Result<u32> {
        let p1 = self.pager.get(1)?;
        let trunk = be32(&p1, 32);
        if trunk == 0 {
            return self.pager.append_page();
        }
        let count = be32(&p1, 36);
        let t = self.pager.get(trunk)?;
        let n = be32(&t, 4) as usize;
        let got = if n > 0 {
            // Take the last leaf listed on the trunk.
            let leaf = be32(&t, 8 + 4 * (n - 1));
            let mut tb = t.to_vec();
            tb[4..8].copy_from_slice(&((n - 1) as u32).to_be_bytes());
            self.pager.write_page(trunk, tb)?;
            leaf
        } else {
            // An empty trunk is itself the page handed out.
            let next = be32(&t, 0);
            self.set_header_u32(32, next)?;
            trunk
        };
        self.set_header_u32(36, count.saturating_sub(1))?;
        if got == 0 || got > self.pager.page_count() {
            return corrupt(format!("freelist lists invalid page {got}"));
        }
        Ok(got)
    }

    /// Put a page on the freelist.
    pub fn free_page(&self, pgno: u32) -> Result<()> {
        if pgno < 2 {
            return corrupt(format!("attempt to free page {pgno}"));
        }
        let p1 = self.pager.get(1)?;
        let trunk = be32(&p1, 32);
        let count = be32(&p1, 36);
        // SQLite fills trunks to usable/4 - 8 entries, for compatibility
        // with old readers.
        let max_leaves = self.usable / 4 - 8;
        if trunk != 0 {
            let t = self.pager.get(trunk)?;
            let n = be32(&t, 4) as usize;
            if n < max_leaves {
                let mut tb = t.to_vec();
                tb[8 + 4 * n..12 + 4 * n].copy_from_slice(&pgno.to_be_bytes());
                tb[4..8].copy_from_slice(&((n + 1) as u32).to_be_bytes());
                self.pager.write_page(trunk, tb)?;
                return self.set_header_u32(36, count + 1);
            }
        }
        // The freed page becomes the new first trunk.
        let mut tb = vec![0u8; self.pager.page_size()];
        tb[0..4].copy_from_slice(&trunk.to_be_bytes());
        self.pager.write_page(pgno, tb)?;
        self.set_header_u32(32, pgno)?;
        self.set_header_u32(36, count + 1)
    }

    pub fn set_header_u32(&self, off: usize, v: u32) -> Result<()> {
        let mut p1 = self.pager.get(1)?.to_vec();
        p1[off..off + 4].copy_from_slice(&v.to_be_bytes());
        self.pager.write_page(1, p1)
    }

    // ------------------------------------------------------------ cells

    /// Build the on-page bytes for a payload: the local part, and an
    /// overflow chain for the rest. `prefix` is what comes before the
    /// payload size (a child pointer on interior index pages).
    fn payload_cell(
        &self,
        prefix: &[u8],
        rowid: Option<i64>,
        payload: &[u8],
        is_index: bool,
    ) -> Result<Vec<u8>> {
        let mut cell = prefix.to_vec();
        write_varint(payload.len() as u64, &mut cell);
        if let Some(r) = rowid {
            write_varint(r as u64, &mut cell);
        }
        let local = local_payload(self.usable, payload.len(), is_index);
        cell.extend_from_slice(&payload[..local]);
        if local < payload.len() {
            let first = self.write_overflow(&payload[local..])?;
            cell.extend_from_slice(&first.to_be_bytes());
        }
        Ok(cell)
    }

    fn write_overflow(&self, mut rest: &[u8]) -> Result<u32> {
        let per = self.usable - 4;
        let mut pages = Vec::new();
        let n = rest.len().div_ceil(per);
        for _ in 0..n {
            pages.push(self.alloc_page()?);
        }
        for (i, &pg) in pages.iter().enumerate() {
            let mut buf = vec![0u8; self.pager.page_size()];
            let next = pages.get(i + 1).copied().unwrap_or(0);
            buf[0..4].copy_from_slice(&next.to_be_bytes());
            let take = rest.len().min(per);
            buf[4..4 + take].copy_from_slice(&rest[..take]);
            rest = &rest[take..];
            self.pager.write_page(pg, buf)?;
        }
        Ok(pages[0])
    }

    /// Free the overflow chain of a cell taken from a page of `kind`.
    fn free_cell_overflow(&self, cell: &[u8], kind: u8) -> Result<()> {
        if kind == INTERIOR_TABLE {
            return Ok(());
        }
        let c = parse_cell(cell, 0, kind, self.usable)?;
        let mut next = c.overflow;
        let mut hops = 0;
        while next != 0 {
            hops += 1;
            if hops > self.pager.page_count() {
                return corrupt("overflow chain loops");
            }
            let pg = self.pager.get(next)?;
            let after = be32(&pg, 0);
            self.free_page(next)?;
            next = after;
        }
        Ok(())
    }

    /// The full payload of a cell (reading its overflow chain).
    fn cell_payload(&self, cell: &[u8], kind: u8) -> Result<Vec<u8>> {
        let c = parse_cell(cell, 0, kind, self.usable)?;
        let page = PageRef {
            pgno: 0,
            data: std::rc::Rc::from(cell.to_vec()),
        };
        Ok(read_payload(self.pager, &page, &c)?.as_slice().to_vec())
    }

    fn cell_rowid(cell: &[u8], kind: u8) -> i64 {
        match kind {
            INTERIOR_TABLE => read_varint(&cell[4..]).0 as i64,
            _ => {
                let (_, n) = read_varint(cell);
                read_varint(&cell[n..]).0 as i64
            }
        }
    }

    /// Compare the search key with cell `cell` of a page of `kind`:
    /// `Less` means the key sorts before the cell.
    fn compare_key(&self, key: &Key, cell: &[u8], kind: u8) -> Result<Ordering> {
        match key {
            Key::Rowid(r) => Ok(r.cmp(&Self::cell_rowid(cell, kind))),
            Key::Entry { fields, order, n } => {
                let body = if kind == INTERIOR_INDEX {
                    &cell[4..]
                } else {
                    cell
                };
                let payload = self.cell_payload(body, LEAF_INDEX)?;
                let vals = decode_all(&payload, self.enc)?;
                Ok(order.compare(fields, &vals, *n))
            }
        }
    }

    /// Index of the first cell whose key is >= `key`, and whether it is
    /// equal.
    fn search(&self, node: &Node, key: &Key) -> Result<(usize, bool)> {
        let (mut lo, mut hi) = (0, node.cells.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.compare_key(key, &node.cells[mid], node.kind)? {
                Ordering::Greater => lo = mid + 1,
                _ => hi = mid,
            }
        }
        let eq = lo < node.cells.len()
            && self.compare_key(key, &node.cells[lo], node.kind)? == Ordering::Equal;
        Ok((lo, eq))
    }

    // ------------------------------------------------------------ trees

    /// Create an empty b-tree and return its root page.
    pub fn create_tree(&self, is_index: bool) -> Result<u32> {
        let pgno = self.alloc_page()?;
        self.write_node(&Node {
            pgno,
            kind: if is_index { LEAF_INDEX } else { LEAF_TABLE },
            cells: Vec::new(),
            right: 0,
        })?;
        Ok(pgno)
    }

    /// Free every page of a b-tree except its root, and empty the root.
    pub fn clear_tree(&self, root: u32) -> Result<()> {
        let node = self.read_node(root)?;
        let leaf_kind = if node.is_index() {
            LEAF_INDEX
        } else {
            LEAF_TABLE
        };
        self.free_subtrees(&node)?;
        self.write_node(&Node {
            pgno: root,
            kind: leaf_kind,
            cells: Vec::new(),
            right: 0,
        })
    }

    /// Free every page of a b-tree, root included.
    pub fn drop_tree(&self, root: u32) -> Result<()> {
        let node = self.read_node(root)?;
        self.free_subtrees(&node)?;
        self.free_page(root)
    }

    fn free_subtrees(&self, node: &Node) -> Result<()> {
        for c in &node.cells {
            let body_kind = node.kind;
            self.free_cell_overflow(c, body_kind)?;
        }
        if !node.is_leaf() {
            for i in 0..=node.cells.len() {
                let child = self.read_node(node.child(i))?;
                self.free_subtrees(&child)?;
                self.free_page(child.pgno)?;
            }
        }
        Ok(())
    }

    /// Insert a row into a table b-tree, replacing any row with the same
    /// rowid.
    pub fn table_insert(&self, root: u32, rowid: i64, record: &[u8]) -> Result<()> {
        let cell = self.payload_cell(&[], Some(rowid), record, false)?;
        self.insert(root, &Key::Rowid(rowid), cell, true)
    }

    /// Insert an entry into an index b-tree (or a row into a WITHOUT ROWID
    /// table). With `replace`, an entry equal on the first `n` fields is
    /// replaced; otherwise equal entries are an error.
    pub fn index_insert(
        &self,
        root: u32,
        record: &[u8],
        order: &KeyOrder,
        n: usize,
        replace: bool,
    ) -> Result<()> {
        let fields = decode_all(record, self.enc)?;
        let cell = self.payload_cell(&[], None, record, true)?;
        let key = Key::Entry {
            fields: &fields,
            order,
            n,
        };
        self.insert(root, &key, cell, replace)
    }

    fn insert(&self, root: u32, key: &Key, cell: Vec<u8>, replace: bool) -> Result<()> {
        let mut path: Vec<(Node, usize)> = Vec::new();
        let mut node = self.read_node(root)?;
        loop {
            let (i, eq) = self.search(&node, key)?;
            if eq && (node.is_leaf() || node.is_index()) {
                if !replace {
                    return Err(Error::Constraint("duplicate entry in index".into()));
                }
                // Replace in place (index entries keep any child pointer).
                let old = std::mem::take(&mut node.cells[i]);
                if node.kind == INTERIOR_INDEX {
                    self.free_cell_overflow(&old[4..], LEAF_INDEX)?;
                    let mut c = old[..4].to_vec();
                    c.extend_from_slice(&cell);
                    node.cells[i] = c;
                } else {
                    self.free_cell_overflow(&old, node.kind)?;
                    node.cells[i] = cell;
                }
                return self.store(path, node, root);
            }
            if node.is_leaf() {
                node.cells.insert(i, cell);
                return self.store(path, node, root);
            }
            let child = node.child(i);
            path.push((node, i));
            node = self.read_node(child)?;
        }
    }

    /// Write a changed node, splitting it (and its ancestors) as needed.
    /// `path` holds the ancestors with the child index taken in each.
    fn store(&self, mut path: Vec<(Node, usize)>, mut node: Node, root: u32) -> Result<()> {
        loop {
            if self.fits(&node) {
                return self.write_node(&node);
            }
            if node.pgno == root {
                return self.split_root(node);
            }
            let dividers = self.split(&mut node, false)?;
            let (mut parent, i) = path.pop().expect("a non-root page has a parent");
            for (k, d) in dividers.into_iter().enumerate() {
                parent.cells.insert(i + k, d);
            }
            node = parent;
        }
    }

    /// Pack a too-full node's cells into pages. Returns the parent cells
    /// (child pointer + divider) for every new page, in order. Unless
    /// `all_new`, the last group stays on the node's own page (written
    /// here); otherwise it is left in `node` for the caller.
    fn split(&self, node: &mut Node, all_new: bool) -> Result<Vec<Vec<u8>>> {
        let kind = node.kind;
        let cap = self.usable - header_len(kind);
        let cells = std::mem::take(&mut node.cells);
        // Leaves of table b-trees copy the divider key; every other kind
        // moves a cell up into the parent.
        let consumes = kind != LEAF_TABLE;
        let mut groups: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
        let mut dividers: Vec<Vec<u8>> = Vec::new();
        let mut used = 0;
        let mut it = cells.into_iter().peekable();
        while let Some(c) = it.next() {
            let need = c.len() + 2;
            let cur = groups.last_mut().unwrap();
            if used + need <= cap || cur.is_empty() {
                used += need;
                cur.push(c);
                continue;
            }
            if consumes {
                if it.peek().is_none() {
                    // The last cell cannot be a divider (nothing would be
                    // left for the last page); use the previous one.
                    let prev = cur.pop().unwrap();
                    dividers.push(prev);
                    groups.push(vec![c]);
                } else {
                    dividers.push(c);
                    groups.push(Vec::new());
                }
                used = 0;
            } else {
                dividers.push(Vec::new());
                groups.push(vec![c]);
                used = need;
            }
        }
        let n_new = if all_new {
            groups.len()
        } else {
            groups.len() - 1
        };
        let mut parent_cells = Vec::new();
        let last = groups.pop().unwrap();
        for (gi, group) in groups.into_iter().enumerate() {
            let pgno = self.alloc_page()?;
            let d = std::mem::take(&mut dividers[gi]);
            let mut page = Node {
                pgno,
                kind,
                cells: group,
                right: 0,
            };
            let pcell = match kind {
                LEAF_TABLE => {
                    let r = Self::cell_rowid(page.cells.last().unwrap(), kind);
                    let mut c = pgno.to_be_bytes().to_vec();
                    write_varint(r as u64, &mut c);
                    c
                }
                LEAF_INDEX => {
                    let mut c = pgno.to_be_bytes().to_vec();
                    c.extend_from_slice(&d);
                    c
                }
                _ => {
                    // Interior: the divider's child becomes this page's
                    // right-most child; the divider points at this page.
                    page.right = be32(&d, 0);
                    let mut c = pgno.to_be_bytes().to_vec();
                    c.extend_from_slice(&d[4..]);
                    c
                }
            };
            self.write_node(&page)?;
            parent_cells.push(pcell);
        }
        debug_assert!(n_new == parent_cells.len() || all_new);
        node.cells = last;
        if !all_new {
            self.write_node(node)?;
        }
        Ok(parent_cells)
    }

    /// Split the root: its content moves to new pages and the root becomes
    /// an interior page over them, keeping its page number.
    fn split_root(&self, mut node: Node) -> Result<()> {
        let root = node.pgno;
        // Page 1 has less room; split as if the content lived elsewhere.
        let mut dividers = self.split(&mut node, true)?;
        let last_pg = self.alloc_page()?;
        let last = Node {
            pgno: last_pg,
            kind: node.kind,
            cells: std::mem::take(&mut node.cells),
            right: node.right,
        };
        if self.fits(&last) {
            self.write_node(&last)?;
        } else {
            // The remainder alone overflows (only possible on page 1's
            // reduced capacity being inherited): split it normally.
            let mut last = last;
            let more = self.split(&mut last, false)?;
            dividers.extend(more);
        }
        let interior = if node.is_index() {
            INTERIOR_INDEX
        } else {
            INTERIOR_TABLE
        };
        let new_root = Node {
            pgno: root,
            kind: interior,
            cells: dividers,
            right: last_pg,
        };
        if self.fits(&new_root) {
            return self.write_node(&new_root);
        }
        // Too many dividers for one page (huge index keys): push them down
        // a level by splitting the new root as well.
        self.split_root(new_root)
    }

    // ------------------------------------------------------------ delete

    /// Delete the row with `rowid` from a table b-tree. Returns whether it
    /// existed.
    pub fn table_delete(&self, root: u32, rowid: i64) -> Result<bool> {
        self.delete(root, &Key::Rowid(rowid))
    }

    /// Delete the entry equal to `record` on its first `n` fields.
    pub fn index_delete(
        &self,
        root: u32,
        record: &[u8],
        order: &KeyOrder,
        n: usize,
    ) -> Result<bool> {
        let fields = decode_all(record, self.enc)?;
        self.delete(
            root,
            &Key::Entry {
                fields: &fields,
                order,
                n,
            },
        )
    }

    fn delete(&self, root: u32, key: &Key) -> Result<bool> {
        let default = KeyOrder::default();
        let order = match key {
            Key::Entry { order, .. } => *order,
            Key::Rowid(_) => &default,
        };
        let mut path: Vec<(Node, usize)> = Vec::new();
        let mut node = self.read_node(root)?;
        loop {
            let (i, eq) = self.search(&node, key)?;
            if eq && node.is_leaf() {
                let old = node.cells.remove(i);
                self.free_cell_overflow(&old, node.kind)?;
                self.after_delete(path, node, root, order)?;
                return Ok(true);
            }
            if eq && node.kind == INTERIOR_INDEX {
                return self
                    .delete_interior_entry(path, node, i, root, order)
                    .map(|_| true);
            }
            if node.is_leaf() {
                return Ok(false);
            }
            let child = node.child(i);
            path.push((node, i));
            node = self.read_node(child)?;
        }
    }

    /// Remove index entry `i` of an interior page: take the largest entry
    /// of its left subtree out of its leaf and put it in the entry's place.
    fn delete_interior_entry(
        &self,
        path: Vec<(Node, usize)>,
        mut node: Node,
        i: usize,
        root: u32,
        order: &KeyOrder,
    ) -> Result<()> {
        // Find the predecessor: right-most leaf entry under child(i).
        let mut sub_path: Vec<(Node, usize)> = Vec::new();
        let mut leaf = self.read_node(node.child(i))?;
        while !leaf.is_leaf() {
            let n = leaf.cells.len();
            let next = leaf.child(n);
            sub_path.push((leaf, n));
            leaf = self.read_node(next)?;
        }
        let pred = leaf
            .cells
            .pop()
            .ok_or_else(|| Error::Corrupt(format!("index page {} is empty", leaf.pgno)))?;
        let old = std::mem::take(&mut node.cells[i]);
        self.free_cell_overflow(&old[4..], LEAF_INDEX)?;
        let mut cell = old[..4].to_vec();
        cell.extend_from_slice(&pred);
        node.cells[i] = cell;
        // Write the interior page first (it may split), then fix the leaf.
        // The leaf sits below `node`, so finish the leaf before the page
        // above it changes shape: do the leaf with a full path.
        let mut full: Vec<(Node, usize)> = path;
        full.push((node, i));
        full.extend(sub_path);
        self.after_delete(full, leaf, root, order)
    }

    /// Store a node that lost a cell; free and unlink it if it is empty.
    fn after_delete(
        &self,
        mut path: Vec<(Node, usize)>,
        node: Node,
        root: u32,
        order: &KeyOrder,
    ) -> Result<()> {
        if !node.cells.is_empty() || node.pgno == root {
            if node.cells.is_empty() && !node.is_leaf() {
                return self.collapse_root(node);
            }
            // Ancestors may hold a changed cell (an interior entry swap);
            // store from the bottom up.
            return self.store_path(path, node, root);
        }
        if !node.is_leaf() {
            // An interior page with no cells, only a right-most child. It
            // cannot simply be replaced by that child (the subtree would be
            // one level shorter than its neighbours), so it merges with a
            // sibling, pulling the parent's divider down between them.
            let (mut parent, i) = path.pop().unwrap();
            let mut merged = if i > 0 {
                let sib = self.read_node(parent.child(i - 1))?;
                let d = parent.cells.remove(i - 1);
                let mut mid = sib.right.to_be_bytes().to_vec();
                mid.extend_from_slice(&d[4..]);
                let mut cells = sib.cells;
                cells.push(mid);
                self.free_page(sib.pgno)?;
                Node {
                    pgno: node.pgno,
                    kind: node.kind,
                    cells,
                    right: node.right,
                }
            } else {
                let sib = self.read_node(parent.child(1))?;
                let d = parent.cells.remove(0);
                let mut first = node.right.to_be_bytes().to_vec();
                first.extend_from_slice(&d[4..]);
                let mut cells = vec![first];
                cells.extend(sib.cells);
                self.free_page(node.pgno)?;
                Node {
                    pgno: sib.pgno,
                    kind: sib.kind,
                    cells,
                    right: sib.right,
                }
            };
            let at = i.saturating_sub(1);
            if parent.cells.is_empty() && parent.pgno == root {
                // The parent was the root with this one divider: the merged
                // page's content becomes the root (the whole tree shrinks).
                self.free_page(merged.pgno)?;
                merged.pgno = root;
                return if self.fits(&merged) {
                    self.write_node(&merged)
                } else {
                    self.split_root(merged)
                };
            }
            if self.fits(&merged) {
                self.write_node(&merged)?;
            } else {
                let dividers = self.split(&mut merged, false)?;
                for (k, d) in dividers.into_iter().enumerate() {
                    parent.cells.insert(at + k, d);
                }
            }
            return self.after_delete(path, parent, root, order);
        }
        // An empty leaf: unlink it from its parent.
        let (mut parent, i) = path.pop().unwrap();
        self.free_page(node.pgno)?;
        let mut reinsert: Option<Vec<u8>> = None;
        if i < parent.cells.len() {
            let removed = parent.cells.remove(i);
            if parent.kind == INTERIOR_INDEX {
                reinsert = Some(removed[4..].to_vec());
            }
        } else {
            let removed = parent.cells.pop().unwrap();
            parent.right = be32(&removed, 0);
            if parent.kind == INTERIOR_INDEX {
                reinsert = Some(removed[4..].to_vec());
            }
        }
        self.after_delete(path, parent, root, order)?;
        if let Some(cell) = reinsert {
            // The entry that divided the freed leaf from its neighbour is
            // a real index entry: put it back (keeping its overflow chain).
            let payload = self.cell_payload(&cell, LEAF_INDEX)?;
            let fields = decode_all(&payload, self.enc)?;
            let key = Key::Entry {
                fields: &fields,
                order,
                n: fields.len(),
            };
            self.insert(root, &key, cell, true)?;
        }
        Ok(())
    }

    /// Write `node` and every ancestor on `path` (each may have changed).
    fn store_path(&self, path: Vec<(Node, usize)>, node: Node, root: u32) -> Result<()> {
        // Store bottom-up; a split at one level adds cells to the next.
        let mut path = path;
        let mut node = node;
        loop {
            if self.fits(&node) {
                self.write_node(&node)?;
                match path.pop() {
                    Some((parent, _)) => {
                        node = parent;
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            if node.pgno == root {
                return self.split_root(node);
            }
            let dividers = self.split(&mut node, false)?;
            let (mut parent, i) = path.pop().expect("a non-root page has a parent");
            for (k, d) in dividers.into_iter().enumerate() {
                parent.cells.insert(i + k, d);
            }
            node = parent;
        }
    }

    /// The root is an interior page with no cells: copy its only child's
    /// content into the root page (repeatedly) and free the child.
    fn collapse_root(&self, mut root: Node) -> Result<()> {
        while !root.is_leaf() && root.cells.is_empty() {
            let child = self.read_node(root.right)?;
            self.free_page(child.pgno)?;
            root = Node {
                pgno: root.pgno,
                kind: child.kind,
                cells: child.cells,
                right: child.right,
            };
        }
        if self.fits(&root) {
            self.write_node(&root)
        } else {
            // Page 1 holds 100 bytes less than the child did.
            self.split_root(root)
        }
    }
}

/// Read the 2-byte big-endian value at `off` (re-exported for callers that
/// inspect page headers).
pub fn page_u16(page: &[u8], off: usize) -> usize {
    be16(page, off)
}
