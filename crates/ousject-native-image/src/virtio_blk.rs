//! Polled transitional `VirtIO` PCI block transport for the QEMU first boot.
//!
//! This intentionally implements only one legacy virtqueue and synchronous
//! sector reads/writes/flushes. The OMS layer above it owns snapshot semantics.

// Queue allocation and every shared-ring field offset are explicitly checked
// for alignment before this module casts byte pointers to descriptor types.
#![allow(clippy::cast_ptr_alignment)]

use alloc::boxed::Box;
use core::cell::RefCell;
use core::sync::atomic::{Ordering, fence};
use core::{hint::spin_loop, ptr};
use oms_runtime::BlockDevice;
use oms_types::OmsError;

const PCI_CONFIG_ADDRESS: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;
const PCI_VENDOR_VIRTIO: u16 = 0x1af4;
const PCI_DEVICE_BLOCK_LEGACY: u16 = 0x1001;
const PCI_COMMAND: u8 = 0x04;
const PCI_HEADER_TYPE: u8 = 0x0e;
const PCI_BAR0: u8 = 0x10;
const PCI_COMMAND_IO: u16 = 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

const VIRTIO_DEVICE_FEATURES: u16 = 0;
const VIRTIO_DRIVER_FEATURES: u16 = 4;
const VIRTIO_QUEUE_PFN: u16 = 8;
const VIRTIO_QUEUE_SIZE: u16 = 12;
const VIRTIO_QUEUE_SELECT: u16 = 14;
const VIRTIO_QUEUE_NOTIFY: u16 = 16;
const VIRTIO_DEVICE_STATUS: u16 = 18;
const VIRTIO_DEVICE_CONFIG: u16 = 20;
const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FAILED: u8 = 128;
const VIRTIO_F_ANY_LAYOUT: u32 = 1 << 27;
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const REQUEST_IN: u32 = 0;
const REQUEST_OUT: u32 = 1;
const REQUEST_FLUSH: u32 = 4;
const STATUS_OK: u8 = 0;
const STATUS_IOERR: u8 = 1;
const STATUS_UNSUPP: u8 = 2;
const QUEUE_MEMORY_BYTES: usize = 16 * 1024;
const MAX_QUEUE_ENTRIES: u16 = 256;
const DATA_OFFSET: usize = 12 * 1024;
const SECTOR_OFFSET: usize = DATA_OFFSET + 16;
const STATUS_OFFSET: usize = SECTOR_OFFSET + 512;
const REQUEST_POLL_LIMIT: usize = 20_000_000;
const DEVICE_RESET_POLL_LIMIT: usize = 1_000_000;

#[repr(C, align(4096))]
#[derive(Debug)]
struct QueueMemory([u8; QUEUE_MEMORY_BYTES]);

#[derive(Debug)]
struct QueueState {
    memory: Box<QueueMemory>,
    entries: u16,
    available_index: u16,
    used_index: u16,
    disabled: bool,
}

#[derive(Debug)]
pub struct VirtioBlock {
    io_base: u16,
    sectors: u64,
    queue: RefCell<QueueState>,
}

