# Native UEFI Stage A1 Notes

## Behavior reviewed

- UEFI 2.10 requires an OS loader to obtain the current memory map and pass its
  current map key to `ExitBootServices`. A changed map invalidates that key;
  descriptors must be walked using the returned descriptor size, not a fixed
  firmware descriptor size. After a failed first exit attempt, only the memory
  map and exit calls are permitted before retrying.
- UEFI Serial I/O is a byte-stream protocol for UART-style and other
  character-based devices. It is polled I/O and is only used during the boot
  services phase here. It is not the post-handoff native COM1 driver.
- Linux EFI stub behavior reviewed in `efi_exit_boot_services`: on an invalid
  map key, reuse the allocated buffer, refresh the map, and retry once. This
  behavior matches the UEFI constraint. Ousject adopts the recovery behavior,
  not Linux's stub interfaces, setup-data layout, or boot architecture.
- Rust `uefi` provides the EFI entry macro and wrappers for UEFI protocols and
  memory maps. The cached current release requires Rust 1.91; this repository
  uses `uefi` 0.35.0, whose registry metadata lists Rust 1.81 as its MSRV, so it
  fits the workspace's Rust 1.85 floor. The image compiled with the local Rust
  1.99 toolchain and the UEFI target's `core` built from source.
- The `uefi` crate's optional panic helper prints through boot-services stdout
  and is documented for the pre-ExitBootServices phase. Ousject disables that
  helper and supplies a no-allocation COM1 panic handler.
- The installed QEMU firmware descriptor pairs a read-only x86_64 EDK2 code
  image with a writable NVRAM variables image. The run script makes a private
  copy of the variables image and boots a FAT ESP directory on a Q35 machine.

## Native COM1 behavior reviewed

- TI TL16C550C specifies the 16550-compatible register aliases, DLAB divisor
  window, FIFO controls, 8-bit line format, and LSR transmit-ready/empty bits.
- QEMU's 16550A model uses matching DLAB, THRE, TEMT, and FIFO bit values; its
  transmit path clears THRE/TEMT on a data write and sets them as the FIFO
  drains.
- Linux's 8250 port driver probes chip variants and handles their quirks.
  Ousject uses no Linux code or driver framework; it implements the common
  legacy PC COM1 register sequence and bounded polling directly from UART
  behavior. The QEMU path is 115200 8N1, interrupts disabled, FIFOs enabled
  and cleared, and bounded waits for THRE/TEMT.

## Ousject translation

The EFI image uses the Rust UEFI entry point, reports the firmware phase over
the discovered UEFI Serial I/O protocol, calls the UEFI crate's memory-map and
`ExitBootServices` wrapper, conservatively marks only conventional memory as
usable, and forms the shared `ousject-platform::BootInfo` value. It does not
claim those regions for a general allocator: the current bootstrap selector can
only return monotonically selected 4 KiB frame addresses. Physical-address
width remains unknown until native CPU detection is implemented.

Unsafe operations are limited to the `ExitBootServices` handoff, the documented
x86 port-I/O wrappers, and the freestanding `wcslen` ABI shim. The image drops
firmware protocol handles before handoff and uses no boot service afterward.
No Linux code was copied. Linux source reviewed is GPL-2.0-only, so it is used
only as a behavioral reference.

Before ExitBootServices, firmware Serial I/O reports the entry and transition.
Afterward, the Native image initializes COM1 itself and writes a marker through
the shared `TerminalTransport` contract. This proves the transport boundary,
not yet a `core.terminal` Provider or an interactive Terminal. Polling is
bounded and returns `DeviceFailure`.

## Early exception handling

- Intel's x86_64 IDT gates are 16 bytes, point at a 64-bit code segment, and
  identify handlers by vector. The CPU places an exception frame on the active
  stack; selected exceptions also push an error code.
- QEMU's x86 TCG exception delivery follows the gate and stack-frame rules.
  The smoke image executes `UD2` after loading its table, then checks the
  resulting vector, zero error code, and saved RIP on COM1.
- Linux's x86 `idt.c` and `traps.c` demonstrate per-vector entry stubs,
  error-code differences, and a separate recovery policy. These files are
  GPL-2.0-only; no code or Linux entry ABI was copied.
