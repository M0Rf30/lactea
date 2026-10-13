//! megabuf / gmegabuf / reg00..reg99 storage (MemoryBuffer.c, TreeVariables.c).

use crate::ns::Dialect;
use crate::rng::{Mt, PROJECTM_DEFAULT_SEED};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};

pub(crate) const BLOCKS: usize = 128;
pub(crate) const BLOCK_LEN: usize = 65536;
pub(crate) const BLOCK_MASK: usize = BLOCK_LEN - 1;
/// Number of addressable megabuf / gmegabuf slots (`128 * 65536`).
pub const MEM_ITEMS: usize = BLOCKS * BLOCK_LEN;

/// State shared by every [`Context`](crate::Context) of one engine: `gmegabuf`, the
/// `reg00..reg99` globals and the `rand()` generator, plus the [`Dialect`] they all run.
///
/// Registers, gmegabuf and the `rand()` generator are relaxed atomics (no `unsafe`; a plain
/// load/store on mainstream targets, no lock).  The generator is one stream shared by all
/// memories created with [`sharing_rng_with`](Self::sharing_rng_with); the contexts that share
/// it run one at a time (drawing from several threads at once is memory-safe but the values are
/// unspecified).  The type is `Send + Sync`.
pub struct GlobalMemory {
    dialect: Dialect,
    regs: Arc<[AtomicU64; 100]>,
    blocks: Arc<Vec<OnceLock<Box<[AtomicU64]>>>>,
    rng: Arc<Mt>,
}

impl Default for GlobalMemory {
    fn default() -> Self {
        Self::new()
    }
}

fn new_regs() -> Arc<[AtomicU64; 100]> {
    Arc::new(std::array::from_fn(|_| AtomicU64::new(0)))
}

fn new_blocks() -> Arc<Vec<OnceLock<Box<[AtomicU64]>>>> {
    Arc::new((0..BLOCKS).map(|_| OnceLock::new()).collect())
}

impl GlobalMemory {
    /// Empty memory with its own `rand()` stream seeded with [`PROJECTM_DEFAULT_SEED`],
    /// for [`Dialect::ProjectMEval`].
    pub fn new() -> Self {
        Self::with_dialect(Dialect::ProjectMEval)
    }

    /// Like [`new`](Self::new) for the given dialect.  Every [`Context`](crate::Context) created
    /// on this memory compiles and runs that dialect.
    pub fn with_dialect(dialect: Dialect) -> Self {
        GlobalMemory {
            dialect,
            regs: new_regs(),
            blocks: new_blocks(),
            rng: Arc::new(Mt::new(PROJECTM_DEFAULT_SEED)),
        }
    }

    /// The dialect of the contexts built on this memory.
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Memory that draws `rand()` from the **same** stream as `other` and runs its dialect.
    ///
    /// projectm-eval's generator is one process-wide static: every preset loaded in a process
    /// (also two presets blending during a transition) advances the same sequence.  There
    /// `reg00..reg99` and gmegabuf are per preset (fresh here).  ns-eel2 keeps them as
    /// process-wide statics too, never cleared by a preset change, so for
    /// [`Dialect::NsEel2`] they are shared with `other` as well.
    pub fn sharing_rng_with(other: &GlobalMemory) -> Self {
        let (regs, blocks) = match other.dialect {
            Dialect::ProjectMEval => (new_regs(), new_blocks()),
            Dialect::NsEel2 => (Arc::clone(&other.regs), Arc::clone(&other.blocks)),
        };
        GlobalMemory {
            dialect: other.dialect,
            regs,
            blocks,
            rng: Arc::clone(&other.rng),
        }
    }

    /// Memory of `dialect` that draws `rand()` from the **same** stream as `other` (like
    /// [`sharing_rng_with`](Self::sharing_rng_with)) but starts with its own, empty `reg00..reg99` and gmegabuf: a
    /// host that switches the dialect mid-run keeps one random sequence and loses the state of the other dialect.
    pub fn sharing_rng_with_dialect(other: &GlobalMemory, dialect: Dialect) -> Self {
        GlobalMemory {
            dialect,
            regs: new_regs(),
            blocks: new_blocks(),
            rng: Arc::clone(&other.rng),
        }
    }

