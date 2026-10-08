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
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::hint::spin_loop;
use core::ops::{Deref, DerefMut};
use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use oms_runtime::{
    AccessContext, BlockSnapshotBackend, CreateObject, CreateSpec, InMemoryObjectManager,
};
use oms_types::{
    CORE_EFFECT_TYPE, CORE_TERMINAL_TYPE, CORE_VALUE_TYPE, ObjectId, ObjectVersion, SubjectId,
    SYSTEM_SUBJECT, Value, seed_id_generator,
};
use ousject_provider::{EffectRecord, EffectStatus, ObjectProvider, ProviderError, ProviderOutcome};
use ousject_vm::{NativeCooperativeScheduler, NativeVirtualMachine, ProcessStatus};
use praxis_compiler::compile_program;
use tf_format::Token;
use ousject_platform::{
    BootInfo, MemoryRegion, MemoryRegionKind, MonotonicClock, PhysicalFrameAllocator,
    TerminalInputBuffer, TerminalInputEvent, TerminalTransport,
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
mod virtio_blk;

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
    timer::initialize();
    run_oms_smoke(&mut serial);
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

#[allow(clippy::arc_with_non_send_sync)] // Single-core Native OMS has no thread executor.
fn run_oms_smoke(serial: &mut com1::Com1) {
    serial_marker(serial, b"Ousject native: VirtIO block discovery started\r\n");
    let Ok(device) = virtio_blk::VirtioBlock::initialize() else {
        serial_marker(serial, b"Ousject native: VirtIO block discovery failed\r\n");
        enter_halt_loop();
    };
    serial_marker(serial, b"Ousject native: VirtIO block device ready\r\n");
    let backend = Arc::new(
        BlockSnapshotBackend::new(Arc::new(device)).unwrap_or_else(|_| enter_halt_loop()),
    );
    serial_marker(serial, b"Ousject native: OMS snapshot backend ready\r\n");
    serial_marker(serial, b"Ousject native: OMS snapshot recovery started\r\n");
    let Ok(manager) = InMemoryObjectManager::open_with_backend(backend.clone()) else {
        serial_marker(serial, b"Ousject native: OMS snapshot recovery failed\r\n");
        enter_halt_loop();
    };
    let manager = Rc::new(manager);
    serial_marker(serial, b"Ousject native: OMS initialized\r\n");
    serial_marker(serial, b"Ousject native: OMS block backend mounted\r\n");

    let system = AccessContext::new(SYSTEM_SUBJECT);
    let persistence_marker = Value::Text(String::from("Ousject native persistent OMS marker v1"));
    let headers = manager.list(system).unwrap_or_else(|_| enter_halt_loop());
    let existing_marker = headers.iter().find(|header| {
        header.type_id == CORE_VALUE_TYPE
            && manager.value(system, header.id).as_ref() == Ok(&persistence_marker)
    });
    if existing_marker.is_some() {
        assert!(headers.iter().any(|header| {
            header.type_id == CORE_VALUE_TYPE
                && manager.value(system, header.id).as_ref() == Ok(&Value::Integer(42))
        }));
        serial_marker(serial, b"Ousject native: prior boot OMS object recovered\r\n");
    } else {
        manager
            .create_object(system, CreateSpec::new("core.value", persistence_marker))
            .unwrap_or_else(|_| enter_halt_loop());
    }
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
    let recovered = InMemoryObjectManager::open_with_backend(backend)
        .unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(
        recovered.value(system, object).unwrap_or_else(|_| enter_halt_loop()),
        Value::Integer(42)
    );
    serial_marker(serial, b"Ousject native: OMS block persistence smoke passed\r\n");

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

#[allow(clippy::too_many_lines)] // Ordered smoke orchestration is easiest to audit inline.
fn run_vm_smoke(serial: &mut com1::Com1, manager: Rc<InMemoryObjectManager>) {
    let vm = NativeVirtualMachine::new(manager);
    vm.register_provider(Arc::new(NativeTerminalProvider::new()))
        .unwrap_or_else(|_| enter_halt_loop());
    vm.seal_providers()
        .unwrap_or_else(|_| enter_halt_loop());
    serial_marker(serial, b"Ousject native: VM initialized\r\n");
    run_native_process_recovery(serial, &vm);
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

    run_native_object_field_smoke(serial, &vm);
    run_native_scheduler_smoke(serial, &vm);
    let terminal = run_native_terminal_smoke(serial, &vm);
    run_native_terminal_input_smoke(serial, &vm, terminal);
    serial_marker(serial, b"Ousject native: OTF executed\r\n");
    serial_marker(serial, b"Ousject native: execution result verified\r\n");
    serial_marker(serial, b"Ousject native: VM smoke passed\r\n");

    let recovery_program = compile_program("func main() {\n return 73\n}\n")
        .unwrap_or_else(|_| enter_halt_loop());
    let recovery_process = vm
        .create_process(&recovery_program)
        .unwrap_or_else(|_| enter_halt_loop());
    let checkpoint = vm
        .run_slice(recovery_process, 1)
        .unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(checkpoint.status, ProcessStatus::Ready);
    assert!(vm
        .process_state(recovery_process)
        .unwrap_or_else(|_| enter_halt_loop())
        .token_position > 1);
    serial_marker(
        serial,
        b"Ousject native: restartable Praxis Process checkpoint committed\r\n",
    );
    run_native_process_wait_checkpoint(serial, &vm);
}

#[derive(Debug)]
struct NativeTerminalProvider {
    input: NativeSpinMutex<NativeTerminalInput>,
}

impl NativeTerminalProvider {
    const fn new() -> Self {
        Self {
            input: NativeSpinMutex::new(NativeTerminalInput {
                owner: None,
                line: TerminalInputBuffer::new(),
                secrets: BTreeMap::new(),
            }),
        }
    }
}

#[derive(Debug)]
struct NativeTerminalInput {
    owner: Option<ObjectId>,
    line: TerminalInputBuffer<1024>,
    secrets: BTreeMap<String, (ObjectId, String)>,
}

/// Small lock for boot-scoped single-core state shared behind `ObjectProvider`.
#[derive(Debug)]
struct NativeSpinMutex<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: Access to `value` is exclusive while `held` is true; the guard
// releases the lock with Release ordering after its last mutable reference.
unsafe impl<T: Send> Sync for NativeSpinMutex<T> {}

impl<T> NativeSpinMutex<T> {
    const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn lock(&self) -> NativeSpinGuard<'_, T> {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        NativeSpinGuard { mutex: self }
    }
}

struct NativeSpinGuard<'a, T> {
    mutex: &'a NativeSpinMutex<T>,
}

