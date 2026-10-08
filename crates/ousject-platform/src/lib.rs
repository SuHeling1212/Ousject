#![no_std]
//! Small, `no_std` contracts for mechanisms supplied by a boot platform.
//!
//! This crate deliberately contains no OMS, VM, scheduler, or device policy.
//! Hosted adapters and native transports can implement these interfaces while
//! Ousject keeps one Object model and one persistent world.

/// One region reported by firmware or a platform memory map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryRegion {
    pub start: u64,
    pub length: u64,
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    /// Returns the exclusive end address, or `None` if the range overflows.
    #[must_use]
    pub const fn end(self) -> Option<u64> {
        self.start.checked_add(self.length)
    }
}

/// Platform-independent classification of memory known at boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryRegionKind {
    Usable,
    Reserved,
    Firmware,
    Mmio,
}

/// Bootstrap allocator that selects 4 KiB physical frames from usable regions.
///
/// This allocator is monotonic and does not reclaim frames. The caller remains
/// responsible for reserving frames after selection and for mapping them
/// before dereferencing their physical addresses.
#[derive(Clone, Copy, Debug, Default)]
pub struct PhysicalFrameAllocator {
    next_address: u64,
}

impl PhysicalFrameAllocator {
    /// Creates an allocator at the beginning of the supplied memory map.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            // Keep the null page outside the allocator even if a malformed or
            // synthetic memory map incorrectly labels it as usable.
            next_address: 4096,
        }
    }

    /// Selects the next 4 KiB-aligned physical frame, if one is available.
    ///
    /// The returned address is a physical address, not a dereferenceable
    /// pointer. Only regions classified as [`MemoryRegionKind::Usable`] are
    /// considered.
    pub fn allocate_frame(&mut self, regions: &[MemoryRegion]) -> Option<u64> {
        let mut lowest_candidate = None;
        for region in regions {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }
            let Some(region_end) = region.end() else {
                continue;
            };

            let candidate = region.start.max(self.next_address);
            let Some(aligned) = candidate.checked_add(4095).map(|value| value & !4095) else {
                continue;
            };
            let Some(frame_end) = aligned.checked_add(4096) else {
                continue;
            };
            if frame_end > region_end {
                continue;
            }
            lowest_candidate =
                Some(lowest_candidate.map_or(aligned, |lowest: u64| lowest.min(aligned)));
        }

        let frame = lowest_candidate?;
        self.next_address = frame.checked_add(4096)?;
        Some(frame)
    }
}

/// Immutable information collected before the kernel takes ownership.
#[derive(Clone, Copy, Debug)]
pub struct BootInfo<'a> {
    pub memory_map: &'a [MemoryRegion],
    pub physical_address_bits: Option<u8>,
}

/// A source of monotonic time. Values must not move backwards during a boot.
pub trait MonotonicClock {
    /// Returns elapsed nanoseconds from an unspecified boot-local origin.
    fn now_nanos(&self) -> u64;
}

/// Converts elapsed hardware ticks to nanoseconds without overflowing a
/// 64-bit intermediate. Returns `None` when the frequency is zero.
#[must_use]
pub fn ticks_to_nanos(elapsed_ticks: u64, ticks_per_second: u64) -> Option<u64> {
    if ticks_per_second == 0 {
        return None;
    }
    ticks_to_nanos_ratio(elapsed_ticks, 1, ticks_per_second)
}

