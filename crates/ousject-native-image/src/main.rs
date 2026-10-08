#![no_main]
#![no_std]

#[cfg(not(target_os = "uefi"))]
compile_error!("ousject-native-image must be built for x86_64-unknown-uefi");
#[cfg(any(
    all(feature = "fault-smoke", feature = "panic-smoke"),
    all(feature = "fault-smoke", feature = "double-fault-smoke"),
    all(feature = "panic-smoke", feature = "double-fault-smoke")
))]
compile_error!("Native smoke modes are mutually exclusive");

use core::fmt::{self, Write};
use core::hint::spin_loop;
use core::ptr::read_volatile;
use ousject_platform::{
    BootInfo, MemoryRegion, MemoryRegionKind, MonotonicClock, PhysicalFrameAllocator,
    TerminalTransport,
};
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::proto::console::serial::Serial;

mod clock;
mod com1;
mod cpu;
mod paging;
mod timer;

const MAX_MEMORY_REGIONS: usize = 512;
const MEMORY_REGION_PLACEHOLDER: MemoryRegion = MemoryRegion {
    start: 0,
    length: 0,
    kind: MemoryRegionKind::Reserved,
};

#[entry]
fn main() -> Status {
    if uefi::helpers::init().is_err() {
        return Status::DEVICE_ERROR;
    }

    write_serial(b"Ousject native: UEFI entry\r\n");
    write_serial(b"Ousject native: exiting boot services\r\n");

    // SAFETY: No boot-services resources are retained across this call. The
    // serial protocol handle has been dropped, and after the handoff this
    // image only reads the returned memory map and uses core-only code.
    let memory_map = unsafe { uefi::boot::exit_boot_services(None) };

    let mut regions = [MEMORY_REGION_PLACEHOLDER; MAX_MEMORY_REGIONS];
    let mut region_count = 0;
    for descriptor in memory_map.entries() {
        if region_count == regions.len() {
            enter_halt_loop();
        }

        let length = descriptor.page_count.saturating_mul(4096);
        let kind = if descriptor.ty == uefi::boot::MemoryType::CONVENTIONAL {
            MemoryRegionKind::Usable
        } else if descriptor.ty == uefi::boot::MemoryType::MMIO
            || descriptor.ty == uefi::boot::MemoryType::MMIO_PORT_SPACE
        {
            MemoryRegionKind::Mmio
        } else if descriptor.ty == uefi::boot::MemoryType::LOADER_CODE
            || descriptor.ty == uefi::boot::MemoryType::LOADER_DATA
            || descriptor.ty == uefi::boot::MemoryType::BOOT_SERVICES_CODE
            || descriptor.ty == uefi::boot::MemoryType::BOOT_SERVICES_DATA
            || descriptor.ty == uefi::boot::MemoryType::RUNTIME_SERVICES_CODE
            || descriptor.ty == uefi::boot::MemoryType::RUNTIME_SERVICES_DATA
            || descriptor.ty == uefi::boot::MemoryType::ACPI_RECLAIM
            || descriptor.ty == uefi::boot::MemoryType::ACPI_NON_VOLATILE
        {
            MemoryRegionKind::Firmware
        } else {
            MemoryRegionKind::Reserved
        };
        regions[region_count] = MemoryRegion {
            start: descriptor.phys_start,
            length,
            kind,
        };
        region_count += 1;
    }

    let physical_address_bits = cpu::physical_address_bits();
    let boot_info = BootInfo {
        memory_map: &regions[..region_count],
        physical_address_bits,
    };
    native_kernel_entry(boot_info)
}

fn write_serial(bytes: &[u8]) {
    let Ok(handle) = uefi::boot::get_handle_for_protocol::<Serial>() else {
        return;
    };
    let Ok(mut serial) = uefi::boot::open_protocol_exclusive::<Serial>(handle) else {
        return;
    };
    let _ = serial.write(bytes);
}

