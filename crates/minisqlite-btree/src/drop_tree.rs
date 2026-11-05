//! `drop_btree`: release EVERY page of a whole b-tree to the freelist, the storage
//! half of `DROP TABLE` / `DROP INDEX` (fileformat2 §1.5 freelist, §1.6 b-tree pages,
//! §1.7 overflow pages).
//!
//! Once a table or index's `sqlite_schema` row is gone, nothing references its
//! b-tree any more. Real sqlite frees all of those pages in the same statement
//! (`btreeDropTable` -> `clearDatabasePage`), and its `PRAGMA integrity_check` treats
//! a page that is neither in a b-tree nor on the freelist as corruption ("Page N:
//! never used"). So a dropped object's root, interior, leaf and overflow pages must
//! all land on the freelist, where a later `allocate_page` reuses them before the file
//! grows.
//!
//! ## Shape
//! The walk is split from the frees, as in `table_delete`:
//! 1. A READ-ONLY traversal collects every page id of the tree in the order sqlite's
//!    `clearDatabasePage` frees them: for each cell, the left child's subtree, then the
//!    cell's overflow chain; then the right-most child's subtree; then the page itself.
//!    The root therefore comes last.
//! 2. Only after the traversal (no page borrow live) is each collected page handed to
//!    `Pager::free_page`. `free_page` may turn a freed page into a freelist trunk and
//!    overwrite it, so no freed page may be read afterwards; collecting first rules that
//!    out.
//!
//! ## Fail closed
//! The traversal is bounded and validates as it goes: every page must be in
//! `2..=page_count`, every b-tree page must belong to the root's family (table or
//! index), and no page may be reached twice (a cycle or a page shared between two
//! pointers). Any violation is a format error BEFORE a single page is freed, so a
//! corrupt tree is never half-freed or double-freed; the caller's transaction rolls
//! back on the error. The worklist holds one frame per level of the current
//! root-to-leaf path, so a deep corrupt chain cannot exhaust the native stack. The
//! visited set and the collected id list are O(pages of the dropped tree).

use std::collections::HashSet;

use minisqlite_fileformat::{parse_overflow_page, PageView};
use minisqlite_pager::{PageId, Pager};
use minisqlite_types::{Error, Result};

use crate::tree::usable_of;

/// One step of a page's free order: descend into a child subtree, or release one
/// cell's overflow chain.
#[derive(Clone, Copy)]
enum Step {
    Child(PageId),
    Overflow(PageId),
}

/// A page whose subtree is being collected: the steps still to run, then the page.
struct Frame {
    id: PageId,
    steps: Vec<Step>,
    next: usize,
}

/// Free every page of the b-tree rooted at `root` (root, interior, leaf, and every
/// cell's overflow chain) to the freelist, and return how many pages were freed.
/// Works for both families: a rowid table b-tree and an index b-tree (a secondary
/// index, an auto-index, or a WITHOUT ROWID table's PRIMARY KEY tree).
///
/// Runs inside the caller's transaction (it does not begin/commit), so a ROLLBACK
/// restores the tree and the freelist. The caller must already have removed every
/// reference to `root` (its `sqlite_schema` row); the tree is unusable afterwards.
/// Page 1 (the schema table's root) can never be dropped.
pub fn drop_btree(pager: &mut dyn Pager, root: PageId) -> Result<u32> {
    let pages = collect_tree_pages(&*pager, root)?;
    let freed = u32::try_from(pages.len())
        .map_err(|_| Error::format("dropped b-tree has more pages than a database can hold"))?;
    for id in pages {
        pager.free_page(id)?;
    }
    Ok(freed)
}

/// Collect every page of the tree rooted at `root` in sqlite's free order (see the
/// module docs), validating the tree as it goes. Reads only.
fn collect_tree_pages(pager: &dyn Pager, root: PageId) -> Result<Vec<PageId>> {
    let usable = usable_of(pager);
    let page_count = pager.page_count()?;
    if root < 2 || root > page_count {
        return Err(Error::format(format!(
            "cannot drop b-tree rooted at page {root}: not a droppable page (page count {page_count})"
        )));
    }
    let root_is_table = {
        let view = PageView::new(pager.read_page(root)?, root, usable)?;
        view.page_type().is_table()
    };

    let mut visited: HashSet<PageId> = HashSet::new();
    let mut out: Vec<PageId> = Vec::new();
    let mut stack: Vec<Frame> = vec![open_frame(
        pager,
        root,
        root_is_table,
        usable,
        page_count,
        &mut visited,
    )?];

    while let Some(top) = stack.last_mut() {
        if top.next < top.steps.len() {
            let step = top.steps[top.next];
            top.next += 1;
            match step {
                Step::Child(child) => {
                    let frame = open_frame(
                        pager,
                        child,
                        root_is_table,
                        usable,
                        page_count,
                        &mut visited,
                    )?;
                    stack.push(frame);
                }
                Step::Overflow(first) => {
                    collect_overflow_chain(
                        pager,
                        first,
                        usable,
                        page_count,
                        &mut visited,
                        &mut out,
                    )?;
                }
            }
        } else {
            let done = stack.pop().expect("loop guard saw a top frame");
            out.push(done.id);
        }
    }
    Ok(out)
}

