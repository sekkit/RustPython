use crate::atomic::{Ordering, PyAtomic, Radium};

// State layout (usize):
//   [1 bit: destructed] [1 bit: published] [1 bit: leaked] [N bits: weak_count] [M bits: strong_count]
// 64-bit: N=30, M=31.  32-bit: N=14, M=15.
const FLAG_BITS: u32 = 3;
const DESTRUCTED: usize = 1 << (usize::BITS - 1);
/// Object was published to a lock-free cache; memory reclamation is
/// deferred through QSBR so concurrent try-incref readers never touch
/// freed memory. Sticky once set.
const PUBLISHED: usize = 1 << (usize::BITS - 2);
const LEAKED: usize = 1 << (usize::BITS - 3);
const TOTAL_COUNT_WIDTH: u32 = usize::BITS - FLAG_BITS;
const WEAK_WIDTH: u32 = TOTAL_COUNT_WIDTH / 2;
const STRONG_WIDTH: u32 = TOTAL_COUNT_WIDTH - WEAK_WIDTH;
const STRONG: usize = (1 << STRONG_WIDTH) - 1;
const COUNT: usize = 1;
const WEAK_COUNT: usize = 1 << STRONG_WIDTH;

#[inline(never)]
#[cold]
#[allow(
    clippy::disallowed_methods,
    reason = "refcount overflow must preserve upstream abort semantics"
)]
fn refcount_overflow() -> ! {
    cfg_select! {
        feature = "std" => std::process::abort(),
        _ => core::panic!("refcount overflow"),
    }
}

/// State wraps reference count + flags in a single word (platform usize)
#[derive(Clone, Copy)]
struct State {
    inner: usize,
}

impl State {
    #[inline]
    fn from_raw(inner: usize) -> Self {
        Self { inner }
    }

    #[inline]
    fn as_raw(self) -> usize {
        self.inner
    }

    #[inline]
    fn strong(self) -> u32 {
        ((self.inner & STRONG) / COUNT) as u32
    }

    #[inline]
    fn destructed(self) -> bool {
        (self.inner & DESTRUCTED) != 0
    }

    #[inline]
    fn leaked(self) -> bool {
        (self.inner & LEAKED) != 0
    }

    #[inline]
    fn add_strong(self, val: u32) -> Self {
        Self::from_raw(self.inner + (val as usize) * COUNT)
    }

    #[inline]
    fn with_leaked(self, leaked: bool) -> Self {
        Self::from_raw((self.inner & !LEAKED) | if leaked { LEAKED } else { 0 })
    }
}

/// Reference count using state layout with LEAKED support.
///
/// State layout (usize):
/// 64-bit: [1 bit: destructed] [1 bit: published] [1 bit: leaked] [30 bits: weak_count] [31 bits: strong_count]
/// 32-bit: [1 bit: destructed] [1 bit: published] [1 bit: leaked] [14 bits: weak_count] [15 bits: strong_count]
#[repr(transparent)]
pub struct RefCount {
    state: PyAtomic<usize>,
}

impl Default for RefCount {
    fn default() -> Self {
        Self::new()
    }
}

impl RefCount {
    /// Create a new RefCount with strong count = 1
    #[must_use]
    pub fn new() -> Self {
        // Initial state: strong=1, weak=1 (implicit weak for strong refs)
        Self {
            state: Radium::new(COUNT + WEAK_COUNT),
        }
    }

    /// Get current strong count
    #[inline]
    pub fn get(&self) -> usize {
        State::from_raw(self.state.load(Ordering::Relaxed)).strong() as usize
    }

