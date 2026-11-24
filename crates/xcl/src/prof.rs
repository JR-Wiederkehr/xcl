//! `XCL_ACTIVITY=1`: where XCL's own time goes on the title's threads – stopwatches around
//! the pieces of work it does for the title, added up and printed with the activity line.
//! Nested scopes each count their own whole time (a child's time is also in its parent's).
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub enum Cat {
    Execute,
    Refresh,
    SparseBefore,
    UploadBatch,
    WaitValues,
    Present,
    LazyFault,
    DsEnqueue,
    DsSubmit,
    ResolveWait,
    Recheck,
    Watch,
    Pick,
    UploadAll,
    Hash,
    Detile,
}
pub const NAMES: [&str; 16] = ["execute", "tex refresh", "sparse", "tex upload", "wait values", "present", "lazy fault", "ds enqueue", "ds submit", "pso wait", "recheck", "watch", "pick", "upload all", "hash", "detile"];
static NANOS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static CALLS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

pub struct Scope(Cat, std::time::Instant);

#[inline]
pub fn scope(c: Cat) -> Scope {
    Scope(c, std::time::Instant::now())
}

impl Drop for Scope {
    fn drop(&mut self) {
        NANOS[self.0 as usize].fetch_add(self.1.elapsed().as_nanos() as u64, Ordering::Relaxed);
        CALLS[self.0 as usize].fetch_add(1, Ordering::Relaxed);
    }
}

/// (milliseconds, calls) of each category since the process began
pub fn snapshot() -> [(u64, u64); 16] {
    std::array::from_fn(|i| (NANOS[i].load(Ordering::Relaxed) / 1_000_000, CALLS[i].load(Ordering::Relaxed)))
}
