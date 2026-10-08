#![no_main]
#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

#[cfg(not(target_os = "uefi"))]
compile_error!("ousject-native-image must be built for x86_64-unknown-uefi");
#[cfg(any(
    all(feature = "fault-smoke", feature = "panic-smoke"),
    all(feature = "fault-smoke", feature = "double-fault-smoke"),
    all(feature = "panic-smoke", feature = "double-fault-smoke")
))]
compile_error!("Native smoke modes are mutually exclusive");

use core::alloc::{GlobalAlloc, Layout};
use core::fmt::{self, Write};
use core::hint::spin_loop;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicUsize, Ordering};
use oms_runtime::{AccessContext, CreateObject, CreateSpec, InMemoryObjectManager};
use oms_types::{
    CORE_VALUE_TYPE, ObjectId, ObjectVersion, SubjectId, SYSTEM_SUBJECT, Value,
    seed_id_generator,
};
use ousject_vm::{NativeCooperativeScheduler, NativeVirtualMachine, ProcessStatus};
use praxis_compiler::compile_program;
use tf_format::Token;
use ousject_platform::{
    BootInfo, MemoryRegion, MemoryRegionKind, MonotonicClock, PhysicalFrameAllocator,
    TerminalTransport,
};
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::proto::console::serial::Serial;
use uefi::proto::rng::Rng;

mod clock;
mod com1;
mod cpu;
mod heap;
mod paging;
mod timer;

#[global_allocator]
static GLOBAL_HEAP: heap::BootstrapHeap = heap::BootstrapHeap::new();
static HEAP_RANGE_START: AtomicUsize = AtomicUsize::new(0);
static HEAP_RANGE_LENGTH: AtomicUsize = AtomicUsize::new(0);

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

    let boot_id_prefix = acquire_boot_id_prefix();

    // SAFETY: No boot-services protocol handle is retained across this call.
    // The memory map is returned in loader-owned memory and remains reserved.
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
        boot_id_prefix,
    };
    native_kernel_entry(boot_info)
}