impl VirtioBlock {
    /// Finds and initializes a QEMU transitional `virtio-blk-pci` device.
    ///
    /// # Errors
    ///
    /// Returns an error if PCI discovery, legacy feature negotiation, queue
    /// setup, or device capacity validation fails.
    #[allow(
        clippy::manual_is_multiple_of,
        clippy::too_many_lines // Keep the legacy device's ordered handshake together.
    )] // Preserve Rust 1.85 compatibility and make setup order auditable.
    pub fn initialize() -> Result<Self, OmsError> {
        let (bus, device, function) = find_legacy_block_device()
            .ok_or_else(|| storage_error("QEMU transitional VirtIO block device not found"))?;
        let vendor_device = pci_read32(bus, device, function, 0);
        let vendor = u16::try_from(vendor_device & u32::from(u16::MAX))
            .expect("masked PCI vendor fits in u16");
        let device_id = u16::try_from(vendor_device >> 16).expect("PCI device id fits in u16");
        if vendor != PCI_VENDOR_VIRTIO || device_id != PCI_DEVICE_BLOCK_LEGACY {
            return Err(storage_error("unsupported VirtIO PCI device identity"));
        }

        let bar0 = pci_read32(bus, device, function, PCI_BAR0);
        if bar0 & 1 == 0 {
            return Err(storage_error("VirtIO legacy BAR0 is not I/O space"));
        }
        let io_base = u16::try_from(bar0 & 0xfffc)
            .map_err(|_| storage_error("VirtIO I/O BAR is out of range"))?;
        if io_base == 0 {
            return Err(storage_error("VirtIO I/O BAR is not assigned"));
        }

        let command = pci_read16(bus, device, function, PCI_COMMAND);
        pci_write16(
            bus,
            device,
            function,
            PCI_COMMAND,
            command | PCI_COMMAND_IO | PCI_COMMAND_BUS_MASTER,
        );

        write_u8(io_base + VIRTIO_DEVICE_STATUS, 0);
        if !wait_for_status(io_base, 0) {
            return Err(storage_error("VirtIO block reset timed out"));
        }
        write_u8(io_base + VIRTIO_DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
        write_u8(
            io_base + VIRTIO_DEVICE_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
        );

        let host_features = read_u32(io_base + VIRTIO_DEVICE_FEATURES);
        if host_features & VIRTIO_BLK_F_FLUSH == 0 {
            fail_device(io_base);
            return Err(storage_error("VirtIO block device has no durable FLUSH feature"));
        }
        // Request only FLUSH. The block size is the specified legacy 512-byte
        // sector and no optional discard, write-zeroes, or topology is used.
        write_u32(
            io_base + VIRTIO_DRIVER_FEATURES,
            VIRTIO_BLK_F_FLUSH | (host_features & VIRTIO_F_ANY_LAYOUT),
        );

        write_u16(io_base + VIRTIO_QUEUE_SELECT, 0);
        let entries = read_u16(io_base + VIRTIO_QUEUE_SIZE);
        if !(3..=MAX_QUEUE_ENTRIES).contains(&entries) || !entries.is_power_of_two() {
            fail_device(io_base);
            return Err(storage_error("VirtIO block queue size is unsupported"));
        }
        let memory = Box::new(QueueMemory([0; QUEUE_MEMORY_BYTES]));
        let queue_address = memory.0.as_ptr() as u64;
        if queue_address % 4096 != 0 || queue_address >= u64::from(u32::MAX) {
            fail_device(io_base);
            return Err(storage_error("VirtIO queue is not legacy DMA-addressable"));
        }
        let used_offset = queue_used_offset(entries);
        let required_bytes = used_offset + 4 + usize::from(entries) * 8;
        if required_bytes > QUEUE_MEMORY_BYTES {
            fail_device(io_base);
            return Err(storage_error("VirtIO queue exceeds its DMA allocation"));
        }
        // Do not enable legacy PCI interrupts: the first driver polls the used
        // ring and intentionally has no IRQ delivery path yet.
        let available_offset = queue_available_offset(entries);
        // SAFETY: The queue allocation remains live, and its available flags
        // field is naturally aligned inside the initialized DMA region.
        unsafe {
            ptr::write_volatile(
                memory
                    .0
                    .as_ptr()
                    .add(available_offset)
                    .cast_mut()
                    .cast::<u16>(),
                1_u16,
            );
        }
        fence(Ordering::Release);
        let queue_pfn = u32::try_from(queue_address >> 12)
            .map_err(|_| storage_error("VirtIO queue PFN exceeds the legacy register"))?;
        write_u32(io_base + VIRTIO_QUEUE_PFN, queue_pfn);

        // Legacy block capacity is expressed in 512-byte sectors and may
        // change while the device initializes. Read it until two samples agree.
        let mut previous = read_capacity(io_base);
        let mut sectors = None;
        for _ in 0..DEVICE_RESET_POLL_LIMIT {
            let current = read_capacity(io_base);
            if current == previous {
                sectors = Some(current);
                break;
            }
            previous = current;
            spin_loop();
        }
        let Some(sectors) = sectors.filter(|capacity| *capacity > 0) else {
            fail_device(io_base);
            return Err(storage_error("VirtIO block capacity is unavailable"));
        };
        write_u8(
            io_base + VIRTIO_DEVICE_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
        );

        Ok(Self {
            io_base,
            sectors,
            queue: RefCell::new(QueueState {
                memory,
                entries,
                available_index: 0,
                used_index: 0,
                disabled: false,
            }),
        })
    }

    #[allow(clippy::too_many_lines)] // Request layout, publication, polling, and status are one protocol operation.
    fn request(
        &self,
        request_type: u32,
        sector: u64,
        mut data: Option<&mut [u8]>,
    ) -> Result<(), OmsError> {
        let mut queue = self.queue.borrow_mut();
        if queue.disabled {
            return Err(storage_error("VirtIO block queue is disabled after an error"));
        }
        if matches!(data.as_ref(), Some(bytes) if bytes.len() != 512) {
            return Err(storage_error("VirtIO block requests require one sector"));
        }
        let base = queue.memory.0.as_mut_ptr();
        let base_address = base as u64;
        // SAFETY: The queue's DMA allocation is page-aligned, retained for the
        // device lifetime, and contained in the mapped Native heap.
        unsafe {
            ptr::write_volatile(base.add(DATA_OFFSET).cast::<u32>(), request_type.to_le());
            ptr::write_volatile(base.add(DATA_OFFSET + 4).cast::<u32>(), 0);
            ptr::write_volatile(base.add(DATA_OFFSET + 8).cast::<u64>(), sector.to_le());
            ptr::write_volatile(base.add(STATUS_OFFSET), 0xff);
        }
        if let (REQUEST_OUT, Some(bytes)) = (request_type, data.as_deref_mut()) {
            // SAFETY: Sector buffer is inside the retained DMA allocation.
            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(SECTOR_OFFSET), 512) };
        }

        set_descriptor(
            base,
            0,
            base_address + DATA_OFFSET as u64,
            16,
            DESC_NEXT,
            1,
        );
        if request_type == REQUEST_FLUSH {
            set_descriptor(
                base,
                1,
                base_address + STATUS_OFFSET as u64,
                1,
                DESC_WRITE,
                0,
            );
        } else {
            let data_flags = DESC_NEXT | if request_type == REQUEST_IN { DESC_WRITE } else { 0 };
            set_descriptor(
                base,
                1,
                base_address + SECTOR_OFFSET as u64,
                512,
                data_flags,
                2,
            );
            set_descriptor(
                base,
                2,
                base_address + STATUS_OFFSET as u64,
                1,
                DESC_WRITE,
                0,
            );
        }

        let available_offset = queue_available_offset(queue.entries);
        let ring_slot = queue.available_index % queue.entries;
        // SAFETY: `available_offset` and `ring_slot` are within the validated
        // queue allocation; volatile writes publish descriptor ownership.
        unsafe {
            ptr::write_volatile(
                base.add(available_offset + 4 + usize::from(ring_slot) * 2)
                    .cast::<u16>(),
                0_u16,
            );
        }
        queue.available_index = queue.available_index.wrapping_add(1);
        fence(Ordering::Release);
        // SAFETY: The avail index is at a naturally aligned location in the
        // shared ring and the DMA allocation remains live.
        unsafe {
            ptr::write_volatile(
                base.add(available_offset + 2).cast::<u16>(),
                queue.available_index.to_le(),
            );
        }
        fence(Ordering::SeqCst);
        write_u16(self.io_base + VIRTIO_QUEUE_NOTIFY, 0);

        let used_offset = queue_used_offset(queue.entries);
        let expected_used = queue.used_index.wrapping_add(1);
        let mut completed = false;
        for _ in 0..REQUEST_POLL_LIMIT {
            fence(Ordering::Acquire);
            // SAFETY: Device updates this aligned used index in the queue.
            let used = unsafe { ptr::read_volatile(base.add(used_offset + 2).cast::<u16>()) };
            if u16::from_le(used) == expected_used {
                completed = true;
                break;
            }
            spin_loop();
        }
        if !completed {
            queue.disabled = true;
            fail_device(self.io_base);
            return Err(storage_error("VirtIO block request timed out"));
        }
        // SAFETY: A completed request has published one used-ring element.
        let used_head = unsafe {
            ptr::read_volatile(
                base.add(used_offset + 4 + usize::from(queue.used_index % queue.entries) * 8)
                    .cast::<u32>(),
            )
        };
        if u32::from_le(used_head) != 0 {
            queue.disabled = true;
            fail_device(self.io_base);
            return Err(storage_error("VirtIO returned an unexpected queue head"));
        }
        queue.used_index = expected_used;
        // SAFETY: Device completed writing the request status byte.
        let status = unsafe { ptr::read_volatile(base.add(STATUS_OFFSET)) };
        match status {
            STATUS_OK => {}
            STATUS_IOERR => return Err(storage_error("VirtIO block I/O failed")),
            STATUS_UNSUPP => return Err(storage_error("VirtIO block request is unsupported")),
            _ => return Err(storage_error("VirtIO block returned an invalid status")),
        }
        if let (REQUEST_IN, Some(bytes)) = (request_type, data) {
            // SAFETY: The device completed the read into the sector buffer.
            unsafe { ptr::copy_nonoverlapping(base.add(SECTOR_OFFSET), bytes.as_mut_ptr(), 512) };
        }
        Ok(())
    }
}

