//! Monotonic bootstrap heap backed by a UEFI-map-reserved physical range.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use ousject_platform::MemoryRange;

/// Initial heap size reserved from a firmware-reported Conventional region.
pub const HEAP_SIZE: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapInitError {
    EmptyRange,
    AddressOverflow,
    AlreadyInitialized,
}

pub struct BootstrapHeap {
    next: AtomicUsize,
    end: AtomicUsize,
    state: AtomicU8,
}

impl BootstrapHeap {
    pub const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            state: AtomicU8::new(0),
        }
    }

    /// Activates the heap over a range already reserved by the frame allocator.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/overflowing range or repeated setup.
    pub fn initialize(&self, range: MemoryRange) -> Result<(), HeapInitError> {
        if range.length == 0 {
            return Err(HeapInitError::EmptyRange);
        }
        let end = range
            .end()
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(HeapInitError::AddressOverflow)?;
        let start = usize::try_from(range.start).map_err(|_| HeapInitError::AddressOverflow)?;
        self.state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire)
            .map_err(|_| HeapInitError::AlreadyInitialized)?;
        self.next.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Relaxed);
        self.state.store(2, Ordering::Release);
        Ok(())
    }

    fn allocate(&self, layout: Layout) -> *mut u8 {
        if self.state.load(Ordering::Acquire) != 2 {
            return null_mut();
        }
        let mut current = self.next.load(Ordering::Relaxed);
        loop {
            let Some(aligned) = current.checked_add(layout.align() - 1) else {
                return null_mut();
            };
            let aligned = aligned & !(layout.align() - 1);
            let Some(next) = aligned.checked_add(layout.size().max(1)) else {
                return null_mut();
            };
            if next > self.end.load(Ordering::Acquire) {
                return null_mut();
            }
            match self.next.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return aligned as *mut u8,
                Err(observed) => current = observed,
            }
        }
    }
}

// The allocator only hands out disjoint ranges using an atomic bump pointer.
// Memory is a valid identity-mapped Conventional range reserved before use.
unsafe impl GlobalAlloc for BootstrapHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocate(layout)
    }

    unsafe fn dealloc(&self, _pointer: *mut u8, _layout: Layout) {
        // This bootstrap allocator intentionally does not reclaim memory.
    }
}
