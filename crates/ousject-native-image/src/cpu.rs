//! Early `x86_64` exception gates. Hardware interrupts remain disabled.

use core::arch::{asm, global_asm};
use ousject_platform::TerminalTransport;

const IDT_ENTRIES: usize = 256;
const GATE_INTERRUPT_PRESENT_RING0: u8 = 0x8e;
const KERNEL_CODE_SELECTOR: u16 = 0x08;
const KERNEL_DATA_SELECTOR: u16 = 0x10;
const TSS_SELECTOR: u16 = 0x18;
const STACK_SIZE: usize = 16 * 1024;
const DOUBLE_FAULT_IST: u8 = 1;
const NMI_IST: u8 = 2;
#[cfg(feature = "fault-smoke")]
const SMOKE_IST: u8 = 3;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtGate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtGate {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_middle: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn interrupt(handler: usize, selector: u16, ist: u8) -> Self {
        let address = handler as u64;
        let low_bytes = address.to_le_bytes();
        Self {
            offset_low: u16::from_le_bytes([low_bytes[0], low_bytes[1]]),
            selector,
            ist: ist & 0x07,
            attributes: GATE_INTERRUPT_PRESENT_RING0,
            offset_middle: u16::from_le_bytes([low_bytes[2], low_bytes[3]]),
            offset_high: u32::from_le_bytes([
                low_bytes[4],
                low_bytes[5],
                low_bytes[6],
                low_bytes[7],
            ]),
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
struct TaskStateSegment {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    io_map_base: u16,
}

impl TaskStateSegment {
    const EMPTY: Self = Self {
        reserved0: 0,
        rsp: [0; 3],
        reserved1: 0,
        ist: [0; 7],
        reserved2: 0,
        reserved3: 0,
        // The packed 64-bit TSS layout above is 104 bytes by the x86-64 SDM.
        io_map_base: 104,
    };
}

#[repr(align(16))]
struct CpuStack {
    _bytes: [u8; STACK_SIZE],
}

const EMPTY_STACK: CpuStack = CpuStack {
    _bytes: [0; STACK_SIZE],
};

static mut GDT: [u64; 5] = [0; 5];
static mut TSS: TaskStateSegment = TaskStateSegment::EMPTY;
static mut KERNEL_STACK: CpuStack = EMPTY_STACK;
static mut DOUBLE_FAULT_STACK: CpuStack = EMPTY_STACK;
static mut NMI_STACK: CpuStack = EMPTY_STACK;
static mut SMOKE_STACK: CpuStack = EMPTY_STACK;

static mut IDT: [IdtGate; IDT_ENTRIES] = [IdtGate::MISSING; IDT_ENTRIES];

global_asm!(
    r#"
    .macro OUSJECT_EXCEPTION_STUB number
    .globl ousject_exception_stub_\number
ousject_exception_stub_\number:
    mov rcx, \number
    jmp ousject_exception_common
    .endm

    .macro OUSJECT_NO_ERROR_STUB number
    .globl ousject_exception_stub_\number
ousject_exception_stub_\number:
    push 0
    mov rcx, \number
    jmp ousject_exception_common
    .endm

    OUSJECT_NO_ERROR_STUB 0
    OUSJECT_NO_ERROR_STUB 1
    OUSJECT_NO_ERROR_STUB 2
    OUSJECT_NO_ERROR_STUB 3
    OUSJECT_NO_ERROR_STUB 4
    OUSJECT_NO_ERROR_STUB 5
    OUSJECT_NO_ERROR_STUB 6
    OUSJECT_NO_ERROR_STUB 7
    OUSJECT_EXCEPTION_STUB 8
    OUSJECT_NO_ERROR_STUB 9
    OUSJECT_EXCEPTION_STUB 10
    OUSJECT_EXCEPTION_STUB 11
    OUSJECT_EXCEPTION_STUB 12
    OUSJECT_EXCEPTION_STUB 13
    OUSJECT_EXCEPTION_STUB 14
    OUSJECT_NO_ERROR_STUB 15
    OUSJECT_NO_ERROR_STUB 16
    OUSJECT_EXCEPTION_STUB 17
    OUSJECT_NO_ERROR_STUB 18
    OUSJECT_NO_ERROR_STUB 19
    OUSJECT_NO_ERROR_STUB 20
    OUSJECT_EXCEPTION_STUB 21
    OUSJECT_NO_ERROR_STUB 22
    OUSJECT_NO_ERROR_STUB 23
    OUSJECT_NO_ERROR_STUB 24
    OUSJECT_NO_ERROR_STUB 25
    OUSJECT_NO_ERROR_STUB 26
    OUSJECT_NO_ERROR_STUB 27
    OUSJECT_NO_ERROR_STUB 28
    OUSJECT_EXCEPTION_STUB 29
    OUSJECT_EXCEPTION_STUB 30
    OUSJECT_NO_ERROR_STUB 31

ousject_exception_default_stub:
    push 0
    mov rcx, 255
ousject_exception_common:
    mov rdx, [rsp]
    mov r8, [rsp + 8]
    cli
    and rsp, -16
    sub rsp, 32
    call ousject_exception_handler
    ud2
    "#,
);

unsafe extern "efiapi" {
    fn ousject_exception_stub_0();
    fn ousject_exception_stub_1();
    fn ousject_exception_stub_2();
    fn ousject_exception_stub_3();
    fn ousject_exception_stub_4();
    fn ousject_exception_stub_5();
    fn ousject_exception_stub_6();
    fn ousject_exception_stub_7();
    fn ousject_exception_stub_8();
    fn ousject_exception_stub_9();
    fn ousject_exception_stub_10();
    fn ousject_exception_stub_11();
    fn ousject_exception_stub_12();
    fn ousject_exception_stub_13();
    fn ousject_exception_stub_14();
    fn ousject_exception_stub_15();
    fn ousject_exception_stub_16();
    fn ousject_exception_stub_17();
    fn ousject_exception_stub_18();
    fn ousject_exception_stub_19();
    fn ousject_exception_stub_20();
    fn ousject_exception_stub_21();
    fn ousject_exception_stub_22();
    fn ousject_exception_stub_23();
    fn ousject_exception_stub_24();
    fn ousject_exception_stub_25();
    fn ousject_exception_stub_26();
    fn ousject_exception_stub_27();
    fn ousject_exception_stub_28();
    fn ousject_exception_stub_29();
    fn ousject_exception_stub_30();
    fn ousject_exception_stub_31();
    fn ousject_exception_default_stub();
}

const EXCEPTION_STUBS: [unsafe extern "efiapi" fn(); 32] = [
    ousject_exception_stub_0,
    ousject_exception_stub_1,
    ousject_exception_stub_2,
    ousject_exception_stub_3,
    ousject_exception_stub_4,
    ousject_exception_stub_5,
    ousject_exception_stub_6,
    ousject_exception_stub_7,
    ousject_exception_stub_8,
    ousject_exception_stub_9,
    ousject_exception_stub_10,
    ousject_exception_stub_11,
    ousject_exception_stub_12,
    ousject_exception_stub_13,
    ousject_exception_stub_14,
    ousject_exception_stub_15,
    ousject_exception_stub_16,
    ousject_exception_stub_17,
    ousject_exception_stub_18,
    ousject_exception_stub_19,
    ousject_exception_stub_20,
    ousject_exception_stub_21,
    ousject_exception_stub_22,
    ousject_exception_stub_23,
    ousject_exception_stub_24,
    ousject_exception_stub_25,
    ousject_exception_stub_26,
    ousject_exception_stub_27,
    ousject_exception_stub_28,
    ousject_exception_stub_29,
    ousject_exception_stub_30,
    ousject_exception_stub_31,
];

/// Installs an owned GDT and TSS, then loads the early fatal exception IDT.
pub fn initialize() {
    initialize_gdt_and_tss();
    initialize_idt();
}

/// Reads the CPU-reported physical-address width without assuming a model.
#[must_use]
pub fn physical_address_bits() -> Option<u8> {
    let maximum = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if maximum < 0x8000_0008 {
        return None;
    }

    let width = u8::try_from(core::arch::x86_64::__cpuid(0x8000_0008).eax & 0xff).ok()?;
    (32..=52).contains(&width).then_some(width)
}

/// Returns one boot-unique prefix from a feature-detected hardware RNG.
///
/// RDSEED is preferred because it supplies seed material; RDRAND is a
/// hardware DRBG fallback. Both instructions are attempted only after CPUID
/// reports support, and carry failure is retried a bounded number of times.
pub fn hardware_random_u64() -> Option<u64> {
    let basic = core::arch::x86_64::__cpuid(0);
    if basic.eax < 1 {
        return None;
    }
    let features = core::arch::x86_64::__cpuid(1).ecx;
    let has_rdrand = features & (1 << 30) != 0;
    let has_rdseed = basic.eax >= 7 && core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0;
    if !has_rdseed && !has_rdrand {
        return None;
    }

    for _ in 0..10 {
        let mut value = 0_u64;
        let success: u8;
        // SAFETY: CPUID above gates each instruction on the advertised CPU
        // feature. Carry reports whether the instruction produced a value.
        unsafe {
            if has_rdseed {
                core::arch::asm!(
                    "rdseed {value}",
                    "setc {success}",
                    value = out(reg) value,
                    success = out(reg_byte) success,
                    options(nomem, nostack)
                );
            } else {
                core::arch::asm!(
                    "rdrand {value}",
                    "setc {success}",
                    value = out(reg) value,
                    success = out(reg_byte) success,
                    options(nomem, nostack)
                );
            }
        }
        if success != 0 {
            return Some(value);
        }
        core::hint::spin_loop();
    }
    None
}

fn initialize_gdt_and_tss() {
    // SAFETY: The GDT, TSS, and stacks are static LoaderData memory, so the
    // firmware memory map will not give these pages to the frame allocator.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
        let kernel_stack = stack_top(core::ptr::addr_of_mut!(KERNEL_STACK));
        let double_fault_stack = stack_top(core::ptr::addr_of_mut!(DOUBLE_FAULT_STACK));
        let nmi_stack = stack_top(core::ptr::addr_of_mut!(NMI_STACK));
        let smoke_stack = stack_top(core::ptr::addr_of_mut!(SMOKE_STACK));
        let tss = core::ptr::addr_of_mut!(TSS);
        tss.write(TaskStateSegment::EMPTY);
        core::ptr::addr_of_mut!((*tss).rsp[0]).write_unaligned(kernel_stack);
        core::ptr::addr_of_mut!((*tss).ist[0]).write_unaligned(double_fault_stack);
        core::ptr::addr_of_mut!((*tss).ist[1]).write_unaligned(nmi_stack);
        core::ptr::addr_of_mut!((*tss).ist[2]).write_unaligned(smoke_stack);

        let tss_base = tss as u64;
        let (tss_low, tss_high) = encode_tss_descriptor(
            tss_base,
            u32::try_from(core::mem::size_of::<TaskStateSegment>() - 1)
                .expect("TSS limit fits its descriptor field"),
        );
        let gdt = core::ptr::addr_of_mut!(GDT).cast::<u64>();
        gdt.add(0).write(0);
        // Long mode code and flat data descriptors with the accessed bit set.
        gdt.add(1).write(0x00af_9b00_0000_ffff);
        gdt.add(2).write(0x00cf_9300_0000_ffff);
        gdt.add(3).write(tss_low);
        gdt.add(4).write(tss_high);

        let pointer = DescriptorTablePointer {
            limit: u16::try_from(core::mem::size_of::<[u64; 5]>() - 1)
                .expect("GDT limit fits its descriptor field"),
            base: gdt as u64,
        };
        asm!(
            "lgdt [{pointer}]",
            "push {code_selector}",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, {data_selector}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ax, {tss_selector}",
            "ltr ax",
            pointer = in(reg) core::ptr::addr_of!(pointer),
            code_selector = const KERNEL_CODE_SELECTOR,
            data_selector = const KERNEL_DATA_SELECTOR,
            tss_selector = const TSS_SELECTOR,
            out("rax") _,
            options(preserves_flags)
        );
    }
}

/// Transfers execution to the statically reserved Native ring-0 stack.
///
/// This also leaves the TSS `RSP0` entry pointing at the stack top, ready for
/// a future ring-3 transition. The current image remains ring 0.
pub fn enter_kernel_stack(entry: extern "efiapi" fn() -> !) -> ! {
    // SAFETY: The static stack is aligned, reserved by the EFI image, and has
    // enough space for the early kernel call frame and firmware ABI shadow area.
    unsafe {
        asm!(
            "mov rsp, {stack_top}",
            "and rsp, -16",
            "xor rbp, rbp",
            "sub rsp, 32",
            "call {entry}",
            "ud2",
            stack_top = in(reg) stack_top(core::ptr::addr_of_mut!(KERNEL_STACK)),
            entry = in(reg) entry as usize,
            options(noreturn)
        );
    }
}

unsafe fn stack_top(stack: *mut CpuStack) -> u64 {
    // SAFETY: The supplied pointer names one of the static aligned stacks.
    unsafe { stack.cast::<u8>().add(STACK_SIZE) as u64 }
}

fn encode_tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let low = u64::from(limit & 0xffff)
        | ((base & 0x00ff_ffff) << 16)
        | (0x89_u64 << 40)
        | (u64::from((limit >> 16) & 0x0f) << 48)
        | (((base >> 24) & 0xff) << 56);
    (low, base >> 32)
}

/// Installs fatal early exception gates and leaves maskable interrupts off.
fn initialize_idt() {
    // SAFETY: Interrupts remain disabled while replacing IDTR. The table is in
    // static LoaderData memory owned by this EFI image.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
        let idt = core::ptr::addr_of_mut!(IDT).cast::<IdtGate>();
        let default = ousject_exception_default_stub as *const () as usize;
        for vector in 0..IDT_ENTRIES {
            idt.add(vector)
                .write(IdtGate::interrupt(default, KERNEL_CODE_SELECTOR, 0));
        }
        for (vector, handler) in EXCEPTION_STUBS.iter().copied().enumerate() {
            let ist = match vector {
                2 => NMI_IST,
                8 => DOUBLE_FAULT_IST,
                #[cfg(feature = "fault-smoke")]
                6 => SMOKE_IST,
                _ => 0,
            };
            idt.add(vector).write(IdtGate::interrupt(
                handler as usize,
                KERNEL_CODE_SELECTOR,
                ist,
            ));
        }
        idt.add(32).write(IdtGate::interrupt(
            super::timer::interrupt_stub_address(),
            KERNEL_CODE_SELECTOR,
            0,
        ));

        let pointer = DescriptorTablePointer {
            limit: u16::try_from(core::mem::size_of::<IdtGate>() * IDT_ENTRIES - 1)
                .expect("IDT limit fits its descriptor field"),
            base: idt as u64,
        };
        asm!(
            "lidt [{pointer}]",
            pointer = in(reg) core::ptr::addr_of!(pointer),
            options(nostack, preserves_flags)
        );
    }
}

