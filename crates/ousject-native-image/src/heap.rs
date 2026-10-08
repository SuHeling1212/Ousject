//! Reclaiming single-core heap over a UEFI-map-reserved physical range.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::{align_of, size_of};
use core::ptr::{null_mut, read_unaligned, write_unaligned};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use ousject_platform::MemoryRange;

/// Initial heap size reserved from a firmware-reported Conventional region.
pub const HEAP_SIZE: u64 = 8 * 1024 * 1024;
const ALLOCATION_MAGIC: usize = 0x4f55_534a_4845_4150;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapInitError {
    EmptyRange,
    AddressOverflow,
    TooSmall,
    AlreadyInitialized,
}

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AllocationHeader {
    magic: usize,
    start: usize,
    size: usize,
}

struct HeapState {
    start: usize,
    end: usize,
    free_head: *mut FreeBlock,
}

// All accesses to HeapState and its intrusive nodes are protected by `lock`.
unsafe impl Sync for HeapState {}

pub struct BootstrapHeap {
    lock: AtomicBool,
    initialized: AtomicU8,
    state: UnsafeCell<HeapState>,
}

// Alloc/dealloc are serialized by a spin lock. Native currently has one
// cooperative CPU and allocation is forbidden in hardware interrupt handlers.
unsafe impl Sync for BootstrapHeap {}