impl<T> Deref for NativeSpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The guard owns the mutex until Drop.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for NativeSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The guard owns the mutex until Drop and is unique.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for NativeSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.held.store(false, Ordering::Release);
    }
}

impl ObjectProvider for NativeTerminalProvider {
    fn type_id(&self) -> oms_types::TypeId {
        CORE_TERMINAL_TYPE
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        let mut fields = match initial {
            Value::Null => BTreeMap::new(),
            Value::Map(fields) | Value::Record(fields) => fields.clone(),
            _ => Err(ProviderError::InvalidArguments(
                "Terminal state must be a Record or null",
            ))?,
        };
        fields
            .entry(String::from("columns"))
            .or_insert(Value::Integer(80));
        fields
            .entry(String::from("rows"))
            .or_insert(Value::Integer(25));
        fields
            .entry(String::from("input_mode"))
            .or_insert(Value::Text(String::from("canonical")));
        fields
            .entry(String::from("echo"))
            .or_insert(Value::Bool(true));
        Ok(Value::Record(fields))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        match (capability, arguments) {
            ("print" | "println", [value]) => {
                let mut bytes = value.to_string().into_bytes();
                if capability == "println" {
                    bytes.extend_from_slice(b"\r\n");
                }
                let mut serial = com1::Com1;
                TerminalTransport::output(&mut serial, &bytes)
                    .map_err(|_| ProviderError::Adapter(String::from("COM1 output failed")))?;
                Ok(ProviderOutcome::result(Value::Null))
            }
            ("output", [Value::Bytes(bytes)]) => {
                let mut serial = com1::Com1;
                TerminalTransport::output(&mut serial, bytes)
                    .map_err(|_| ProviderError::Adapter(String::from("COM1 output failed")))?;
                Ok(ProviderOutcome::result(Value::Integer(
                    i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                )))
            }
            ("size", []) => Ok(ProviderOutcome::result(Value::Record(BTreeMap::from([
                (String::from("columns"), Value::Integer(80)),
                (String::from("rows"), Value::Integer(25)),
            ])))),
            ("is_interactive", []) => {
                Ok(ProviderOutcome::result(Value::Bool(true)))
            }
            ("read_line" | "read_secret", []) => Err(ProviderError::Pending),
            _ => Err(ProviderError::UnsupportedCapability(String::from(
                capability,
            ))),
        }
    }