    /// Increment strong count
    #[inline]
    pub fn inc(&self) {
        let val = State::from_raw(self.state.fetch_add(COUNT, Ordering::Relaxed));
        if val.destructed() || (val.strong() as usize) > STRONG - 1 {
            refcount_overflow();
        }
        if val.strong() == 0 {
            // The previous fetch_add created a permission to run decrement again
            self.state.fetch_add(COUNT, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn inc_by(&self, n: usize) {
        debug_assert!(n <= STRONG);
        let val = State::from_raw(self.state.fetch_add(n * COUNT, Ordering::Relaxed));
        if val.destructed() || (val.strong() as usize) > STRONG - n {
            refcount_overflow();
        }
    }

    /// Returns true if successful
    #[inline]
    #[must_use]
    pub fn safe_inc(&self) -> bool {
        let mut old = State::from_raw(self.state.load(Ordering::Relaxed));
        loop {
            if old.destructed() || old.strong() == 0 {
                return false;
            }
            if (old.strong() as usize) >= STRONG {
                refcount_overflow();
            }
            let new_state = old.add_strong(1);
            match self.state.compare_exchange_weak(
                old.as_raw(),
                new_state.as_raw(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(curr) => old = State::from_raw(curr),
            }
        }
    }

    /// Decrement strong count. Returns true when count drops to 0.
    #[inline]
    #[must_use]
    pub fn dec(&self) -> bool {
        let old = State::from_raw(self.state.fetch_sub(COUNT, Ordering::Release));

        // LEAKED objects never reach 0
        if old.leaked() {
            return false;
        }

        if old.strong() == 1 {
            core::sync::atomic::fence(Ordering::Acquire);
            return true;
        }
        false
    }

    /// Mark this object as leaked (interned). It will never be deallocated.
    pub fn leak(&self) {
        debug_assert!(!self.is_leaked());
        let mut old = State::from_raw(self.state.load(Ordering::Relaxed));
        loop {
            let new_state = old.with_leaked(true);
            match self.state.compare_exchange_weak(
                old.as_raw(),
                new_state.as_raw(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(curr) => old = State::from_raw(curr),
            }
        }
    }

    /// Check if this object is leaked (interned).
    pub fn is_leaked(&self) -> bool {
        State::from_raw(self.state.load(Ordering::Acquire)).leaked()
    }

    /// Mark the object as published to a lock-free cache (sticky).
    #[inline]
    pub fn mark_published(&self) {
        self.state.fetch_or(PUBLISHED, Ordering::Release);
    }

    #[inline]
    pub fn is_published(&self) -> bool {
        (self.state.load(Ordering::Acquire) & PUBLISHED) != 0
    }

    /// Raw pointer to the state word for ownership-biased plain updates.
    ///
    /// SAFETY at call sites: single-threaded mode guarantees no concurrent
    /// accessors, so writes through this pointer cannot race.
    #[cfg(feature = "threading")]
    #[inline]
    fn state_raw(&self) -> *mut usize {
        // `PyAtomic<usize>` is either `AtomicUsize` or `Cell<usize>`; both
        // are #[repr(transparent)] wrappers over `UnsafeCell<usize>`, so the
        // cast is layout-correct and writes through the result are
        // legitimate interior mutation.
        unsafe { core::cell::UnsafeCell::raw_get(core::ptr::addr_of!(self.state).cast()) }
    }

    /// Ownership-biased increment ("biased reference counting").
    ///
    /// While the process is single-threaded, every object is effectively
    /// thread-confined and its count word can be updated with plain
    /// loads/stores instead of atomic read-modify-writes. Once a second
    /// interpreter thread exists, unpublished objects may still be reachable
    /// cross-thread through shared registries (the interner, sys.modules),
    /// so all updates take the atomic path. The transition is sticky — see
    /// [`enter_multi_thread_mode`].
    #[inline]
    pub fn inc_biased(&self) {
        if is_multi_thread_mode() || self.is_published() {
            self.inc();
            return;
        }
        #[cfg(feature = "threading")]
        {
            // SAFETY: single-threaded mode — no other thread can observe
            // this non-atomic RMW of the count word.
            let raw = self.state_raw();
            let old = State::from_raw(unsafe { *raw });
            if old.destructed() || (old.strong() as usize) > STRONG - 1 {
                refcount_overflow();
            }
            if old.strong() == 0 {
                // Mirror `inc`: resurrect the decrement permission first.
                unsafe { *raw = old.add_strong(2).as_raw() };
                return;
            }
            unsafe { *raw = old.add_strong(1).as_raw() };
        }
        #[cfg(not(feature = "threading"))]
        {
            // Without the threading feature the state word is a `Cell`,
            // i.e. these operations are already plain stores.
            self.inc();
        }
    }

    /// Ownership-biased decrement; returns true when the count reached zero
    /// and destruction must proceed. Mirrors [`RefCount::dec`] exactly,
    /// minus the acquire fence (which only matters when some other thread
    /// released the last reference — impossible in single-threaded mode).
    #[inline]
    pub fn dec_biased(&self) -> bool {
        if is_multi_thread_mode() || self.is_published() {
            return self.dec();
        }
        #[cfg(feature = "threading")]
        {
            // SAFETY: as in `inc_biased`; single-threaded mode means a
            // non-atomic RMW cannot race anything.
            let raw = self.state_raw();
            let prev = State::from_raw(unsafe { *raw });
            unsafe { *raw = prev.as_raw().wrapping_sub(COUNT) };
            if prev.leaked() {
                return false;
            }
            // Mirror `dec`: it reports destruction based on the *pre*-
            // decrement snapshot (`fetch_sub`'s return value).
            prev.strong() == 1
        }
        #[cfg(not(feature = "threading"))]
        {
            self.dec()
        }
    }
}

/// Whether more than one OS thread may touch Python objects.
///
/// Starts `false`: a freshly started interpreter runs Python code on one
/// thread. Spawning an interpreter thread flips it to `true` (sticky), which
/// permanently routes all biased refcount updates through atomics.
#[cfg(feature = "threading")]
static MULTI_THREAD_MODE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Announce that multiple threads may now touch Python objects. Sticky.
///
/// Called by the VM before spawning an interpreter thread; embedding hosts
/// that run several interpreters concurrently on their own threads must call
/// it too (before starting the second thread).
#[cfg(feature = "threading")]
pub fn enter_multi_thread_mode() {
    use core::sync::atomic::Ordering;
    MULTI_THREAD_MODE.store(true, Ordering::Release);
}

#[cfg(feature = "threading")]
#[inline]
pub fn is_multi_thread_mode() -> bool {
    MULTI_THREAD_MODE.load(core::sync::atomic::Ordering::Acquire)
}

#[cfg(not(feature = "threading"))]
#[inline]
pub fn is_multi_thread_mode() -> bool {
    false
}

// Deferred Drop Infrastructure
//
// This mechanism allows untrack_object() calls to be deferred until after
// the GC collection phase completes, preventing deadlocks that occur when
// clear (pop_edges) triggers object destruction while holding the tracked_objects lock.

#[cfg(feature = "std")]
use core::cell::{Cell, RefCell};

#[cfg(feature = "std")]
thread_local! {
    /// Flag indicating if we're inside a deferred drop context.
    /// When true, drop operations should defer untrack calls.
    static IN_DEFERRED_CONTEXT: Cell<bool> = const { Cell::new(false) };

    /// Queue of deferred untrack operations.
    /// No Send bound needed - this is thread-local and only accessed from the same thread.
    static DEFERRED_QUEUE: RefCell<Vec<Box<dyn FnOnce()>>> = const { RefCell::new(Vec::new()) };
}

#[cfg(feature = "std")]
struct DeferredDropGuard {
    was_in_context: bool,
}

#[cfg(feature = "std")]
impl Drop for DeferredDropGuard {
    fn drop(&mut self) {
        IN_DEFERRED_CONTEXT.with(|in_ctx| {
            in_ctx.set(self.was_in_context);
        });
        // Only flush if we're the outermost context and not already panicking
        // (flushing during unwinding risks double-panic → process abort).
        if !self.was_in_context && !std::thread::panicking() {
            flush_deferred_drops();
        }
    }
}

/// Execute a function within a deferred drop context.
/// Any calls to `try_defer_drop` within this context will be queued
/// and executed when the context exits (even on panic).
#[cfg(feature = "std")]
#[inline]
pub fn with_deferred_drops<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = IN_DEFERRED_CONTEXT.with(|in_ctx| {
        let was_in_context = in_ctx.get();
        in_ctx.set(true);
        DeferredDropGuard { was_in_context }
    });
    f()
}

/// Try to defer a drop-related operation.
/// If inside a deferred context, the operation is queued.
/// Otherwise, it executes immediately.
#[cfg(feature = "std")]
#[inline]
pub fn try_defer_drop<F>(f: F)
where
    F: FnOnce() + 'static,
{
    let should_defer = IN_DEFERRED_CONTEXT.with(|in_ctx| in_ctx.get());

    if should_defer {
        DEFERRED_QUEUE.with(|q| {
            q.borrow_mut().push(Box::new(f));
        });
    } else {
        f();
    }
}

/// Flush all deferred drop operations.
/// This is automatically called when exiting a deferred context.
#[cfg(feature = "std")]
#[inline]
pub fn flush_deferred_drops() {
    DEFERRED_QUEUE.with(|q| {
        // Take all queued operations
        let ops: Vec<_> = q.borrow_mut().drain(..).collect();
        // Execute them outside the borrow
        for op in ops {
            op();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_bit_survives_refcount_traffic() {
        let rc = RefCount::new(); // strong = 1
        assert!(!rc.is_published());
        rc.mark_published();
        assert!(rc.is_published());
        rc.inc(); // strong = 2
        assert!(rc.is_published());
        assert!(!rc.dec()); // strong = 1
        assert!(rc.is_published());
        assert!(rc.safe_inc()); // strong = 2
        assert!(!rc.dec()); // strong = 1
        assert!(rc.dec()); // strong = 0 -> true
        assert!(rc.is_published());
    }

    #[test]
    fn biased_ops_match_plain_ops_unpublished() {
        let rc = RefCount::new();
        assert!(!rc.is_published());
        rc.inc_biased();
        rc.inc_biased();
        assert_eq!(rc.get(), 3);
        assert!(!rc.dec_biased());
        assert_eq!(rc.get(), 2);
        assert!(!rc.dec_biased());
        assert_eq!(rc.get(), 1);
        // Reaching zero through the biased path reports destruction.
        assert!(rc.dec_biased());
    }

    #[test]
    fn biased_ops_route_through_atomics_once_published() {
        let rc = RefCount::new();
        rc.inc_biased(); // strong = 2, still unpublished
        rc.mark_published();
        for _ in 0..3 {
            rc.inc_biased();
        }
        assert_eq!(rc.get(), 5);
        for _ in 0..4 {
            assert!(!rc.dec_biased());
        }
        // The final decrement reports zero even though the sticky flag set.
        assert!(rc.dec_biased());
        assert!(rc.is_published());
    }

    #[test]
    fn biased_dec_on_leaked_never_reports_zero() {
        let rc = RefCount::new();
        rc.leak();
        assert!(!rc.dec_biased());
        assert!(!rc.dec_biased());
        assert!(rc.is_leaked());
    }

    #[cfg(feature = "threading")]
    #[test]
    fn biased_published_counts_survive_thread_storm() {
        use std::sync::{Arc, Barrier};
        // The test spawns raw std threads, so it must announce multi-thread
        // mode exactly like the VM does before spawning an interpreter
        // thread; afterwards all updates take the atomic path.
        enter_multi_thread_mode();
        assert!(is_multi_thread_mode());
        let rc = Arc::new(RefCount::new());
        rc.inc(); // caller's own reference; count = 2
        rc.mark_published();
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let rc = Arc::clone(&rc);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..10_000 {
                    rc.inc_biased();
                    rc.dec_biased();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(rc.get(), 2);
    }
}
