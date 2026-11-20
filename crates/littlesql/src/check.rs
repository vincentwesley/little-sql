//! `PRAGMA integrity_check`: a structural check of the whole file.
//!
//! * Every page is used exactly once: by a b-tree, an overflow chain or
//!   the freelist (the lock-byte page excepted).
//! * Every b-tree page has a valid type for its tree, cells inside the page
//!   that do not overlap the header or each other, and keys in order and
//!   within the range its parent allows; all leaves are at the same depth.
//! * Overflow chains are as long as their payloads need.
//! * The freelist holds as many pages as the header says.
//! * Every index has exactly one entry per row of its table, and every
//!   entry points at an existing row.
//!
//! Messages follow SQLite's wording where there is one.

use crate::btree::{
    be16, be32, parse_cell, read_payload, Cursor, PageRef, INTERIOR_INDEX, INTERIOR_TABLE,
    LEAF_INDEX, LEAF_TABLE,
};
use crate::db::Database;
use crate::error::Result;
use crate::pager::Pager;
use crate::record::decode_all;
use crate::schema::{Index, Table};
use crate::value::{Collation, Value};
use std::cmp::Ordering;

const MAX_PROBLEMS: usize = 100;

struct Checker<'p> {
    pager: &'p Pager,
    usable: usize,
    /// For each page (index = page number), what uses it.
    owner: Vec<Option<String>>,
    problems: Vec<String>,
}