    /// Value of `reg00 + i`; `i >= 100` reads 0 (like [`gmem`](Self::gmem) out of range).
    pub fn reg(&self, i: usize) -> f64 {
        self.regs
            .get(i)
            .map_or(0.0, |r| f64::from_bits(r.load(Relaxed)))
    }

    /// Sets `reg00 + i`; `i >= 100` is ignored.
    pub fn set_reg(&self, i: usize, v: f64) {
        if let Some(r) = self.regs.get(i) {
            r.store(v.to_bits(), Relaxed);
        }
    }

    /// Reads gmegabuf slot `index` (does not allocate; unallocated / out of range reads 0).
    pub fn gmem(&self, index: usize) -> f64 {
        match self.blocks.get(index >> 16).and_then(|b| b.get()) {
            Some(b) => f64::from_bits(b[index & BLOCK_MASK].load(Relaxed)),
            None => 0.0,
        }
    }

    /// Writes gmegabuf slot `index`; out of range writes are ignored.
    pub fn set_gmem(&self, index: usize, v: f64) {
        if let Some(cell) = self.blocks.get(index >> 16) {
            let b = cell.get_or_init(alloc_block);
            b[index & BLOCK_MASK].store(v.to_bits(), Relaxed);
        }
    }

    /// Zeroes gmegabuf and reg00..reg99.
    pub fn reset(&self) {
        for r in self.regs.iter() {
            r.store(0, Relaxed);
        }
        for b in self.blocks.iter() {
            if let Some(b) = b.get() {
                for c in b.iter() {
                    c.store(0, Relaxed);
                }
            }
        }
    }

    /// Reseeds the shared `rand()` stream (see [`Context::set_rng_seed`](crate::Context::set_rng_seed)).
    pub fn set_rng_seed(&self, seed: u64) {
        self.rng.reseed(fold_seed(seed));
    }

    #[inline]
    pub(crate) fn rand_u32(&self) -> u32 {
        self.rng.next_u32()
    }

    /// Block `b`, allocated (512 KiB) on first access.  Precondition: `b < BLOCKS`; callers
    /// take indices from `mem_index`, which rejects every other value.
    #[inline]
    pub(crate) fn block(&self, b: usize) -> &[AtomicU64] {
        self.blocks[b].get_or_init(alloc_block)
    }

    /// [`gmem`](Self::gmem) for an index already validated by `mem_index`.
    #[inline]
    pub(crate) fn gmem_get(&self, idx: u32) -> f64 {
        self.gmem(idx as usize)
    }
}

pub(crate) fn fold_seed(seed: u64) -> u32 {
    (seed ^ (seed >> 32)) as u32
}

fn alloc_block() -> Box<[AtomicU64]> {
    (0..BLOCK_LEN).map(|_| AtomicU64::new(0)).collect()
}

/// Per-context megabuf: 128 lazily allocated blocks of 65536 doubles.
///
/// Index arguments of `get`/`set`/`ensure` must already be validated by `mem_index` (block
/// index `< BLOCKS`); anything else is a caller bug and panics.
pub(crate) struct LocalMem {
    blocks: Vec<Option<Box<[f64]>>>,
}

