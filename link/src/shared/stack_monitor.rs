//! Stack info trait abstraction.
//!
//! Provides the base trait for stack monitoring, extended by
//! chip-specific Board traits in mgmt and ui modules.

use core::ops::Range;

/// Trait for stack monitoring operations.
///
/// Base trait extended by chip-specific Board traits.
pub trait StackMonitor {
    /// Get the stack memory range.
    ///
    /// Returns a range where:
    /// - `start` is the top of the stack (highest address)
    /// - `end` is the base of the stack (lowest address)
    fn stack(&self) -> Range<*mut u32>;

    /// Get the total stack size in bytes.
    fn stack_size(&self) -> u32;

    /// Get the amount of stack that has been "painted" (used).
    ///
    /// Stack painting fills unused stack with a known pattern. This
    /// returns how much of that pattern remains unpainted.
    fn stack_painted(&self) -> u32;

    /// Repaint the stack with the pattern for usage tracking.
    fn repaint_stack(&self);
}

/// No-op implementation for tests.
#[cfg(test)]
pub struct NoOpBoard;

#[cfg(test)]
impl StackMonitor for NoOpBoard {
    fn stack(&self) -> Range<*mut u32> {
        core::ptr::null_mut()..core::ptr::null_mut()
    }

    fn stack_size(&self) -> u32 {
        0
    }

    fn stack_painted(&self) -> u32 {
        0
    }

    fn repaint_stack(&self) {}
}