/// Installs an owned GDT/TSS and maps IST1/IST2/IST3 to fault, NMI, and smoke
/// stacks. The TSS selector is read back to catch a failed `ltr` handoff.
pub fn verify_task_register() -> bool {
    let selector: u16;
    // SAFETY: STR is privileged but this EFI image remains at CPL0.
    unsafe {
        asm!(
            "str {selector:x}",
            selector = out(reg) selector,
            options(nomem, nostack, preserves_flags)
        );
    }
    if selector != TSS_SELECTOR {
        return false;
    }

    // SAFETY: The TSS and kernel stack are static Native-owned storage. The
    // TSS is packed, so read its stack pointers without assuming alignment.
    unsafe {
        let tss = core::ptr::addr_of!(TSS);
        let ring0_stack = core::ptr::addr_of!((*tss).rsp[0]).read_unaligned();
        let double_fault_stack = core::ptr::addr_of!((*tss).ist[0]).read_unaligned();
        ring0_stack == stack_top(core::ptr::addr_of_mut!(KERNEL_STACK))
            && double_fault_stack == stack_top(core::ptr::addr_of_mut!(DOUBLE_FAULT_STACK))
    }
}

/// Emits a minimal fatal diagnostic without allocation or firmware services.
#[unsafe(no_mangle)]
pub extern "efiapi" fn ousject_exception_handler(
    vector: u64,
    error_code: u64,
    instruction_pointer: u64,
) -> ! {
    let mut serial = super::com1::Com1::initialize();
    let _ = serial.output(b"Ousject native: fatal CPU exception vector 0x");
    write_hex(&mut serial, vector);
    let _ = serial.output(b" error=0x");
    write_hex(&mut serial, error_code);
    let _ = serial.output(b" rip=0x");
    write_hex(&mut serial, instruction_pointer);
    let _ = serial.output(b"\r\n");
    let _ = serial.output(b"Ousject native: fatal handler halt\r\n");
    super::enter_halt_loop()
}

