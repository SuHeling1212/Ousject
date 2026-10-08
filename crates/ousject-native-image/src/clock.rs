//! Native monotonic time backed by an invariant, frequency-described TSC.

use core::arch::asm;
use ousject_platform::{MonotonicClock, ticks_to_nanos};

/// A boot-local clock using the x86 invariant TSC.
#[derive(Clone, Copy, Debug)]
pub struct TscClock {
    start_ticks: u64,
    ticks_per_second: u64,
}

impl TscClock {
    /// Builds a clock only when CPUID advertises an invariant TSC and a usable
    /// architectural frequency ratio.
    #[must_use]
    pub fn initialize() -> Option<Self> {
        let basic = core::arch::x86_64::__cpuid(0);
        if basic.eax < 0x15 {
            return None;
        }

        let extended = core::arch::x86_64::__cpuid(0x8000_0000);
        if extended.eax < 0x8000_0007 {
            return None;
        }
        let invariant = core::arch::x86_64::__cpuid(0x8000_0007).edx & (1 << 8) != 0;
        if !invariant {
            return None;
        }

        let ratio = core::arch::x86_64::__cpuid(0x15);
        if ratio.eax == 0 || ratio.ebx == 0 || ratio.ecx == 0 {
            return None;
        }
        let frequency = u64::from(ratio.ecx)
            .checked_mul(u64::from(ratio.ebx))?
            .checked_div(u64::from(ratio.eax))?;
        if frequency == 0 {
            return None;
        }

        Some(Self {
            start_ticks: read_tsc(),
            ticks_per_second: frequency,
        })
    }
}

impl MonotonicClock for TscClock {
    fn now_nanos(&self) -> u64 {
        let elapsed_ticks = read_tsc().wrapping_sub(self.start_ticks);
        ticks_to_nanos(elapsed_ticks, self.ticks_per_second).unwrap_or(u64::MAX)
    }
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: LFENCE orders prior execution before RDTSC. TSC is a user-visible
    // read-only architectural counter on x86_64; interrupts remain disabled.
    unsafe {
        asm!(
            "lfence",
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}