impl BlockDevice for VirtioBlock {
    fn block_size(&self) -> u32 {
        512
    }

    fn block_count(&self) -> u64 {
        self.sectors
    }

    #[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks_mut` is newer than the Rust 1.85 MSRV.
    fn read_blocks(&self, first_block: u64, output: &mut [u8]) -> Result<(), OmsError> {
        self.validate_range(first_block, output.len())?;
        for (offset, block) in output.chunks_exact_mut(512).enumerate() {
            self.request(REQUEST_IN, first_block + offset as u64, Some(block))?;
        }
        Ok(())
    }

    #[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks` is newer than the Rust 1.85 MSRV.
    fn write_blocks(&self, first_block: u64, data: &[u8]) -> Result<(), OmsError> {
        self.validate_range(first_block, data.len())?;
        for (offset, block) in data.chunks_exact(512).enumerate() {
            let mut sector = [0; 512];
            sector.copy_from_slice(block);
            self.request(REQUEST_OUT, first_block + offset as u64, Some(&mut sector))?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), OmsError> {
        self.request(REQUEST_FLUSH, 0, None)
    }
}

impl VirtioBlock {
    #[allow(clippy::manual_is_multiple_of)] // Keep compatibility with the crate's Rust 1.85 MSRV.
    fn validate_range(&self, first_block: u64, byte_length: usize) -> Result<(), OmsError> {
        if byte_length == 0 || byte_length % 512 != 0 {
            return Err(storage_error("VirtIO block I/O must use whole 512-byte sectors"));
        }
        let blocks = u64::try_from(byte_length / 512)
            .map_err(|_| storage_error("VirtIO block request length overflow"))?;
        let end = first_block
            .checked_add(blocks)
            .ok_or_else(|| storage_error("VirtIO block request range overflow"))?;
        if end > self.sectors {
            return Err(storage_error("VirtIO block request exceeds device capacity"));
        }
        Ok(())
    }
}

fn find_legacy_block_device() -> Option<(u8, u8, u8)> {
    for bus in 0..=u8::MAX {
        for device in 0..32_u8 {
            let id = pci_read32(bus, device, 0, 0);
            if u16::try_from(id & u32::from(u16::MAX)).expect("masked PCI vendor fits in u16")
                == u16::MAX
            {
                continue;
            }
            if u16::try_from(id & u32::from(u16::MAX)).expect("masked PCI vendor fits in u16")
                == PCI_VENDOR_VIRTIO
                && u16::try_from(id >> 16).expect("PCI device id fits in u16")
                    == PCI_DEVICE_BLOCK_LEGACY
            {
                return Some((bus, device, 0));
            }
            let header = pci_read32(bus, device, 0, PCI_HEADER_TYPE);
            let functions = if (u8::try_from((header >> 16) & u32::from(u8::MAX))
                .expect("masked PCI header type fits in u8"))
                & 0x80
                != 0
            {
                8_u8
            } else {
                1_u8
            };
            for function in 1..functions {
                let id = pci_read32(bus, device, function, 0);
                if u16::try_from(id & u32::from(u16::MAX))
                    .expect("masked PCI vendor fits in u16")
                    == PCI_VENDOR_VIRTIO
                    && u16::try_from(id >> 16).expect("PCI device id fits in u16")
                        == PCI_DEVICE_BLOCK_LEGACY
                {
                    return Some((bus, device, function));
                }
            }
        }
    }
    None
}

fn queue_available_offset(entries: u16) -> usize {
    usize::from(entries) * 16
}

fn queue_used_offset(entries: u16) -> usize {
    let available_end = queue_available_offset(entries) + 4 + usize::from(entries) * 2;
    (available_end + 4095) & !4095
}

fn set_descriptor(
    queue: *mut u8,
    index: u16,
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
) {
    let offset = usize::from(index) * 16;
    // SAFETY: Queue setup validated the selected size and backing allocation.
    unsafe {
        ptr::write_volatile(queue.add(offset).cast::<u64>(), address.to_le());
        ptr::write_volatile(queue.add(offset + 8).cast::<u32>(), length.to_le());
        ptr::write_volatile(queue.add(offset + 12).cast::<u16>(), flags.to_le());
        ptr::write_volatile(queue.add(offset + 14).cast::<u16>(), next.to_le());
    }
}

fn read_capacity(base: u16) -> u64 {
    let low = read_u32(base + VIRTIO_DEVICE_CONFIG);
    let high = read_u32(base + VIRTIO_DEVICE_CONFIG + 4);
    u64::from(low) | (u64::from(high) << 32)
}

fn wait_for_status(base: u16, expected: u8) -> bool {
    for _ in 0..DEVICE_RESET_POLL_LIMIT {
        if read_u8(base + VIRTIO_DEVICE_STATUS) == expected {
            return true;
        }
        spin_loop();
    }
    false
}

fn fail_device(base: u16) {
    let status = read_u8(base + VIRTIO_DEVICE_STATUS);
    write_u8(base + VIRTIO_DEVICE_STATUS, status | VIRTIO_STATUS_FAILED);
}

fn pci_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset & 0xfc)
}