- Ousject installs a private flat long-mode GDT, a 64-bit TSS, and a 256-entry
  fatal IDT after disabling maskable interrupts. It reloads CS/data selectors,
  executes `LTR`, and reads back the task register. TSS `RSP0` and the current
  execution stack point to a statically reserved 16 KiB Native stack. NMI and
  double fault use separate 16 KiB IST stacks. Exception stubs normalize the
  stack shape and pass vector, error-code slot, and saved instruction pointer
  to a non-returning diagnostic. The image remains at CPL0; no user transition
  or stack reclamation exists yet.
- QEMU `fault-smoke` verifies `#UD`; `double-fault-smoke` removes the `#UD` and
  `#GP` gates, triggers `#UD`, and verifies escalation to vector 8 with a valid
  saved RIP. `panic-smoke` checks the native panic formatter. These paths halt
  intentionally and do not imply exception recovery.
- The Native panic handler formats `PanicInfo` through a small `core::fmt::Write`
  adapter over `TerminalTransport`, then halts. The QEMU `panic-smoke` feature
  injects a panic after IDT setup and checks the location, message, and halt
  marker. This is diagnostic handling only; there is no unwind or recovery.

## Verification state

### Monotonic clock capability

The optional native TSC clock uses ordered `RDTSC` reads only if
CPUID reports both an invariant TSC and the architectural `0x15` frequency
ratio. It returns unavailable when either guarantee is missing, avoiding a
fabricated clock rate. The QEMU profile used here does not expose that full
combination, so this boot reports the TSC clock unavailable. The active
`MonotonicClock` is instead driven by the legacy PIT interrupt described below.

Intel's SDM defines the `0x15` ratio as `ECX * EBX / EAX`. QEMU's x86 CPU
model controls which CPUID features the guest sees. Linux also checks the
frequency ratio, but adds PIT/HPET/ACPI PM timer calibration and CPU-specific
fallbacks; Ousject has not adopted those device mechanisms yet. Linux's TSC
code is GPL-2.0-only and was read for behavior only; no code was copied.

### Legacy timer interrupt

Intel's PC chipset documentation specifies the PIT's three I/O counters and
nominal 1,193,182 Hz input, with counter 0 conventionally driving IRQ0. Ousject
programs counter 0 in mode 2 with divisor 11,932 (about 100 Hz), remaps the
master/slave 8259 PIC vectors to `0x20`/`0x28`, masks all but master IRQ0, and
sends a non-specific EOI from the assembly interrupt stub. The stub preserves
all general-purpose registers before returning with `IRETQ`. PIT ticks become
a boot-local monotonic clock with their rational period retained in the
nanosecond conversion; there is no scheduler integration yet.

QEMU's i8254 and i8259 models deliver the periodic IRQ in the Q35 test machine.
The smoke boot waits for two interrupts and checks the clock advances by at
least 20 ms. Linux's 8259/PIT code was reviewed for initialization, masking,
EOI, and periodic mode behavior; both files are GPL-2.0-only and no code was
copied. The implementation is Ousject's small single-CPU bootstrap route; it
does not adopt Linux IRQ abstractions. Other timer routes, including HPET,
local APIC, IOAPIC enumeration, lost-tick recovery, and multi-CPU clock
synchronization, remain future work.

### Bootstrap page-table ownership

Intel's long-mode paging rules allow 2 MiB pages at the page-directory level;
the mapping structures are reached through CR3, and large pages require the
appropriate control-register features. QEMU's x86 page walker checks the
present, writable, large-page, and cache-control bits in these entries. Linux's
early x86 paging code was reviewed for its use of identity/direct mappings and
large pages, but none of its page-table layout or code was copied.

Ousject now installs its own four-level tables for the first 4 GiB, using four
page directories with 512 identity-mapped 2 MiB entries each. The bootstrap
image, GDT/TSS/IDT, current stack, and first selected frame must all fit below
4 GiB. A 2 MiB entry intersecting a UEFI MMIO descriptor is marked uncached;
the first 2 MiB is also uncached to cover legacy low-memory device ranges. The
current map is supervisor read/write and executable everywhere in that window,
and it maps physical holes too. It is a temporary single-address-space map,
not process isolation or a final memory policy. Fine-grained 4 KiB permissions,
dynamic mapping, reclamation, and physical addresses above 4 GiB remain missing.

The QEMU boot proves that execution, COM1 diagnostics, the owned stack, and
the PIT interrupt continue after CR3 is replaced. The test machine has 512 MiB
RAM, so it does not exercise addresses above the current 4 GiB boundary or
hardware-specific MMIO cache attributes.