/// Read page `id`, check it, and list the steps that free its subtree.
fn open_frame(
    pager: &dyn Pager,
    id: PageId,
    root_is_table: bool,
    usable: usize,
    page_count: PageId,
    visited: &mut HashSet<PageId>,
) -> Result<Frame> {
    claim_page(id, page_count, visited)?;
    let view = PageView::new(pager.read_page(id)?, id, usable)?;
    let page_type = view.page_type();
    if page_type.is_table() != root_is_table {
        return Err(Error::format(format!(
            "b-tree page {id} is not in the same b-tree family as its root"
        )));
    }
    let cells = view.cell_count() as usize;
    let mut steps = Vec::with_capacity(if page_type.is_leaf() {
        0
    } else {
        cells * 2 + 1
    });
    if page_type.is_leaf() {
        for i in 0..cells {
            let overflow = if root_is_table {
                view.table_leaf_cell(i)?.overflow_page
            } else {
                view.index_leaf_cell(i)?.overflow_page
            };
            if let Some(first) = overflow {
                steps.push(Step::Overflow(first));
            }
        }
    } else {
        for i in 0..cells {
            if root_is_table {
                // Table interior cells are bare (child, rowid) separators: no payload.
                steps.push(Step::Child(view.table_interior_cell(i)?.left_child));
            } else {
                let cell = view.index_interior_cell(i)?;
                steps.push(Step::Child(cell.left_child));
                if let Some(first) = cell.overflow_page {
                    steps.push(Step::Overflow(first));
                }
            }
        }
        let right = view.right_most_pointer().ok_or_else(|| {
            Error::format(format!("interior page {id} has no right-most pointer"))
        })?;
        steps.push(Step::Child(right));
    }
    Ok(Frame { id, steps, next: 0 })
}

/// Append an overflow chain's pages (head first) to `out`, checking each page.
fn collect_overflow_chain(
    pager: &dyn Pager,
    first: PageId,
    usable: usize,
    page_count: PageId,
    visited: &mut HashSet<PageId>,
    out: &mut Vec<PageId>,
) -> Result<()> {
    let mut current = first;
    while current != 0 {
        claim_page(current, page_count, visited)?;
        let next = parse_overflow_page(pager.read_page(current)?, usable)?.next;
        out.push(current);
        current = next;
    }
    Ok(())
}