impl Checker<'_> {
    fn problem(&mut self, msg: String) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(msg);
        }
    }

    /// Mark a page as used; false if it was already used or out of range.
    fn claim(&mut self, pgno: u32, what: &str) -> bool {
        let n = self.owner.len() as u32 - 1;
        if pgno == 0 || pgno > n {
            self.problem(format!("{what}: invalid page number {pgno}"));
            return false;
        }
        if let Some(prev) = &self.owner[pgno as usize] {
            let prev = prev.clone();
            self.problem(format!(
                "2nd reference to page {pgno} ({what}; first by {prev})"
            ));
            return false;
        }
        self.owner[pgno as usize] = Some(what.to_string());
        true
    }

    fn overflow(&mut self, first: u32, mut remaining: usize, what: &str) {
        let mut next = first;
        let per = self.usable - 4;
        while next != 0 && remaining > 0 {
            if !self.claim(next, &format!("overflow of {what}")) {
                return;
            }
            let Ok(page) = self.pager.get(next) else {
                self.problem(format!("{what}: cannot read overflow page {next}"));
                return;
            };
            remaining = remaining.saturating_sub(per);
            next = be32(&page, 0);
        }
        if remaining > 0 {
            self.problem(format!("{what}: overflow chain is too short"));
        } else if next != 0 {
            self.problem(format!("{what}: extends off end of overflow chain"));
        }
    }

    /// Walk a b-tree. `order` compares index entries; table trees compare
    /// rowids. Returns the leaf depth.
    #[allow(clippy::too_many_arguments)]
    fn tree(
        &mut self,
        pgno: u32,
        depth: usize,
        is_index: bool,
        order: &dyn Fn(&[Value], &[Value]) -> Ordering,
        lo: Option<Bound>,
        hi: Option<Bound>,
        what: &str,
        leaf_depth: &mut Option<usize>,
    ) {
        if depth > 40 {
            self.problem(format!("{what}: b-tree is too deep (a loop?)"));
            return;
        }
        if !self.claim(pgno, what) {
            return;
        }
        let page = match PageRef::load(self.pager, pgno) {
            Ok(p) => p,
            Err(e) => {
                self.problem(format!("{what}: page {pgno}: {e}"));
                return;
            }
        };
        let kind = page.kind();
        let ok_kind = if is_index {
            matches!(kind, INTERIOR_INDEX | LEAF_INDEX)
        } else {
            matches!(kind, INTERIOR_TABLE | LEAF_TABLE)
        };
        if !ok_kind {
            self.problem(format!(
                "{what}: page {pgno} has the wrong b-tree page type {kind}"
            ));
            return;
        }
        let n = page.ncells();
        let content_floor = page.cell_array() + 2 * n;
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(n);
        let mut keys: Vec<Bound> = Vec::with_capacity(n);
        for i in 0..n {
            let off = be16(&page.data, page.cell_array() + 2 * i);
            if off < content_floor || off >= self.usable {
                self.problem(format!(
                    "{what}: page {pgno} cell {i}: offset {off} out of range"
                ));
                return;
            }
            let cell = match parse_cell(&page.data, off, kind, self.usable) {
                Ok(c) => c,
                Err(e) => {
                    self.problem(format!("{what}: page {pgno} cell {i}: {e}"));
                    return;
                }
            };
            if off + cell.size > self.usable {
                self.problem(format!(
                    "{what}: page {pgno} cell {i} extends past the page"
                ));
                return;
            }
            spans.push((off, off + cell.size));
            if cell.overflow != 0 {
                let rest = cell.payload_size - cell.local_len;
                self.overflow(cell.overflow, rest, what);
            }
            let key = if is_index {
                match read_payload(self.pager, &page, &cell)
                    .and_then(|p| decode_all(p.as_slice(), self.pager.encoding()))
                {
                    Ok(v) => Bound::Entry(v),
                    Err(e) => {
                        self.problem(format!("{what}: page {pgno} cell {i}: {e}"));
                        return;
                    }
                }
            } else {
                Bound::Rowid(cell.rowid)
            };
            keys.push(key);
        }
        spans.sort_unstable();
        for w in spans.windows(2) {
            if w[0].1 > w[1].0 {
                self.problem(format!("{what}: page {pgno}: cells overlap"));
                break;
            }
        }
        // Free space: the gap, the freeblock list and the fragment count
        // must account for every byte not used by cells (as SQLite checks).
        let hdr = page.hdr();
        let content = match be16(&page.data, hdr + 5) {
            0 => 65536,
            c => c,
        };
        if content < content_floor || content > self.usable {
            self.problem(format!(
                "{what}: page {pgno}: free space corruption (content start)"
            ));
            return;
        }
        let mut free = content - content_floor + page.data[hdr + 7] as usize;
        let mut fb = be16(&page.data, hdr + 1);
        let mut last_end = 0usize;
        let mut hops = 0;
        while fb != 0 {
            hops += 1;
            let size = if fb + 4 <= self.usable {
                be16(&page.data, fb + 2)
            } else {
                0
            };
            if fb < content
                || fb + size > self.usable
                || size < 4
                || fb < last_end + 4
                || hops > 10_000
            {
                self.problem(format!(
                    "{what}: page {pgno}: free space corruption (freeblock list)"
                ));
                return;
            }
            free += size;
            last_end = fb + size;
            fb = be16(&page.data, fb);
        }
        let in_cells: usize = spans.iter().map(|(a, b)| b - a).sum();
        if content_floor + free + in_cells != self.usable {
            self.problem(format!(
                "{what}: page {pgno}: free space corruption ({} bytes unaccounted for)",
                self.usable as isize - (content_floor + free + in_cells) as isize
            ));
        }
        // Keys: in order, and inside (lo, hi].
        let cmp = |a: &Bound, b: &Bound| -> Ordering {
            match (a, b) {
                (Bound::Rowid(x), Bound::Rowid(y)) => x.cmp(y),
                (Bound::Entry(x), Bound::Entry(y)) => order(x, y),
                _ => Ordering::Equal,
            }
        };
        let leaf = page.is_leaf();
        for i in 0..keys.len() {
            if i > 0 {
                let o = cmp(&keys[i - 1], &keys[i]);
                if o == Ordering::Greater || (o == Ordering::Equal && (is_index || leaf)) {
                    self.problem(format!(
                        "{what}: page {pgno}: keys out of order at cell {i}"
                    ));
                    break;
                }
            }
            if let Some(l) = &lo {
                if cmp(&keys[i], l) != Ordering::Greater {
                    self.problem(format!("{what}: page {pgno}: key below its parent's range"));
                    break;
                }
            }
            if let Some(h) = &hi {
                let o = cmp(&keys[i], h);
                // Table dividers are inclusive upper bounds.
                if o == Ordering::Greater || (is_index && o == Ordering::Equal) {
                    self.problem(format!("{what}: page {pgno}: key above its parent's range"));
                    break;
                }
            }
        }
        if leaf {
            match *leaf_depth {
                None => *leaf_depth = Some(depth),
                Some(d) if d != depth => self.problem(format!(
                    "{what}: leaves at different depths ({d} and {depth})"
                )),
                _ => {}
            }
            return;
        }
        for i in 0..=n {
            let child = match page.child(i) {
                Ok(c) => c,
                Err(e) => {
                    self.problem(format!("{what}: page {pgno}: {e}"));
                    return;
                }
            };
            let clo = if i == 0 {
                lo.clone()
            } else {
                Some(keys[i - 1].clone())
            };
            let chi = if i == n {
                hi.clone()
            } else {
                Some(keys[i].clone())
            };
            self.tree(
                child,
                depth + 1,
                is_index,
                order,
                clo,
                chi,
                what,
                leaf_depth,
            );
        }
    }

    fn freelist(&mut self, first: u32, expected: u32) {
        let mut trunk = first;
        let mut count = 0u32;
        let max_leaves = (self.usable / 4 - 2) as u32;
        while trunk != 0 {
            if !self.claim(trunk, "freelist trunk") {
                return;
            }
            count += 1;
            let Ok(p) = self.pager.get(trunk) else {
                self.problem(format!("cannot read freelist trunk {trunk}"));
                return;
            };
            let n = be32(&p, 4);
            if n > max_leaves {
                self.problem(format!("freelist trunk {trunk} lists too many pages ({n})"));
                return;
            }
            for k in 0..n as usize {
                let leaf = be32(&p, 8 + 4 * k);
                if self.claim(leaf, "freelist leaf") {
                    count += 1;
                }
            }
            trunk = be32(&p, 0);
        }
        if count != expected {
            self.problem(format!(
                "freelist count is {expected} in the header but {count} pages are listed"
            ));
        }
    }
}