The EFI image compiled for `x86_64-unknown-uefi` using the local Rust 1.99
toolchain's `build-std=core,compiler_builtins` path and `lld-link`; the default
developer script still uses the ordinary installed-target Cargo path. QEMU
11.1 and x86_64 EDK2 firmware booted the image, and `scripts/check-native-boot`
waits for the post-ExitBootServices COM1 marker before passing. The image then
uses the shared bootstrap frame allocator to select a usable 4 KiB frame,
passes the CPUID-reported physical-address width when available, installs its
GDT/TSS/IDT, replaces the firmware CR3 with Ousject's 4 GiB identity map, moves
execution to its own kernel stack, initializes PIT/8259, and halts. The
allocator does not dereference or reclaim frames. There is still no dynamic
page-table manager, heap, OMS/VM, Native Terminal Provider, or shell.

Run the exception and panic smoke paths with:

```sh
NATIVE_FAULT_SMOKE=1 ./scripts/check-native-boot
NATIVE_DOUBLE_FAULT_SMOKE=1 ./scripts/check-native-boot
NATIVE_PANIC_SMOKE=1 ./scripts/check-native-boot
```

References:

- [UEFI Specification 2.10: Boot Services](https://uefi.org/specs/UEFI/2.10/07_Services_Boot_Services.html)
- [UEFI Specification 2.10: Serial I/O Protocol](https://uefi.org/specs/UEFI/2.10/12_Protocols_Console_Support.html#serial-i-o-protocol)
- [Rust UEFI crate documentation](https://docs.rs/uefi/latest/uefi/)
- [Linux EFI stub documentation](https://docs.kernel.org/admin-guide/efi-stub.html)
- [Linux EFI stub memory-map/exit helper](https://github.com/torvalds/linux/blob/master/drivers/firmware/efi/libstub/efi-stub-helper.c)
- [QEMU x86 system emulator documentation](https://www.qemu.org/docs/master/system/target-i386.html)
- [Intel SDM Volume 3A: Interrupt and Exception Handling](https://cdrdv2-public.intel.com/812386/253668-sdm-vol-3a.pdf)
- [Intel SDM combined volumes: CPUID leaf 0x15 TSC frequency](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf)
- [QEMU x86 TCG exception delivery](https://github.com/qemu/qemu/blob/master/target/i386/tcg/excp_helper.c)
- [Linux x86 IDT setup](https://github.com/torvalds/linux/blob/master/arch/x86/kernel/idt.c)
- [Linux x86 trap handling](https://github.com/torvalds/linux/blob/master/arch/x86/kernel/traps.c)
- [Linux x86 TSC calibration](https://github.com/torvalds/linux/blob/master/arch/x86/kernel/tsc.c)
- [QEMU x86 CPU model and CPUID](https://github.com/qemu/qemu/blob/master/target/i386/cpu.c)
- [TI TL16C550C datasheet](https://www.ti.com/lit/ds/symlink/tl16c550c.pdf)
- [QEMU 16550A serial model](https://github.com/qemu/qemu/blob/master/hw/char/serial.c)
- [Linux 8250 serial port driver](https://github.com/torvalds/linux/blob/master/drivers/tty/serial/8250/8250_port.c)
- [Intel Atom C2000 datasheet, integrated 8254 PIT](https://cdrdv2-public.intel.com/330061/atom-c2000-microserver-datasheet.pdf)
- [Intel 8259A PIC datasheet](https://www.pcjs.org/documents/datasheets/intel/INTEL_8259A_PIC.pdf)
- [QEMU i8254 model](https://qemu.googlesource.com/qemu/+/refs/tags/v9.2.0-rc0/hw/timer/i8254.c)
- [QEMU i8259 model](https://github.com/qemu/qemu/blob/master/hw/intc/i8259.c)
- [Linux x86 i8259 setup](https://github.com/torvalds/linux/blob/master/arch/x86/kernel/i8259.c)
- [Linux i8253 clock event](https://github.com/torvalds/linux/blob/master/drivers/clocksource/i8253.c)
- [Intel SDM Volume 3: Paging and 2 MiB pages](https://cdrdv2-public.intel.com/782157/325384-sdm-vol-3abcd.pdf)
- [QEMU x86 page-table walk](https://github.com/qemu/qemu/blob/master/target/i386/tcg/sysemu/excp_helper.c)
- [Linux x86 early paging](https://github.com/torvalds/linux/blob/master/arch/x86/kernel/head64.c)