fn acquire_boot_id_prefix() -> Option<u64> {
    let firmware_prefix = uefi::boot::get_handle_for_protocol::<Rng>()
        .ok()
        .and_then(|handle| uefi::boot::open_protocol_exclusive::<Rng>(handle).ok())
        .and_then(|mut rng| {
            let mut bytes = [0_u8; 8];
            rng.get_rng(None, &mut bytes)
                .ok()
                .map(|()| u64::from_le_bytes(bytes))
        });
    firmware_prefix.or_else(cpu::hardware_random_u64)
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
    let Some(heap_range) = frame_allocator.allocate_contiguous(
        boot_info.memory_map,
        heap::HEAP_SIZE,
        4096,
        paging::MAX_MAPPED_ADDRESS,
    ) else {
        enter_halt_loop();
    };

    let mut serial = com1::Com1::initialize();
    if serial
        .output(b"Ousject native: Boot Services exited; COM1 and physical memory online\r\n")
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
    let Some(id_prefix) = boot_info.boot_id_prefix else {
        let _ = serial.output(b"Ousject native: UEFI RNG unavailable; OMS not started\r\n");
        enter_halt_loop();
    };
    if seed_id_generator(id_prefix).is_err() {
        let _ = serial.output(b"Ousject native: ObjectId generator initialization failed\r\n");
        enter_halt_loop();
    }
    HEAP_RANGE_START.store(
        usize::try_from(heap_range.start).unwrap_or_else(|_| enter_halt_loop()),
        Ordering::Relaxed,
    );
    HEAP_RANGE_LENGTH.store(
        usize::try_from(heap_range.length).unwrap_or_else(|_| enter_halt_loop()),
        Ordering::Release,
    );
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
    let heap_range = ousject_platform::MemoryRange {
        start: u64::try_from(HEAP_RANGE_START.load(Ordering::Relaxed))
            .unwrap_or_else(|_| enter_halt_loop()),
        length: u64::try_from(HEAP_RANGE_LENGTH.load(Ordering::Acquire))
            .unwrap_or_else(|_| enter_halt_loop()),
    };
    if GLOBAL_HEAP.initialize(heap_range).is_err() {
        let _ = serial.output(b"Ousject native: heap initialization failed\r\n");
        enter_halt_loop();
    }
    if serial
        .output(b"Ousject native: heap initialized\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    {
        let mut heap_diagnostic = PanicSerial(&mut serial);
        if writeln!(
            heap_diagnostic,
            "Ousject native heap: start={:#018x} length={:#x}",
            heap_range.start,
            heap_range.length
        )
        .is_err()
        {
            enter_halt_loop();
        }
    }
    run_alloc_smoke();
    if serial
        .output(b"Ousject native: alloc smoke passed\r\n")
        .is_err()
    {
        enter_halt_loop();
    }
    run_alloc_reclamation_smoke();
    serial_marker(&mut serial, b"Ousject native: heap reclamation smoke passed\r\n");
    run_oms_smoke(&mut serial);
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

fn run_oms_smoke(serial: &mut com1::Com1) {
    let manager = Rc::new(InMemoryObjectManager::new(4).unwrap_or_else(|_| enter_halt_loop()));
    serial_marker(serial, b"Ousject native: OMS initialized\r\n");

    let system = AccessContext::new(SYSTEM_SUBJECT);
    let object = manager
        .create_object(system, CreateSpec::new("core.value", Value::Integer(40)))
        .unwrap_or_else(|_| enter_halt_loop());
    serial_marker(serial, b"Ousject native: object created\r\n");

    let initial = manager.value(system, object).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(initial, Value::Integer(40));
    manager
        .replace_value(system, object, &Value::Integer(42))
        .unwrap_or_else(|_| enter_halt_loop());
    serial_marker(serial, b"Ousject native: transaction committed\r\n");
    assert_eq!(
        manager.value(system, object).unwrap_or_else(|_| enter_halt_loop()),
        Value::Integer(42)
    );

    let child_id = manager
        .create_object(
            system,
            CreateSpec::new("core.value", Value::Text(String::from("child")))
                .with_parent(object)
                .with_link("root", object),
        )
        .unwrap_or_else(|_| enter_halt_loop());
    let parent = manager.read(system, object).unwrap_or_else(|_| enter_halt_loop());
    let child = manager.read(system, child_id).unwrap_or_else(|_| enter_halt_loop());
    assert!(parent.children().contains(&child_id));
    assert_eq!(child.header().parent_id, Some(object));
    assert_eq!(child.links().get("root"), Some(&object));

    let unauthorized = AccessContext::new(SubjectId::from_u128(0xfeed));
    assert!(manager.read(unauthorized, object).is_err());

    let failed_id = ObjectId::new();
    let mut failed = manager.begin(system);
    failed
        .expect(object, ObjectVersion::new(0))
        .update_state(object, Value::Integer(99).encode().unwrap_or_else(|_| enter_halt_loop()))
        .create(
            CreateObject::new(
                CORE_VALUE_TYPE,
                Value::Text(String::from("must not publish"))
                    .encode()
                    .unwrap_or_else(|_| enter_halt_loop()),
            )
            .with_id(failed_id),
        );
    assert!(manager.commit(failed).is_err());
    assert_eq!(
        manager.value(system, object).unwrap_or_else(|_| enter_halt_loop()),
        Value::Integer(42)
    );
    assert!(manager.read(system, failed_id).is_err());
    serial_marker(serial, b"Ousject native: object value verified\r\n");
    serial_marker(serial, b"Ousject native: OMS smoke passed\r\n");

    run_vm_smoke(serial, manager);
}

fn run_vm_smoke(serial: &mut com1::Com1, manager: Rc<InMemoryObjectManager>) {
    let vm = NativeVirtualMachine::new(manager);
    serial_marker(serial, b"Ousject native: VM initialized\r\n");
    let program = compile_program("func main() {\n value = 0\n while value < 10 {\n  value = value + 1\n }\n return value\n}\n")
        .unwrap_or_else(|_| enter_halt_loop());
    let return_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, Token::Return))
        .and_then(|position| u32::try_from(position).ok())
        .unwrap_or_else(|| enter_halt_loop());
    let process = vm
        .create_process(&program)
        .unwrap_or_else(|_| enter_halt_loop());
    let process_view = vm.manager().read(AccessContext::new(SYSTEM_SUBJECT), process)
        .unwrap_or_else(|_| enter_halt_loop());
    let program_id = *process_view.links().get("program").unwrap_or_else(|| enter_halt_loop());
    assert_eq!(process_view.header().type_id, ousject_vm::PROCESS_TYPE);
    assert_eq!(vm.manager().read(AccessContext::new(SYSTEM_SUBJECT), program_id)
        .unwrap_or_else(|_| enter_halt_loop()).header().type_id, ousject_vm::PROGRAM_TYPE);
    serial_marker(serial, b"Ousject native: Program Object created\r\n");
    serial_marker(serial, b"Ousject native: Process Object created\r\n");

    let mut report = vm.run_slice(process, 7).unwrap_or_else(|_| enter_halt_loop());
    assert!(report.steps > 0);
    assert_eq!(report.status, ProcessStatus::Ready);
    let slice_state = vm.process_state(process).unwrap_or_else(|_| enter_halt_loop());
    let locals = &slice_state.frames.last().unwrap_or_else(|| enter_halt_loop()).locals;
    let value_id = *locals.get("value").unwrap_or_else(|| enter_halt_loop());
    assert_eq!(vm.manager().value(AccessContext::new(SYSTEM_SUBJECT), value_id)
        .unwrap_or_else(|_| enter_halt_loop()), Value::Integer(0));
    serial_marker(serial, b"Ousject native: Process state committed\r\n");
    let mut slices = 1;
    let mut current = slice_state;
    while current.token_position != return_position && slices < 256 {
        let _ = vm.run_slice(process, 1).unwrap_or_else(|_| enter_halt_loop());
        current = vm.process_state(process).unwrap_or_else(|_| enter_halt_loop());
        slices += 1;
    }
    assert_eq!(current.token_position, return_position);
    let final_local = *current
        .frames
        .last()
        .and_then(|frame| frame.locals.get("value"))
        .unwrap_or_else(|| enter_halt_loop());
    assert_eq!(
        vm.manager()
            .value(AccessContext::new(SYSTEM_SUBJECT), final_local)
            .unwrap_or_else(|_| enter_halt_loop()),
        Value::Integer(10)
    );
    report = vm.run_slice(process, 1).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(report.status, ProcessStatus::Ready);
    report = vm.run_slice(process, 1).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(report.status, ProcessStatus::Halted);
    let state = vm.process_state(process).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(state.result, Some(Value::Integer(10)));
    assert_eq!(state.stack.last(), Some(&Value::Integer(10)));
    assert_eq!(state.status, ProcessStatus::Halted);
    assert!(state.token_position > 0);
    assert_eq!(state.frames, Vec::<ousject_vm::CallFrame>::new());
    assert_eq!(state.variables, BTreeMap::new());

    run_native_scheduler_smoke(serial, &vm);

    let unavailable = tf_format::Program {
        tokens: alloc::vec![
            Token::ObjectCall {
                method: String::from("println"),
                arguments: 0,
            },
            Token::Halt,
        ],
    };
    let unavailable_process = vm
        .create_process(&unavailable)
        .unwrap_or_else(|_| enter_halt_loop());
    assert!(matches!(
        vm.run_slice(unavailable_process, 1),
        Err(ousject_vm::VmError::MissingProvider("Native token service"))
    ));
    let unavailable_state = vm
        .process_state(unavailable_process)
        .unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(unavailable_state.status, ProcessStatus::Failed);
    assert!(unavailable_state.error.is_some());
    serial_marker(serial, b"Ousject native: OTF executed\r\n");
    serial_marker(serial, b"Ousject native: execution result verified\r\n");
    serial_marker(serial, b"Ousject native: VM smoke passed\r\n");
}

