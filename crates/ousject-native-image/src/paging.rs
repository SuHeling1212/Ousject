//! Bootstrap `x86_64` identity page tables for the first 4 GiB.

use core::arch::asm;
use core::ptr::write_volatile;
use ousject_platform::{MemoryRegion, MemoryRegionKind};

pub const MAX_MAPPED_ADDRESS: u64 = 1 << 32;
const PAGE_DIRECTORY_SPAN: u64 = 1 << 30;
const LARGE_PAGE_SIZE: u64 = 1 << 21;
const PRESENT_WRITABLE: u64 = 0x003;
const PAGE_SIZE_FLAG: u64 = 0x080;
const CACHE_DISABLE_FLAGS: u64 = 0x018;
const ENTRY_COUNT: usize = 512;
const DIRECTORY_COUNT: usize = 4;

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct PageTable([u64; ENTRY_COUNT]);

static mut ROOT: PageTable = PageTable([0; ENTRY_COUNT]);
static mut DIRECTORY_POINTERS: PageTable = PageTable([0; ENTRY_COUNT]);
static mut DIRECTORIES: [PageTable; DIRECTORY_COUNT] =
    [PageTable([0; ENTRY_COUNT]); DIRECTORY_COUNT];

/// Installs Ousject-owned identity mappings for physical addresses below 4 GiB.
///
/// The early Native image is intentionally limited to a QEMU-sized address
/// space while memory ownership is being brought up. All memory is mapped with
/// 2 MiB pages; any page intersecting a firmware MMIO descriptor is marked
/// uncached. The current instruction pointer and stack must already reside in
/// the identity-mapped range.
pub fn initialize(regions: &[MemoryRegion]) -> bool {
    let current_stack: u64;
    // SAFETY: Reading RSP is a non-mutating ring-0 operation at the EFI entry.
    unsafe {
        asm!(
            "mov {}, rsp",
            out(reg) current_stack,
            options(nomem, nostack, preserves_flags)
        );
    }

    let root = core::ptr::addr_of_mut!(ROOT).cast::<u64>();
    let directory_pointers = core::ptr::addr_of_mut!(DIRECTORY_POINTERS).cast::<u64>();
    let directories = core::ptr::addr_of_mut!(DIRECTORIES).cast::<PageTable>();
    let code_address = initialize as *const () as u64;
    let page_table_range_is_mapped = |start: u64, length: usize| {
        let Ok(length) = u64::try_from(length) else {
            return false;
        };
        start
            .checked_add(length)
            .is_some_and(|end| end <= MAX_MAPPED_ADDRESS)
    };
    if current_stack >= MAX_MAPPED_ADDRESS
        || code_address >= MAX_MAPPED_ADDRESS
        || !page_table_range_is_mapped(root as u64, core::mem::size_of::<PageTable>())
        || !page_table_range_is_mapped(directory_pointers as u64, core::mem::size_of::<PageTable>())
        || !page_table_range_is_mapped(
            directories as u64,
            core::mem::size_of::<PageTable>() * DIRECTORY_COUNT,
        )
    {
        return false;
    }

    // SAFETY: The page tables are static, 4 KiB-aligned EFI image data. Every
    // physical frame used here is the same address as its current identity
    // mapping. Interrupts remain disabled across the CR3 switch.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
        for index in 0..ENTRY_COUNT {
            write_volatile(root.add(index), 0);
            write_volatile(directory_pointers.add(index), 0);
        }
        write_volatile(root, directory_pointers as u64 | PRESENT_WRITABLE);

        for directory_index in 0..DIRECTORY_COUNT {
            let directory = directories.add(directory_index).cast::<u64>();
            write_volatile(
                directory_pointers.add(directory_index),
                directory as u64 | PRESENT_WRITABLE,
            );
            for entry_index in 0..ENTRY_COUNT {
                let physical_start = directory_index as u64 * PAGE_DIRECTORY_SPAN
                    + entry_index as u64 * LARGE_PAGE_SIZE;
                let physical_end = physical_start + LARGE_PAGE_SIZE;
                let contains_mmio = regions.iter().any(|region| {
                    region.kind == MemoryRegionKind::Mmio
                        && region
                            .end()
                            .is_some_and(|end| region.start < physical_end && physical_start < end)
                });
                let cache_flags = if contains_mmio || physical_start == 0 {
                    CACHE_DISABLE_FLAGS
                } else {
                    0
                };
                write_volatile(
                    directory.add(entry_index),
                    physical_start | PRESENT_WRITABLE | PAGE_SIZE_FLAG | cache_flags,
                );
            }
        }

        let new_root = root as u64;
        asm!(
            "mov rax, cr4",
            "or rax, 0x30",
            "mov cr4, rax",
            "mov rax, cr0",
            "or rax, 0x10000",
            "mov cr0, rax",
            "mov cr3, {root}",
            root = in(reg) new_root,
            out("rax") _,
            options(nostack)
        );
        let loaded_root: u64;
        asm!("mov {}, cr3", out(reg) loaded_root, options(nomem, nostack, preserves_flags));
        loaded_root == new_root
    }
}