fn pci_read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    unsafe {
        write_port32(PCI_CONFIG_ADDRESS, pci_config_address(bus, device, function, offset));
        read_port32(PCI_CONFIG_DATA)
    }
}

fn pci_read16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read32(bus, device, function, offset);
    let shifted = value >> (u32::from(offset & 2) * 8);
    u16::try_from(shifted & u32::from(u16::MAX)).expect("masked PCI halfword fits in u16")
}

fn pci_write16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    unsafe {
        write_port32(PCI_CONFIG_ADDRESS, pci_config_address(bus, device, function, offset));
        write_port16(PCI_CONFIG_DATA + u16::from(offset & 2), value);
    }
}

fn read_u8(port: u16) -> u8 {
    unsafe { read_port8(port) }
}

fn write_u8(port: u16, value: u8) {
    unsafe { write_port8(port, value) }
}

fn read_u16(port: u16) -> u16 {
    unsafe { read_port16(port) }
}

fn write_u16(port: u16, value: u16) {
    unsafe { write_port16(port, value) }
}

fn read_u32(port: u16) -> u32 {
    unsafe { read_port32(port) }
}

fn write_u32(port: u16, value: u32) {
    unsafe { write_port32(port, value) }
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_port8(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_port16(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn read_port32(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn write_port8(port: u16, value: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn write_port16(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn write_port32(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn read_port8(_port: u16) -> u8 {
    0
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn read_port16(_port: u16) -> u16 {
    0
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn read_port32(_port: u16) -> u32 {
    0
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn write_port8(_port: u16, _value: u8) {}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn write_port16(_port: u16, _value: u16) {}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn write_port32(_port: u16, _value: u32) {}

fn storage_error(message: &'static str) -> OmsError {
    OmsError::Storage(message.into())
}