    fn poll_for_process(
        &self,
        process: ObjectId,
        _object: ObjectId,
        state: &Value,
        capability: &str,
        _arguments: &[Value],
        effect: ObjectId,
    ) -> Result<Option<ProviderOutcome>, ProviderError> {
        let secret = capability == "read_secret";
        if !secret && capability != "read_line" {
            return Ok(None);
        }
        let echo = !secret
            && matches!(
                state,
                Value::Record(fields) | Value::Map(fields)
                    if !matches!(fields.get("echo"), Some(Value::Bool(false)))
            );
        let mut input = self.input.lock();
        match input.owner {
            Some(owner) if owner != process => return Ok(None),
            None => input.owner = Some(process),
            Some(_) => {}
        }
        let event = {
            let mut serial = com1::Com1;
            input.line.poll(&mut serial, echo).map_err(|error| {
                ProviderError::Adapter(alloc::format!("terminal input failed: {error:?}"))
            })?
        };
        match event {
            Some(TerminalInputEvent::Line(line)) => {
                input.owner = None;
                if secret {
                    let token = alloc::format!("secret:{effect}");
                    input.secrets.insert(token.clone(), (process, line));
                    Ok(Some(ProviderOutcome::result(Value::Text(token))))
                } else {
                    Ok(Some(ProviderOutcome::result(Value::Text(line))))
                }
            }
            Some(TerminalInputEvent::Interrupt) | None => Ok(None),
        }
    }

    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        ["input"].into_iter().map(String::from).collect()
    }

    fn invoke_ephemeral_for_process(
        &self,
        _process: ObjectId,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        if capability != "input" {
            return Err(ProviderError::UnsupportedCapability(String::from(
                capability,
            )));
        }
        let [Value::Integer(maximum)] = arguments else {
            return Err(ProviderError::InvalidArguments(
                "Terminal input expects a maximum byte count",
            ));
        };
        let maximum = usize::try_from(*maximum).map_err(|_| {
            ProviderError::InvalidArguments("input byte count must be between 1 and 65536")
        })?;
        if !(1..=65_536).contains(&maximum) {
            return Err(ProviderError::InvalidArguments(
                "input byte count must be between 1 and 65536",
            ));
        }
        let mut bytes = alloc::vec![0_u8; maximum];
        let mut serial = com1::Com1;
        let count = TerminalTransport::input(&mut serial, &mut bytes)
            .map_err(|_| ProviderError::Adapter(String::from("COM1 input failed")))?;
        bytes.truncate(count);
        Ok(ProviderOutcome::result(Value::Bytes(bytes)))
    }

    fn process_ended(&self, process: ObjectId) {
        let mut input = self.input.lock();
        if input.owner == Some(process) {
            input.owner = None;
            input.line = TerminalInputBuffer::new();
        }
        input
            .secrets
            .retain(|_, (owner, _)| *owner != process);
    }

    fn resolve_secret(&self, token: &str) -> Result<Option<String>, ProviderError> {
        Ok(self
            .input
            .lock()
            .secrets
            .get(token)
            .map(|(_, secret)| secret.clone()))
    }

    fn capabilities(&self) -> BTreeSet<String> {
        [
            "output",
            "input",
            "print",
            "println",
            "read_line",
            "read_secret",
            "size",
            "is_interactive",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}

fn run_native_terminal_smoke(serial: &mut com1::Com1, vm: &NativeVirtualMachine) -> ObjectId {
    let manager = vm.manager();
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let terminal_state = NativeTerminalProvider::new()
        .create(&Value::Null)
        .and_then(|state| state.encode().map_err(ProviderError::from))
        .unwrap_or_else(|_| enter_halt_loop());
    let mut terminal_request = CreateObject::new(CORE_TERMINAL_TYPE, terminal_state);
    terminal_request.capabilities = manager
        .type_by_id(CORE_TERMINAL_TYPE)
        .unwrap_or_else(|_| enter_halt_loop())
        .capabilities;
    let terminal = terminal_request.id;
    let mut transaction = manager.begin(context);
    transaction.create(terminal_request);
    manager
        .commit(transaction)
        .unwrap_or_else(|_| enter_halt_loop());

    let source = String::from(
        "func main() {\n terminal.println(\"Praxis reached Native core.terminal through a durable Effect.\")\n}\n"
    );
    let program = compile_program(&source).unwrap_or_else(|_| enter_halt_loop());
    let process = vm
        .create_process_with_bindings(
            &program,
            &BTreeMap::from([(String::from("terminal"), terminal)]),
        )
        .unwrap_or_else(|_| enter_halt_loop());
    let mut scheduler = NativeCooperativeScheduler::new(vm);
    scheduler
        .enqueue(process)
        .unwrap_or_else(|_| enter_halt_loop());
    let report = scheduler.run(32).unwrap_or_else(|error| {
        let mut diagnostic = PanicSerial(serial);
        let _ = writeln!(diagnostic, "Native Praxis object Terminal scheduler failed: {error:?}");
        enter_halt_loop()
    });
    if report.statuses.get(&process) != Some(&ProcessStatus::Halted) {
        let state = vm
            .process_state(process)
            .unwrap_or_else(|_| enter_halt_loop());
        let view = manager
            .read(context, process)
            .unwrap_or_else(|_| enter_halt_loop());
        let program_id = *view
            .links()
            .get("program")
            .unwrap_or_else(|| enter_halt_loop());
        let program = tf_format::Program::decode(
            manager
                .read(context, program_id)
                .unwrap_or_else(|_| enter_halt_loop())
                .state(),
        )
        .unwrap_or_else(|_| enter_halt_loop());
        let mut diagnostic = PanicSerial(serial);
        let _ = writeln!(
            diagnostic,
            "Native compiled println Process failed at token {} {:?}: {:?}",
            state.token_position,
            program.tokens.get(state.token_position as usize),
            state.error,
        );
    }
    assert_eq!(report.statuses.get(&process), Some(&ProcessStatus::Halted));
    let view = manager
        .read(context, process)
        .unwrap_or_else(|_| enter_halt_loop());
    assert!(!view.links().contains_key("$effect"));
    let durable_effect = manager
        .list(context)
        .unwrap_or_else(|_| enter_halt_loop())
        .into_iter()
        .filter(|header| header.type_id == CORE_EFFECT_TYPE)
        .filter_map(|header| manager.read(context, header.id).ok())
        .filter_map(|effect| EffectRecord::decode(effect.state()).ok())
        .find(|effect| effect.process == process && effect.capability == "println")
        .unwrap_or_else(|| enter_halt_loop());
    assert_eq!(durable_effect.status, EffectStatus::Completed);
    // Effect encoding uses a Null field to represent a void provider result.
    assert_eq!(durable_effect.result, None);
    serial_marker(serial, b"Ousject native: shared Terminal Provider Effect committed\r\n");
    terminal
}

fn run_native_terminal_input_smoke(
    serial: &mut com1::Com1,
    vm: &NativeVirtualMachine,
    terminal: ObjectId,
) {
    let source = String::from(
        "func format_input(line) {\n return \"Praxis input received: \" + line\n}\nfunc main() {\n terminal.println(\"Native COM1 input; type a line and press Enter: \")\n line = terminal.read_line()\n terminal.println(format_input(line))\n item = object.create(\"core.value\", { count: 40 })\n item.count = item.count + 2\n terminal.println(item.count)\n return line\n}\n"
    );
    let program = compile_program(&source).unwrap_or_else(|_| enter_halt_loop());
    let process = vm
        .create_process_with_bindings(
            &program,
            &BTreeMap::from([(String::from("terminal"), terminal)]),
        )
        .unwrap_or_else(|_| enter_halt_loop());
    let mut scheduler = NativeCooperativeScheduler::new(vm);
    scheduler
        .enqueue(process)
        .unwrap_or_else(|_| enter_halt_loop());
    let report = scheduler.run(32).unwrap_or_else(|error| {
        let mut diagnostic = PanicSerial(serial);
        let _ = writeln!(diagnostic, "Native Praxis object Terminal resume failed: {error:?}");
        enter_halt_loop()
    });
    assert_eq!(report.statuses.get(&process), Some(&ProcessStatus::Waiting));
    let waiting = vm
        .process_state(process)
        .unwrap_or_else(|_| enter_halt_loop());
    assert!(matches!(waiting.wait_reason, ousject_vm::WaitReason::Input(_)));
    serial_marker(serial, b"Ousject native: waiting for QEMU Terminal input\r\n");

    let start = timer::ticks();
    let mut awakened = false;
    while timer::ticks().wrapping_sub(start) < 3_000 {
        timer::wait_for_next_tick();
        if scheduler
            .poll_waiting_providers()
            .unwrap_or_else(|_| enter_halt_loop())
            > 0
        {
            awakened = true;
            break;
        }
    }
    assert!(awakened, "Native COM1 input did not wake the waiting Process");
    let report = scheduler.run(32).unwrap_or_else(|error| {
        let mut diagnostic = PanicSerial(serial);
        let _ = writeln!(diagnostic, "Native Praxis object Terminal resume failed: {error:?}");
        enter_halt_loop()
    });
    if report.statuses.get(&process) != Some(&ProcessStatus::Halted) {
        let state = vm.process_state(process).unwrap_or_else(|_| enter_halt_loop());
        let mut diagnostic = PanicSerial(serial);
        let _ = writeln!(diagnostic, "Native Praxis object Terminal status: {:?}, token={}, wait={:?}, error={:?}, current={:?}", state.status, state.token_position, state.wait_reason, state.error, program.tokens.get(state.token_position as usize));
        enter_halt_loop();
    }
    assert_eq!(report.statuses.get(&process), Some(&ProcessStatus::Halted));
    let state = vm
        .process_state(process)
        .unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(
        state.result,
        Some(Value::Text(String::from("native input works")))
    );
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let process_view = vm
        .manager()
        .read(system, process)
        .unwrap_or_else(|_| enter_halt_loop());
    let object_persisted = process_view.children().iter().any(|child| {
        vm.manager()
            .value(system, *child)
            .is_ok_and(|value| {
                value
                    == Value::Record(BTreeMap::from([(
                        String::from("count"),
                        Value::Integer(42),
                    )]))
            })
    });
    assert!(object_persisted, "Praxis-created Object was not persisted");
    serial_marker(
        serial,
        b"Ousject native: Terminal input Process resumed after serial line\r\n",
    );
}

fn run_native_process_recovery(serial: &mut com1::Com1, vm: &NativeVirtualMachine) {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let effects = vm
        .manager()
        .list(context)
        .unwrap_or_else(|_| enter_halt_loop());
    if effects.into_iter().any(|header| {
        if header.type_id != CORE_EFFECT_TYPE {
            return false;
        }
        let Ok(effect_view) = vm.manager().read(context, header.id) else {
            return false;
        };
        EffectRecord::decode(effect_view.state()).is_ok_and(|effect| {
            effect.capability == "println"
                && effect.status == EffectStatus::Completed
                && vm
                    .manager()
                    .inspect(context, effect.target)
                    .is_ok_and(|target| target.type_id == CORE_TERMINAL_TYPE)
        })
    }) {
        serial_marker(
            serial,
            b"Ousject native: completed Terminal Effect recovered after restart\r\n",
        );
    }
    let processes = vm
        .recover_ready_processes()
        .unwrap_or_else(|_| enter_halt_loop());
    if processes.is_empty() {
        return;
    }
    let mut scheduler = NativeCooperativeScheduler::new(vm);
    for process in &processes {
        scheduler
            .enqueue(*process)
            .unwrap_or_else(|_| enter_halt_loop());
    }
    let report = scheduler.run(4096).unwrap_or_else(|_| enter_halt_loop());
    for process in processes {
        assert_eq!(report.statuses.get(&process), Some(&ProcessStatus::Halted));
        assert_eq!(
            vm.process_state(process)
                .unwrap_or_else(|_| enter_halt_loop())
                .result,
            Some(tf_format::Value::Integer(73))
        );
    }
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let resumed_waiter = report.statuses.keys().any(|process| {
        let Ok(state) = vm.process_state(*process) else {
            return false;
        };
        if state.result != Some(tf_format::Value::Text(String::from("halted"))) {
            return false;
        }
        let Ok(process_view) = vm.manager().read(system, *process) else {
            return false;
        };
        let Some(program_id) = process_view.links().get("program") else {
            return false;
        };
        let Ok(program_view) = vm.manager().read(system, *program_id) else {
            return false;
        };
        let Ok(program) = tf_format::Program::decode(program_view.state()) else {
            return false;
        };
        let Some(Token::Push(tf_format::Value::Text(child_id))) = program.tokens.first() else {
            return false;
        };
        let Ok(child_id) = child_id.parse::<ObjectId>() else {
            return false;
        };
        vm.process_state(child_id)
            .is_ok_and(|child| child.result == Some(tf_format::Value::Integer(73)))
    });
    assert!(resumed_waiter, "persisted Process.wait parent did not resume");
    serial_marker(serial, b"Ousject native: Process waiter resumed after restart\r\n");
    serial_marker(serial, b"Ousject native: Praxis Process resumed after restart\r\n");
}

fn run_native_process_wait_checkpoint(serial: &mut com1::Com1, vm: &NativeVirtualMachine) {
    let child = vm
        .create_process(&tf_format::Program {
            tokens: alloc::vec![Token::Push(Value::Integer(73)), Token::Halt],
        })
        .unwrap_or_else(|_| enter_halt_loop());
    let parent = vm
        .create_process(&tf_format::Program {
            tokens: alloc::vec![
                Token::Push(Value::Text(child.to_string())),
                Token::ObjectCall {
                    method: String::from("wait"),
                    arguments: 0,
                },
                Token::Halt,
            ],
        })
        .unwrap_or_else(|_| enter_halt_loop());
    let report = vm.run_slice(parent, 2).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(report.status, ProcessStatus::Waiting);
    assert_eq!(
        vm.process_state(parent)
            .unwrap_or_else(|_| enter_halt_loop())
            .wait_reason,
        ousject_vm::WaitReason::Process(child)
    );
    serial_marker(serial, b"Ousject native: Process wait checkpoint committed\r\n");
}

fn run_native_object_field_smoke(serial: &mut com1::Com1, vm: &NativeVirtualMachine) {
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let object = vm
        .manager()
        .create_object(
            system,
            CreateSpec::new(
                "core.value",
                Value::Record(BTreeMap::from([("count".to_owned(), Value::Integer(1))])),
            ),
        )
        .unwrap_or_else(|_| enter_halt_loop());
    let program = tf_format::Program {
        tokens: alloc::vec![
            Token::Push(Value::Text(object.to_string())),
            Token::Push(Value::Integer(9)),
            Token::SetField(String::from("count")),
            Token::Push(Value::Text(object.to_string())),
            Token::GetField(String::from("count")),
            Token::Halt,
        ],
    };
    let process = vm
        .create_process(&program)
        .unwrap_or_else(|_| enter_halt_loop());
    let report = vm.run_slice(process, 16).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        vm.manager()
            .value(system, object)
            .unwrap_or_else(|_| enter_halt_loop()),
        Value::Record(BTreeMap::from([("count".to_owned(), Value::Integer(9))]))
    );
    assert_eq!(
        vm.process_state(process)
            .unwrap_or_else(|_| enter_halt_loop())
            .result,
        Some(Value::Integer(9))
    );
    serial_marker(serial, b"Ousject native: Praxis OTF object field read and write passed\r\n");
}

