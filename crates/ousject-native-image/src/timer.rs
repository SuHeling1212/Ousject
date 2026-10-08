//! Early single-CPU PIT clock event on the legacy 8259 PIC route.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};
use ousject_platform::{MonotonicClock, ticks_to_nanos_ratio};

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xa0;
const SLAVE_DATA: u16 = 0xa1;
const PIT_CHANNEL_0: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;
// Rounded divisor for the PC PIT's nominal 1,193,182 Hz input clock.
const PIT_DIVISOR: u16 = 11_932;

#[unsafe(no_mangle)]
static OUSJECT_PIT_TICKS: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
    .globl ousject_timer_irq_stub
ousject_timer_irq_stub:
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    lock inc qword ptr [rip + OUSJECT_PIT_TICKS]
    mov al, 0x20
    out 0x20, al
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    iretq
    "#,
);

unsafe extern "efiapi" {
    fn ousject_timer_irq_stub() -> !;
}

/// Installs a 100 Hz PIT source on IRQ0 after the Native IDT is ready.
///
/// The legacy PIC is remapped to vectors 0x20-0x2f, all IRQs are masked except
/// IRQ0, and the PIT is programmed in rate-generator mode. Interrupt delivery
/// begins only after the handler and both controllers are configured.
pub fn initialize() {
    // SAFETY: This runs at CPL0 with a valid GDT/TSS/IDT and on the Native-owned
    // stack. Only the legacy PIC/PIT I/O ports are touched.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
        outb(MASTER_DATA, 0xff);
        outb(SLAVE_DATA, 0xff);
        outb(MASTER_COMMAND, 0x11);
        io_wait();
        outb(SLAVE_COMMAND, 0x11);
        io_wait();
        outb(MASTER_DATA, 0x20);
        io_wait();
        outb(SLAVE_DATA, 0x28);
        io_wait();
        outb(MASTER_DATA, 0x04);
        io_wait();
        outb(SLAVE_DATA, 0x02);
        io_wait();
        outb(MASTER_DATA, 0x01);
        io_wait();
        outb(SLAVE_DATA, 0x01);
        io_wait();
        outb(MASTER_DATA, 0xfe);
        outb(SLAVE_DATA, 0xff);

        outb(PIT_COMMAND, 0x34);
        let divisor_bytes = PIT_DIVISOR.to_le_bytes();
        outb(PIT_CHANNEL_0, divisor_bytes[0]);
        outb(PIT_CHANNEL_0, divisor_bytes[1]);
        asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

/// Returns the number of PIT IRQ0 ticks observed since boot.
#[must_use]
pub fn ticks() -> u64 {
    OUSJECT_PIT_TICKS.load(Ordering::Relaxed)
}

/// Waits until at least `minimum_ticks` more timer interrupts arrive.
pub fn wait_for_ticks(minimum_ticks: u64) -> bool {
    let start = ticks();
    for _ in 0..100_000_000 {
        if ticks().wrapping_sub(start) >= minimum_ticks {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// A boot-local 10 ms resolution clock driven by PIT interrupts.
#[derive(Clone, Copy, Debug)]
pub struct PitClock {
    start_ticks: u64,
}

impl PitClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            start_ticks: ticks(),
        }
    }
}

impl Default for PitClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for PitClock {
    fn now_nanos(&self) -> u64 {
        ticks_to_nanos_ratio(
            ticks().wrapping_sub(self.start_ticks),
            u64::from(PIT_DIVISOR),
            1_193_182,
        )
        .unwrap_or(u64::MAX)
    }
}

pub fn interrupt_stub_address() -> usize {
    ousject_timer_irq_stub as *const () as usize
}

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: The caller owns the early PC timer/controller programming phase.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn io_wait() {
    // SAFETY: Port 0x80 is the conventional legacy I/O delay port.
    unsafe { outb(0x80, 0) };
}