fn native_kernel_entry(boot_info: BootInfo<'_>) -> ! {
    // This first image proves firmware handoff, the bootstrap frame selector,
    // a native serial transport, and fatal CPU exception reporting.
    let _usable_regions = boot_info
        .memory_map
        .iter()
        .filter(|region| region.kind == MemoryRegionKind::Usable)
        .count();

    let mut frame_allocator = PhysicalFrameAllocator::new();
    let Some(first_frame) = frame_allocator.allocate_frame(boot_info.memory_map) else {
        enter_halt_loop();
    };
    if first_frame >= paging::MAX_MAPPED_ADDRESS {
        enter_halt_loop();
    }

    let mut serial = com1::Com1::initialize();
    if serial
        .output(b"Ousject native: Boot Services exited; COM1 and frame allocator online\r\n")
        .is_err()
    {
        enter_halt_loop();
    }

    cpu::initialize();
    assert!(
        cpu::verify_task_register(),
        "Native TSS task register or stack pointers did not load"
    );
    if serial
        .output(b"Ousject native: GDT, TSS, and IDT installed\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    assert!(
        paging::initialize(boot_info.memory_map),
        "Native identity page table installation failed"
    );
    if serial
        .output(b"Ousject native: owned identity page tables active\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    cpu::enter_kernel_stack(native_execution_entry)
}

extern "efiapi" fn native_execution_entry() -> ! {
    let mut serial = com1::Com1::initialize();
    if serial
        .output(b"Ousject native: owned kernel stack active\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    timer::initialize();
    let pit_clock = timer::PitClock::new();
    assert!(
        timer::wait_for_ticks(2),
        "Native PIT did not deliver timer interrupts"
    );
    assert!(
        pit_clock.now_nanos() >= 20_000_000,
        "Native PIT clock did not advance monotonically"
    );
    if serial
        .output(b"Ousject native: PIT timer interrupt online\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    if let Some(clock) = clock::TscClock::initialize() {
        let first = clock.now_nanos();
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
        let second = clock.now_nanos();
        assert!(second >= first, "Native monotonic clock moved backwards");
        if serial
            .output(b"Ousject native: invariant TSC clock online\r\n")
            .is_err()
        {
            enter_halt_loop();
        }
    } else if serial
        .output(b"Ousject native: invariant TSC clock unavailable\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    #[cfg(feature = "panic-smoke")]
    panic!("intentional native panic smoke");
    #[cfg(feature = "double-fault-smoke")]
    cpu::trigger_double_fault();
    #[cfg(feature = "fault-smoke")]
    cpu::trigger_invalid_opcode();

    #[cfg(not(any(
        feature = "fault-smoke",
        feature = "panic-smoke",
        feature = "double-fault-smoke"
    )))]
    enter_halt_loop()
}

fn enter_halt_loop() -> ! {
    loop {
        // SAFETY: The UEFI application executes at ring 0. Halt avoids burning
        // a core after the current fatal or smoke-test path is complete.
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
        spin_loop();
    }
}

struct PanicSerial<'a>(&'a mut com1::Com1);

impl Write for PanicSerial<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.output(text.as_bytes()).map_err(|_| fmt::Error)
    }
}

#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo<'_>) -> ! {
    let mut serial = com1::Com1::initialize();
    let _ = serial.output(b"Ousject native: panic diagnostic begin\r\n");
    {
        let mut output = PanicSerial(&mut serial);
        let _ = write!(output, "Ousject native panic: {info}\r\n");
    }
    let _ = serial.output(b"Ousject native: panic handler halt\r\n");
    enter_halt_loop()
}

/// Supplies the freestanding wide-string primitive referenced by the UEFI
/// crate when LLVM lowers a UTF-16 scan to the platform C ABI.
///
/// # Safety
///
/// `text` must point to a readable, NUL-terminated UTF-16 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcslen(text: *const u16) -> usize {
    let mut length = 0;
    loop {
        // Volatile reads keep this freestanding implementation from being
        // optimized back into a call to `wcslen`.
        if unsafe { read_volatile(text.add(length)) } == 0 {
            return length;
        }
        length += 1;
    }
}