#[derive(Clone)]
enum Bound {
    Rowid(i64),
    Entry(Vec<Value>),
}

fn index_order(index: &Index, table: &Table) -> impl Fn(&[Value], &[Value]) -> Ordering {
    let mut fields: Vec<(Collation, bool)> =
        index.cols.iter().map(|c| (c.collation, c.desc)).collect();
    if let Some(pk) = &table.pk_index {
        if !index.primary {
            for e in &index.extra {
                if let Some(p) = pk.cols.iter().find(|p| p.col == *e) {
                    fields.push((p.collation, p.desc));
                }
            }
        }
    }
    let nkey = if index.primary {
        index.cols.len()
    } else {
        usize::MAX
    };
    move |a: &[Value], b: &[Value]| {
        let n = a.len().min(b.len()).min(nkey);
        for i in 0..n {
            let (coll, desc) = fields.get(i).copied().unwrap_or((Collation::Binary, false));
            let o = a[i].compare(&b[i], coll);
            if o != Ordering::Equal {
                return if desc { o.reverse() } else { o };
            }
        }
        Ordering::Equal
    }
}

/// Returns a list of problems; empty means the database is consistent.
pub fn integrity_check(db: &Database) -> Result<Vec<String>> {
    let pager = &db.pager;
    let n = pager.page_count();
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut c = Checker {
        pager,
        usable: pager.usable_size(),
        owner: vec![None; n as usize + 1],
        problems: Vec::new(),
    };
    let p1 = pager.get(1)?;
    c.freelist(be32(&p1, 32), be32(&p1, 36));
    let schema = db.schema();
    let none = |_: &[Value], _: &[Value]| Ordering::Equal;
    let mut depth = None;
    c.tree(1, 0, false, &none, None, None, "sqlite_schema", &mut depth);
    let mut tables: Vec<&std::rc::Rc<Table>> = Vec::new();
    for r in &schema.rows {
        if r.kind == "table" && r.root > 0 {
            if let Some(t) = schema.table(&r.name) {
                tables.push(t);
            }
        }
    }
    for t in &tables {
        let mut depth = None;
        if t.without_rowid {
            let pk = t.pk_index.as_ref().unwrap();
            let order = index_order(pk, t);
            c.tree(
                t.root,
                0,
                true,
                &order,
                None,
                None,
                &format!("table {}", t.name),
                &mut depth,
            );
        } else {
            c.tree(
                t.root,
                0,
                false,
                &none,
                None,
                None,
                &format!("table {}", t.name),
                &mut depth,
            );
        }
        for ix in &t.indexes {
            let mut depth = None;
            let order = index_order(ix, t);
            c.tree(
                ix.root,
                0,
                true,
                &order,
                None,
                None,
                &format!("index {}", ix.name),
                &mut depth,
            );
        }
    }
    // Pages nobody uses.
    let lock_page = (0x4000_0000u64 / pager.page_size() as u64) as u32 + 1;
    for p in 1..=n {
        if c.owner[p as usize].is_none() && p != lock_page {
            c.problem(format!("Page {p} is never used"));
        }
    }
    // Indexes against their tables (only when the trees themselves are
    // sound, since a broken tree makes the counts meaningless).
    if c.problems.is_empty() {
        for t in &tables {
            let rows = crate::btree::count_entries(pager, t.root)?;
            for ix in t.indexes.iter().filter(|i| i.usable) {
                let entries = crate::btree::count_entries(pager, ix.root)?;
                if entries != rows {
                    c.problem(format!("wrong # of entries in index {}", ix.name));
                    continue;
                }
                if t.without_rowid {
                    continue;
                }
                let mut ic = Cursor::new(pager.clone(), ix.root);
                let mut tc = Cursor::new(pager.clone(), t.root);
                ic.first()?;
                while ic.valid() {
                    let vals = decode_all(ic.payload()?.as_slice(), pager.encoding())?;
                    match vals.last() {
                        Some(Value::Int(rowid)) => {
                            tc.seek_rowid(*rowid)?;
                            if !tc.valid() || tc.rowid()? != *rowid {
                                c.problem(format!(
                                    "row {rowid} missing from table {} but found in index {}",
                                    t.name, ix.name
                                ));
                                break;
                            }
                        }
                        _ => {
                            c.problem(format!("index {} has an entry without a rowid", ix.name));
                            break;
                        }
                    }
                    ic.next()?;
                }
            }
        }
    }
    Ok(c.problems)
}