/// Record `id` as part of the tree, rejecting an out-of-range page or one already
/// reached through another pointer (a cycle or shared page: freeing it twice would
/// corrupt the freelist).
fn claim_page(id: PageId, page_count: PageId, visited: &mut HashSet<PageId>) -> Result<()> {
    if id < 2 || id > page_count {
        return Err(Error::format(format!(
            "b-tree points at page {id}, outside 2..={page_count}"
        )));
    }
    if !visited.insert(id) {
        return Err(Error::format(format!(
            "b-tree reaches page {id} twice (cycle or shared page)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_index_btree, create_table_btree, index_insert, init_database, table_insert,
    };
    use minisqlite_fileformat::{encode_record, DatabaseHeader, HEADER_SIZE};
    use minisqlite_pager::MemPager;
    use minisqlite_types::Value;

    fn header(p: &MemPager) -> DatabaseHeader {
        let mut buf = [0u8; HEADER_SIZE];
        buf.copy_from_slice(&p.read_page(1).unwrap()[..HEADER_SIZE]);
        DatabaseHeader::read(&buf).unwrap()
    }

    fn fresh(page_size: u32) -> MemPager {
        let mut p = MemPager::new(page_size);
        init_database(&mut p).unwrap();
        p
    }

    /// A multi-level table b-tree with overflow rows: every page past page 1 belongs to
    /// it, so dropping it must put exactly `page_count - 1` pages on the freelist.
    #[test]
    fn drop_table_btree_frees_every_page_including_overflow() {
        let mut p = fresh(512);
        p.begin().unwrap();
        let root = create_table_btree(&mut p).unwrap();
        for i in 0..200i64 {
            let big = if i % 10 == 0 {
                "z".repeat(2000)
            } else {
                format!("row-{i:04}-{}", "x".repeat(60))
            };
            let rec = encode_record(&[Value::Integer(i), Value::Text(big)]);
            table_insert(&mut p, root, i, &rec).unwrap();
        }
        p.commit().unwrap();
        let count = p.page_count().unwrap();
        assert!(count > 20, "expected a multi-level tree, got {count} pages");
        assert_eq!(header(&p).freelist_count, 0);

        p.begin().unwrap();
        let freed = drop_btree(&mut p, root).unwrap();
        p.commit().unwrap();
        assert_eq!(
            freed,
            count - 1,
            "every page but page 1 belonged to the dropped tree"
        );
        assert_eq!(header(&p).freelist_count, count - 1);
        assert_eq!(
            p.page_count().unwrap(),
            count,
            "freeing never shrinks the file"
        );

        // The freed pages are reused before the file grows.
        p.begin().unwrap();
        let again = create_table_btree(&mut p).unwrap();
        for i in 0..50i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Text("y".repeat(80))]);
            table_insert(&mut p, again, i, &rec).unwrap();
        }
        p.commit().unwrap();
        assert_eq!(
            p.page_count().unwrap(),
            count,
            "a later tree reuses the freed pages"
        );
    }

    /// An index b-tree (interior dividers carry payload and may overflow) frees all of
    /// its pages too.
    #[test]
    fn drop_index_btree_frees_every_page() {
        let mut p = fresh(512);
        p.begin().unwrap();
        let root = create_index_btree(&mut p).unwrap();
        for i in 0..300i64 {
            let key = if i % 25 == 0 {
                "k".repeat(1500) + &i.to_string()
            } else {
                format!("key-{i:05}")
            };
            let rec = encode_record(&[Value::Text(key), Value::Integer(i)]);
            index_insert(&mut p, root, &rec).unwrap();
        }
        p.commit().unwrap();
        let count = p.page_count().unwrap();
        assert!(
            count > 10,
            "expected a multi-level index, got {count} pages"
        );

        p.begin().unwrap();
        assert_eq!(drop_btree(&mut p, root).unwrap(), count - 1);
        p.commit().unwrap();
        assert_eq!(header(&p).freelist_count, count - 1);
    }

    /// Rollback restores the tree and an empty freelist.
    #[test]
    fn rollback_undoes_a_drop() {
        let mut p = fresh(512);
        p.begin().unwrap();
        let root = create_table_btree(&mut p).unwrap();
        for i in 0..100i64 {
            let rec = encode_record(&[Value::Text(format!("r{i}-{}", "x".repeat(50)))]);
            table_insert(&mut p, root, i, &rec).unwrap();
        }
        p.commit().unwrap();

        p.begin().unwrap();
        drop_btree(&mut p, root).unwrap();
        p.rollback().unwrap();
        assert_eq!(header(&p).freelist_count, 0);
        assert_eq!(crate::table_entry_count(&p, root).unwrap(), 100);
    }

    /// Page 1 and out-of-range roots are refused before anything is freed.
    #[test]
    fn refuses_page_one_and_out_of_range_roots() {
        let mut p = fresh(512);
        p.begin().unwrap();
        assert!(drop_btree(&mut p, 1).is_err());
        assert!(drop_btree(&mut p, 99).is_err());
        assert_eq!(header(&p).freelist_count, 0);
        p.rollback().unwrap();
    }

    /// A tree whose interior points at the same child twice is corrupt; it fails closed
    /// with nothing freed instead of double-freeing the shared page.
    #[test]
    fn shared_child_fails_closed_without_freeing() {
        use minisqlite_fileformat::{encode_table_interior_cell, PageBuilder, PageType};
        let mut p = fresh(512);
        p.begin().unwrap();
        let root = create_table_btree(&mut p).unwrap();
        let leaf = create_table_btree(&mut p).unwrap();
        let mut b = PageBuilder::new(512, 512, root, PageType::InteriorTable);
        let mut cell = Vec::new();
        encode_table_interior_cell(leaf, 10, &mut cell);
        assert!(b.add_cell(&cell));
        b.set_right_most_pointer(leaf);
        p.write_page(root, &b.finish()).unwrap();
        assert!(drop_btree(&mut p, root).is_err());
        assert_eq!(header(&p).freelist_count, 0);
        p.rollback().unwrap();
    }
}