fn run_native_scheduler_smoke(serial: &mut com1::Com1, vm: &NativeVirtualMachine) {
    let source = "func main() {\n counter = 0\n while counter < 8 {\n  counter = counter + 1\n }\n return counter\n}\n";
    let program = compile_program(source).unwrap_or_else(|_| enter_halt_loop());
    let process_a = vm
        .create_process(&program)
        .unwrap_or_else(|_| enter_halt_loop());
    let process_b = vm
        .create_process(&program)
        .unwrap_or_else(|_| enter_halt_loop());
    let failed_program = tf_format::Program {
        tokens: alloc::vec![
            Token::ObjectCall {
                method: String::from("missing_native_service"),
                arguments: 0,
            },
            Token::Halt,
        ],
    };
    let failed_process = vm
        .create_process(&failed_program)
        .unwrap_or_else(|_| enter_halt_loop());

    let mut scheduler = NativeCooperativeScheduler::new(vm);
    scheduler.set_slice_tokens(1);
    scheduler.enqueue(process_a).unwrap_or_else(|_| enter_halt_loop());
    scheduler.enqueue(failed_process).unwrap_or_else(|_| enter_halt_loop());
    scheduler.enqueue(process_b).unwrap_or_else(|_| enter_halt_loop());
    serial_marker(serial, b"Ousject native: Scheduler initialized\r\n");

    let first_round = scheduler.run(3).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(first_round.steps, 3);
    let state_a = vm.process_state(process_a).unwrap_or_else(|_| enter_halt_loop());
    let state_b = vm.process_state(process_b).unwrap_or_else(|_| enter_halt_loop());
    assert!(state_a.token_position > 1);
    assert!(state_b.token_position > 1);
    assert_eq!(
        vm.process_state(failed_process)
            .unwrap_or_else(|_| enter_halt_loop())
            .status,
        ProcessStatus::Failed
    );

    let completion = scheduler.run(2_000).unwrap_or_else(|_| enter_halt_loop());
    assert!(completion.steps > 0);
    let state_a = vm.process_state(process_a).unwrap_or_else(|_| enter_halt_loop());
    let state_b = vm.process_state(process_b).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(state_a.status, ProcessStatus::Halted);
    assert_eq!(state_b.status, ProcessStatus::Halted);
    assert_eq!(state_a.result, Some(tf_format::Value::Integer(8)));
    assert_eq!(state_b.result, Some(tf_format::Value::Integer(8)));
    assert_eq!(
        completion.statuses.get(&failed_process),
        Some(&ProcessStatus::Failed)
    );
    serial_marker(serial, b"Ousject native: Process A executed\r\n");
    serial_marker(serial, b"Ousject native: Process B executed\r\n");
    serial_marker(serial, b"Ousject native: Scheduler smoke passed\r\n");
}