impl BootstrapHeap {
    pub const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            initialized: AtomicU8::new(0),
            state: UnsafeCell::new(HeapState {
                start: 0,
                end: 0,
                free_head: null_mut(),
            }),
        }
    }

    /// Activates the heap over a range already reserved by the frame allocator.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/overflowing/too-small range or repeated setup.
    pub fn initialize(&self, range: MemoryRange) -> Result<(), HeapInitError> {
        if range.length == 0 {
            return Err(HeapInitError::EmptyRange);
        }
        let end = range
            .end()
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(HeapInitError::AddressOverflow)?;
        let start = usize::try_from(range.start).map_err(|_| HeapInitError::AddressOverflow)?;
        let minimum = size_of::<FreeBlock>().max(size_of::<AllocationHeader>() + 1);
        if end.saturating_sub(start) < minimum {
            return Err(HeapInitError::TooSmall);
        }
        self.initialized
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire)
            .map_err(|_| HeapInitError::AlreadyInitialized)?;
        // SAFETY: initialization is exclusive and the range is reserved RAM.
        unsafe {
            let block = start as *mut FreeBlock;
            block.write(FreeBlock {
                size: end - start,
                next: null_mut(),
            });
            self.state.get().write(HeapState {
                start,
                end,
                free_head: block,
            });
        }
        self.initialized.store(2, Ordering::Release);
        Ok(())
    }

    fn allocate(&self, layout: Layout) -> *mut u8 {
        if self.initialized.load(Ordering::Acquire) != 2 {
            return null_mut();
        }
        self.lock();
        // SAFETY: the lock provides exclusive access to the state and free list.
        let result = unsafe { self.allocate_locked(layout) };
        self.unlock();
        result
    }

    /// Caller holds `lock`; all intrusive addresses lie in the reserved heap.
    unsafe fn allocate_locked(&self, layout: Layout) -> *mut u8 {
        // SAFETY: caller holds the allocator lock.
        let state = unsafe { &mut *self.state.get() };
        let mut previous: *mut FreeBlock = null_mut();
        let mut current = state.free_head;
        while !current.is_null() {
            // SAFETY: current is an initialized node from the locked free list.
            let block = unsafe { &mut *current };
            let Some(user) = align_up(
                (current as usize).saturating_add(size_of::<AllocationHeader>()),
                layout.align().max(align_of::<AllocationHeader>()),
            ) else {
                return null_mut();
            };
            let Some(raw_end) = user.checked_add(layout.size().max(1)) else {
                return null_mut();
            };
            let Some(allocated_end) = align_up(raw_end, align_of::<FreeBlock>()) else {
                return null_mut();
            };
            let block_end = (current as usize).saturating_add(block.size);
            if allocated_end <= block_end {
                let remainder = block_end - allocated_end;
                let allocation_size = if remainder >= size_of::<FreeBlock>() {
                    // SAFETY: allocated_end is aligned and the remainder fits a node.
                    let tail = allocated_end as *mut FreeBlock;
                    unsafe {
                        tail.write(FreeBlock {
                            size: remainder,
                            next: block.next,
                        });
                    }
                    if previous.is_null() {
                        state.free_head = tail;
                    } else {
                        // SAFETY: previous is a node in the locked list.
                        unsafe { (*previous).next = tail };
                    }
                    allocated_end - current as usize
                } else {
                    if previous.is_null() {
                        state.free_head = block.next;
                    } else {
                        // SAFETY: previous is a node in the locked list.
                        unsafe { (*previous).next = block.next };
                    }
                    block.size
                };
                let header = (user - size_of::<AllocationHeader>()) as *mut AllocationHeader;
                // SAFETY: header fits in allocated prefix and is suitably aligned.
                unsafe {
                    header.write(AllocationHeader {
                        magic: ALLOCATION_MAGIC,
                        start: current as usize,
                        size: allocation_size,
                    });
                }
                return user as *mut u8;
            }
            previous = current;
            current = block.next;
        }
        null_mut()
    }

    fn deallocate(&self, pointer: *mut u8, _layout: Layout) {
        if pointer.is_null() || self.initialized.load(Ordering::Acquire) != 2 {
            return;
        }
        self.lock();
        // SAFETY: free-list state is exclusively accessed under the allocator lock.
        unsafe { self.deallocate_locked(pointer) };
        self.unlock();
    }

    /// Caller holds `lock`; malformed and duplicate frees are ignored.
    unsafe fn deallocate_locked(&self, pointer: *mut u8) {
        // SAFETY: caller holds the allocator lock.
        let state = unsafe { &mut *self.state.get() };
        let user = pointer as usize;
        if user < state.start.saturating_add(size_of::<AllocationHeader>()) || user >= state.end {
            return;
        }
        let header_pointer = (user - size_of::<AllocationHeader>()) as *mut AllocationHeader;
        // SAFETY: this header resides within the allocator's managed range.
        let mut header = unsafe { read_unaligned(header_pointer) };
        let Some(block_end) = header.start.checked_add(header.size) else {
            return;
        };
        if header.magic != ALLOCATION_MAGIC
            || header.start < state.start
            || block_end > state.end
            || header.size < size_of::<FreeBlock>()
        {
            return;
        }
        header.magic = 0;
        // SAFETY: same in-range header location, accessed under allocator lock.
        unsafe { write_unaligned(header_pointer, header) };

        let mut previous: *mut FreeBlock = null_mut();
        let mut current = state.free_head;
        while !current.is_null() && (current as usize) < header.start {
            previous = current;
            // SAFETY: current is an initialized node from the locked free list.
            current = unsafe { (*current).next };
        }
        let current_start = current as usize;
        if (!current.is_null() && block_end > current_start)
            || (!previous.is_null()
                && (previous as usize).saturating_add(unsafe { (*previous).size }) > header.start)
        {
            return;
        }

        let node = header.start as *mut FreeBlock;
        // SAFETY: block start/size came from a prior allocation from this heap.
        unsafe {
            node.write(FreeBlock {
                size: header.size,
                next: current,
            });
            if previous.is_null() {
                state.free_head = node;
            } else {
                (*previous).next = node;
            }
            if !current.is_null() && block_end == current as usize {
                (*node).size = (*node).size.saturating_add((*current).size);
                (*node).next = (*current).next;
            }
            if !previous.is_null() {
                let previous_end = (previous as usize).saturating_add((*previous).size);
                if previous_end == node as usize {
                    (*previous).size = (*previous).size.saturating_add((*node).size);
                    (*previous).next = (*node).next;
                }
            }
        }
    }

    /// Returns currently free bytes, primarily for the Native allocator smoke.
    pub fn free_bytes(&self) -> usize {
        if self.initialized.load(Ordering::Acquire) != 2 {
            return 0;
        }
        self.lock();
        // SAFETY: allocator state is exclusively read while the lock is held.
        let mut total = 0_usize;
        let mut current = unsafe { (*self.state.get()).free_head };
        while !current.is_null() {
            // SAFETY: current is an initialized node in the protected list.
            let block = unsafe { &*current };
            total = total.saturating_add(block.size);
            current = block.next;
        }
        self.unlock();
        total
    }

    fn lock(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

// SAFETY: allocation is serialized; range ownership and raw-pointer bounds are
// established by initialize and maintained by allocate_locked/deallocate_locked.
unsafe impl GlobalAlloc for BootstrapHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocate(layout)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.deallocate(pointer, layout);
    }
}
