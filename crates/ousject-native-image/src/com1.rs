//! Minimal polled 16550-compatible COM1 transport for the post-UEFI phase.

use core::arch::asm;
use ousject_platform::{PlatformError, TerminalTransport};

const COM1_BASE: u16 = 0x3f8;
const DATA: u16 = COM1_BASE;
const INTERRUPT_ENABLE: u16 = COM1_BASE + 1;
const FIFO_CONTROL: u16 = COM1_BASE + 2;
const LINE_CONTROL: u16 = COM1_BASE + 3;
const MODEM_CONTROL: u16 = COM1_BASE + 4;
const LINE_STATUS: u16 = COM1_BASE + 5;

const DLAB: u8 = 0x80;
const EIGHT_BITS_ONE_STOP_NO_PARITY: u8 = 0x03;
const FIFO_ENABLE_CLEAR_RX_TX_TRIGGER_14: u8 = 0xc7;
const DTR_RTS_OUT2: u8 = 0x0b;
const DATA_READY: u8 = 0x01;
const TRANSMITTER_HOLDING_EMPTY: u8 = 0x20;
const TRANSMITTER_EMPTY: u8 = 0x40;
const POLL_LIMIT: usize = 1_000_000;

/// Legacy PC COM1, initialized for 115200 baud, 8 data bits, no parity, 1 stop.
pub struct Com1;

impl Com1 {
    /// Initializes the conventional 1.8432 MHz 16550-compatible UART.
    #[must_use]
    pub fn initialize() -> Self {
        // SAFETY: The x86_64 UEFI handoff runs at firmware privilege level and
        // this image targets the conventional legacy COM1 I/O port range.
        unsafe {
            write_port(INTERRUPT_ENABLE, 0);
            write_port(LINE_CONTROL, DLAB);
            write_port(DATA, 1); // 1.8432 MHz / (16 * 1) = 115200 baud.
            write_port(INTERRUPT_ENABLE, 0);
            write_port(LINE_CONTROL, EIGHT_BITS_ONE_STOP_NO_PARITY);
            write_port(FIFO_CONTROL, FIFO_ENABLE_CLEAR_RX_TX_TRIGGER_14);
            write_port(MODEM_CONTROL, DTR_RTS_OUT2);
        }
        Self
    }
}

impl TerminalTransport for Com1 {
    fn input(&mut self, output: &mut [u8]) -> Result<usize, PlatformError> {
        let mut count = 0;
        while count < output.len() {
            // SAFETY: Reads the 16550 line-status and receiver-buffer ports.
            let status = unsafe { read_port(LINE_STATUS) };
            if status & DATA_READY == 0 {
                break;
            }
            // SAFETY: LSR reported that at least one received byte is ready.
            output[count] = unsafe { read_port(DATA) };
            count += 1;
        }
        Ok(count)
    }

    fn output(&mut self, bytes: &[u8]) -> Result<(), PlatformError> {
        for &byte in bytes {
            let mut ready = false;
            for _ in 0..POLL_LIMIT {
                // SAFETY: Reads the 16550 line-status register.
                if unsafe { read_port(LINE_STATUS) } & TRANSMITTER_HOLDING_EMPTY != 0 {
                    ready = true;
                    break;
                }
                core::hint::spin_loop();
            }
            if !ready {
                return Err(PlatformError::DeviceFailure);
            }
            // SAFETY: THRE indicates the transmitter can accept another byte.
            unsafe { write_port(DATA, byte) };
        }

        let mut sent = false;
        for _ in 0..POLL_LIMIT {
            // Wait for both the holding and shift registers to drain.
            // SAFETY: Reads the 16550 line-status register.
            if unsafe { read_port(LINE_STATUS) } & TRANSMITTER_EMPTY != 0 {
                sent = true;
                break;
            }
            core::hint::spin_loop();
        }
        if sent {
            Ok(())
        } else {
            Err(PlatformError::DeviceFailure)
        }
    }
}

/// Reads one byte from an x86 port-mapped I/O address.
///
/// # Safety
/// The caller must ensure the port is valid and the current privilege level
/// permits port I/O.
unsafe fn read_port(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Preconditions are delegated to the caller.
    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Writes one byte to an x86 port-mapped I/O address.
///
/// # Safety
/// The caller must ensure the port is valid and the current privilege level
/// permits port I/O.
unsafe fn write_port(port: u16, value: u8) {
    // SAFETY: Preconditions are delegated to the caller.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}