fn serial_marker(serial: &mut com1::Com1, marker: &[u8]) {
    if serial.output(marker).is_err() {
        enter_halt_loop();
    }
}

fn run_alloc_smoke() {
    let boxed = Box::new(0x0bad_f00d_u64);
    assert_eq!(*boxed, 0x0bad_f00d);

    let mut values = Vec::new();
    for value in 0..4096_u64 {
        values.push(value);
    }
    assert_eq!(values.len(), 4096);
    assert_eq!(values[4095], 4095);

    let mut text = String::from("Ousject");
    text.push_str(" Native");
    assert_eq!(text.as_str(), "Ousject Native");

    let mut map = BTreeMap::new();
    for key in 0..256_u64 {
        map.insert(key, key * 3);
    }
    assert_eq!(map.get(&255), Some(&765));

    let shared = Arc::new(String::from("shared"));
    let cloned = Arc::clone(&shared);
    assert!(Arc::ptr_eq(&shared, &cloned));
}

fn run_alloc_reclamation_smoke() {
    let available_before = GLOBAL_HEAP.free_bytes();
    for _ in 0..2048 {
        let mut allocation = Vec::with_capacity(4096);
        for byte in 0_usize..4096 {
            allocation.push(byte.to_le_bytes()[0]);
        }
        assert_eq!(allocation[4095], 0xff);
        drop(allocation);
    }
    let layout = Layout::from_size_align(4096, 4096).unwrap_or_else(|_| enter_halt_loop());
    // SAFETY: this directly exercises one allocation/deallocation pair using
    // the exact Layout passed to the global allocator.
    let aligned = unsafe { GLOBAL_HEAP.alloc(layout) };
    assert!(!aligned.is_null());
    assert_eq!(aligned as usize % 4096, 0);
    // SAFETY: `aligned` was returned by this allocator for `layout` above.
    unsafe { GLOBAL_HEAP.dealloc(aligned, layout) };
    assert!(GLOBAL_HEAP.free_bytes() >= available_before.saturating_sub(64));
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

#[alloc_error_handler]
fn allocation_error(layout: Layout) -> ! {
    let mut serial = com1::Com1::initialize();
    let _ = serial.output(b"Ousject native: heap out of memory; requested layout ");
    let mut output = PanicSerial(&mut serial);
    let _ = write!(output, "size={} align={}\r\n", layout.size(), layout.align());
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