#[allow(clippy::too_many_lines)] // Keep deterministic scheduler setup and assertions together.
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

    let child = vm
        .create_process(&tf_format::Program {
            tokens: alloc::vec![Token::Push(Value::Integer(23)), Token::Halt],
        })
        .unwrap_or_else(|_| enter_halt_loop());
    let parent = vm
        .create_process(&tf_format::Program {
            tokens: alloc::vec![
                Token::Push(Value::Text(child.to_string())),
                Token::ObjectCall {
                    method: String::from("wait"),
                    arguments: 0,
                },
                Token::Halt,
            ],
        })
        .unwrap_or_else(|_| enter_halt_loop());
    let mut wait_scheduler = NativeCooperativeScheduler::new(vm);
    wait_scheduler.set_slice_tokens(1);
    wait_scheduler
        .enqueue(parent)
        .unwrap_or_else(|_| enter_halt_loop());
    wait_scheduler
        .enqueue(child)
        .unwrap_or_else(|_| enter_halt_loop());
    wait_scheduler.run(3).unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(
        vm.process_state(parent)
            .unwrap_or_else(|_| enter_halt_loop())
            .status,
        ProcessStatus::Waiting
    );
    let wait_completion = wait_scheduler
        .run(20)
        .unwrap_or_else(|_| enter_halt_loop());
    assert_eq!(
        wait_completion.statuses.get(&parent),
        Some(&ProcessStatus::Halted)
    );
    assert_eq!(
        vm.process_state(parent)
            .unwrap_or_else(|_| enter_halt_loop())
            .result,
        Some(Value::Text(String::from("halted")))
    );
    assert_eq!(
        vm.process_state(child)
            .unwrap_or_else(|_| enter_halt_loop())
            .result,
        Some(Value::Integer(23))
    );
    serial_marker(serial, b"Ousject native: Process A executed\r\n");
    serial_marker(serial, b"Ousject native: Process B executed\r\n");
    serial_marker(serial, b"Ousject native: Process wait and wakeup passed\r\n");
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