/// Converts ticks where one tick spans `period_numerator / period_denominator`
/// seconds. Returns `None` when either part of the period is zero.
#[must_use]
pub fn ticks_to_nanos_ratio(
    elapsed_ticks: u64,
    period_numerator: u64,
    period_denominator: u64,
) -> Option<u64> {
    if period_numerator == 0 || period_denominator == 0 {
        return None;
    }
    let scaled_ticks = u128::from(elapsed_ticks).checked_mul(u128::from(period_numerator))?;
    let period_denominator = u128::from(period_denominator);
    let whole_seconds = scaled_ticks / period_denominator;
    let remainder = scaled_ticks % period_denominator;
    let nanos_per_second = 1_000_000_000_u128;
    if whole_seconds > u128::from(u64::MAX) / nanos_per_second {
        return Some(u64::MAX);
    }
    let nanos =
        whole_seconds * nanos_per_second + remainder * nanos_per_second / period_denominator;
    Some(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// A platform source capable of supplying bytes for security-sensitive use.
pub trait EntropySource {
    /// Fills the buffer with fresh entropy, or reports that it is unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Unavailable`] when no trusted source is ready.
    fn fill(&mut self, output: &mut [u8]) -> Result<(), PlatformError>;
}

/// Byte transport used by a Terminal provider, such as a UART.
pub trait TerminalTransport {
    /// Reads up to `input.len()` bytes. `Ok(0)` means no bytes are available.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the transport fails.
    fn input(&mut self, output: &mut [u8]) -> Result<usize, PlatformError>;

    /// Writes all bytes or returns an error.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the transport cannot write all bytes.
    fn output(&mut self, bytes: &[u8]) -> Result<(), PlatformError>;
}

/// Raw block transport. Durability is acknowledged separately by `flush`.
pub trait BlockTransport {
    /// Number of addressable blocks on this device.
    fn block_count(&self) -> u64;

    /// Number of bytes in one addressable block.
    fn block_size(&self) -> u32;

    /// Reads complete blocks starting at `first_block`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::OutOfRange`] or [`PlatformError::DeviceFailure`].
    fn read_blocks(&mut self, first_block: u64, output: &mut [u8]) -> Result<(), PlatformError>;

    /// Writes complete blocks starting at `first_block`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::OutOfRange`] or [`PlatformError::DeviceFailure`].
    fn write_blocks(&mut self, first_block: u64, input: &[u8]) -> Result<(), PlatformError>;

    /// Confirms all earlier writes reached the transport's durable boundary.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::DeviceFailure`] if the durability boundary
    /// cannot be confirmed.
    fn flush(&mut self) -> Result<(), PlatformError>;
}

/// Machine control operations provided by firmware or a virtual platform.
pub trait SystemControl {
    /// # Errors
    ///
    /// Returns [`PlatformError::Unavailable`] if firmware does not support
    /// machine shutdown.
    fn shutdown(&mut self) -> Result<(), PlatformError>;

    /// # Errors
    ///
    /// Returns [`PlatformError::Unavailable`] if firmware does not support
    /// machine reboot.
    fn reboot(&mut self) -> Result<(), PlatformError>;
}

/// Errors reported by low-level platform mechanisms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformError {
    Unavailable,
    InvalidBuffer,
    OutOfRange,
    DeviceFailure,
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryRegion, MemoryRegionKind, PhysicalFrameAllocator, ticks_to_nanos,
        ticks_to_nanos_ratio,
    };

    #[test]
    fn memory_region_end_detects_overflow() {
        let region = MemoryRegion {
            start: u64::MAX - 3,
            length: 4,
            kind: MemoryRegionKind::Usable,
        };
        assert_eq!(region.end(), None);
    }

    #[test]
    fn frame_allocator_aligns_and_skips_reserved_regions() {
        let regions = [
            MemoryRegion {
                start: 0x1003,
                length: 0x3000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x8000,
                length: 0x4000,
                kind: MemoryRegionKind::Reserved,
            },
            MemoryRegion {
                start: 0x10000,
                length: 0x2000,
                kind: MemoryRegionKind::Usable,
            },
        ];
        let mut allocator = PhysicalFrameAllocator::new();

        assert_eq!(allocator.allocate_frame(&regions), Some(0x2000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x3000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x10000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x11000));
        assert_eq!(allocator.allocate_frame(&regions), None);
    }

    #[test]
    fn frame_allocator_skips_ranges_that_cannot_form_a_frame() {
        let regions = [
            MemoryRegion {
                start: u64::MAX - 1023,
                length: 1024,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x4000,
                length: 4096,
                kind: MemoryRegionKind::Usable,
            },
        ];
        let mut allocator = PhysicalFrameAllocator::new();

        assert_eq!(allocator.allocate_frame(&regions), Some(0x4000));
        assert_eq!(allocator.allocate_frame(&regions), None);
    }

    #[test]
    fn frame_allocator_does_not_depend_on_map_order_or_duplicate_overlaps() {
        let regions = [
            MemoryRegion {
                start: 0x5000,
                length: 0x4000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x2000,
                length: 0x5000,
                kind: MemoryRegionKind::Usable,
            },
        ];
        let mut allocator = PhysicalFrameAllocator::new();

        assert_eq!(allocator.allocate_frame(&regions), Some(0x2000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x3000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x4000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x5000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x6000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x7000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x8000));
        assert_eq!(allocator.allocate_frame(&regions), None);
    }

    #[test]
    fn frame_allocator_never_returns_the_null_page() {
        let regions = [MemoryRegion {
            start: 0,
            length: 0x3000,
            kind: MemoryRegionKind::Usable,
        }];
        let mut allocator = PhysicalFrameAllocator::new();

        assert_eq!(allocator.allocate_frame(&regions), Some(0x1000));
        assert_eq!(allocator.allocate_frame(&regions), Some(0x2000));
        assert_eq!(allocator.allocate_frame(&regions), None);
    }

    #[test]
    fn tick_conversion_uses_wide_math_and_rejects_unknown_frequency() {
        assert_eq!(ticks_to_nanos(2_500_000, 2_500_000), Some(1_000_000_000));
        assert_eq!(ticks_to_nanos(u64::MAX, 1), Some(u64::MAX));
        assert_eq!(ticks_to_nanos(1, 0), None);
    }

    #[test]
    fn rational_tick_conversion_preserves_fractional_periods() {
        let expected = u64::try_from(100_u128 * 11_932 * 1_000_000_000 / 1_193_182).unwrap();
        assert_eq!(ticks_to_nanos_ratio(100, 11_932, 1_193_182), Some(expected));
        assert_eq!(ticks_to_nanos_ratio(1, 0, 1), None);
        assert_eq!(ticks_to_nanos_ratio(1, 1, 0), None);
        assert_eq!(ticks_to_nanos_ratio(u64::MAX, u64::MAX, 1), Some(u64::MAX));
    }
}