impl LocalMem {
    pub(crate) fn new() -> Self {
        LocalMem {
            blocks: (0..BLOCKS).map(|_| None).collect(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.blocks.fill(None);
    }

    #[inline]
    pub(crate) fn get(&self, idx: u32) -> f64 {
        let idx = idx as usize;
        match &self.blocks[idx >> 16] {
            Some(b) => b[idx & BLOCK_MASK],
            None => 0.0,
        }
    }

    #[inline]
    pub(crate) fn set(&mut self, idx: u32, v: f64) {
        let idx = idx as usize;
        let b = self.blocks[idx >> 16].get_or_insert_with(alloc_local_block);
        b[idx & BLOCK_MASK] = v;
    }

    /// Read helper for embedders / tests: out of range or unallocated reads 0.
    pub(crate) fn peek(&self, idx: usize) -> f64 {
        match self.blocks.get(idx >> 16) {
            Some(Some(b)) => b[idx & BLOCK_MASK],
            _ => 0.0,
        }
    }

    /// `prjm_eval_memory_copy`; offsets already converted with `(int32_t)(x + 0.0001)`.
    pub(crate) fn copy(&mut self, mut dest: i32, mut src: i32, mut count: i32) {
        if src < 0 {
            count = count.wrapping_add(src);
            dest = dest.wrapping_sub(src);
            src = 0;
        }
        if dest < 0 {
            count = count.wrapping_add(dest);
            src = src.wrapping_sub(dest);
            dest = 0;
        }
        let total = MEM_ITEMS as i32;
        while count > 0 {
            let mut copy_len = count;
            let max_dst = BLOCK_LEN as i32 - (dest & BLOCK_MASK as i32);
            let max_src = BLOCK_LEN as i32 - (src & BLOCK_MASK as i32);
            if dest >= total || src >= total {
                break;
            }
            copy_len = copy_len.min(max_dst).min(max_src);
            if copy_len < 1 {
                break;
            }
            let (sb, so) = ((src as usize) >> 16, src as usize & BLOCK_MASK);
            let (db, d_o) = ((dest as usize) >> 16, dest as usize & BLOCK_MASK);
            let n = copy_len as usize;
            // An unallocated source reads as zeros: zero the destination if it exists, but
            // never allocate a block only to hold zeros.
            if sb == db {
                if let Some(b) = self.blocks[sb].as_mut() {
                    b.copy_within(so..so + n, d_o);
                }
            } else {
                // Distinct blocks never overlap: borrow both through one split.
                let (lo, hi) = self.blocks.split_at_mut(sb.max(db));
                let (s, d) = if sb < db {
                    (&lo[sb], &mut hi[0])
                } else {
                    (&hi[0], &mut lo[db])
                };
                match s {
                    Some(s) => d.get_or_insert_with(alloc_local_block)[d_o..d_o + n]
                        .copy_from_slice(&s[so..so + n]),
                    None => {
                        if let Some(d) = d {
                            d[d_o..d_o + n].fill(0.0);
                        }
                    }
                }
            }
            src += copy_len;
            dest += copy_len;
            count -= copy_len;
        }
    }

    /// `prjm_eval_memory_set`.
    pub(crate) fn fill(&mut self, mut dest: i32, mut count: i32, val: f64) {
        if dest < 0 {
            count = count.wrapping_add(dest);
            dest = 0;
        }
        let total = MEM_ITEMS as i64;
        if dest as i64 >= total {
            return;
        }
        if dest as i64 + count as i64 > total {
            count = (total - dest as i64) as i32;
        }
        if count < 1 {
            return;
        }
        // Zero fill of an unallocated block changes nothing: skip the 512 KiB allocation.
        let zero = val.to_bits() == 0;
        while count > 0 {
            let blk = (dest as usize) >> 16;
            let off = dest as usize & BLOCK_MASK;
            let mut n = BLOCK_LEN - off;
            if n as i32 > count {
                n = count as usize;
            }
            let slot = &mut self.blocks[blk];
            if !zero || slot.is_some() {
                slot.get_or_insert_with(alloc_local_block)[off..off + n].fill(val);
            }
            count -= n as i32;
            dest += n as i32;
        }
    }
}

fn alloc_local_block() -> Box<[f64]> {
    vec![0.0; BLOCK_LEN].into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocated(m: &LocalMem) -> usize {
        m.blocks.iter().flatten().count()
    }

    #[test]
    fn zero_traffic_does_not_allocate() {
        let mut m = LocalMem::new();
        assert_eq!(m.get(70_000), 0.0);
        m.fill(0, 200_000, 0.0);
        m.copy(70_000, 0, 1000);
        assert_eq!(allocated(&m), 0);
        m.set(5, 3.0);
        m.copy(65_536, 70_000, 4); // unallocated source, unallocated destination
        assert_eq!(allocated(&m), 1);
        m.copy(0, 70_000, 8); // unallocated source zeroes an allocated destination
        assert_eq!(m.peek(5), 0.0);
        assert_eq!(allocated(&m), 1);
    }

    #[test]
    fn copy_and_fill_still_move_values() {
        let mut m = LocalMem::new();
        m.fill(65_530, 12, 2.5);
        m.copy(200_000, 65_530, 12);
        assert_eq!(m.peek(200_000), 2.5);
        assert_eq!(m.peek(200_011), 2.5);
        assert_eq!(m.peek(200_012), 0.0);
    }
}