fn write_hex(serial: &mut super::com1::Com1, value: u64) {
    let mut digits = [b'0'; 16];
    let mut remaining = value;
    for digit in digits.iter_mut().rev() {
        let nibble = (remaining & 0x0f) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        remaining >>= 4;
    }
    let _ = serial.output(&digits);
}

/// Triggers the invalid-opcode test path for the QEMU exception smoke.
#[cfg(feature = "fault-smoke")]
pub fn trigger_invalid_opcode() -> ! {
    // SAFETY: This deliberate #UD is confined to the explicit QEMU smoke build;
    // the installed IDT turns it into a fatal diagnostic and halt.
    unsafe { asm!("ud2", options(nomem, nostack)) };
    super::enter_halt_loop()
}

/// Corrupts two exception gates so a `#UD` delivery escalates to `#DF`.
#[cfg(feature = "double-fault-smoke")]
pub fn trigger_double_fault() -> ! {
    // SAFETY: This test only removes #UD and #GP gates. The #DF gate and its
    // dedicated TSS stack remain valid, so escalation must reach the fatal
    // handler rather than return to the corrupted table.
    unsafe {
        let idt = core::ptr::addr_of_mut!(IDT).cast::<IdtGate>();
        idt.add(6).write(IdtGate::MISSING);
        idt.add(13).write(IdtGate::MISSING);
        asm!("ud2", options(nomem, nostack));
    }
    super::enter_halt_loop()
}
