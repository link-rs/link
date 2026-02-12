//! Stack monitoring trait abstraction.
//!
//! This module provides a trait for stack usage monitoring, allowing
//! the mgmt and ui modules to work with any stack monitoring implementation
//! without depending directly on cortex-m-stack.

use core::ops::Range;

/// Trait for monitoring stack usage.
///
/// Implementations typically use platform-specific mechanisms to track
/// stack usage, such as the `cortex-m-stack` crate for ARM Cortex-M chips.
pub trait StackMonitor {
    /// Get the stack memory range.
    ///
    /// Returns a range where:
    /// - `start` is the top of the stack (highest address)
    /// - `end` is the base of the stack (lowest address)
    ///
    /// Note: For pointer-based ranges, convert to usize using `.start as usize`.
    fn stack(&self) -> Range<*mut u32>;

    /// Get the total stack size in bytes.
    fn stack_size(&self) -> u32;

    /// Get the amount of stack that has been "painted" (used).
    ///
    /// Stack painting fills unused stack with a known pattern. This
    /// returns how much of that pattern has been overwritten.
    fn stack_painted(&self) -> u32;

    /// Repaint the stack with the pattern for usage tracking.
    ///
    /// This should be called periodically to reset the usage tracking.
    fn repaint_stack(&self);
}

/// No-op stack monitor for platforms without stack monitoring support.
///
/// This is used in tests where actual stack monitoring is not needed.
#[cfg(test)]
pub struct NoOpStackMonitor;

#[cfg(test)]
impl StackMonitor for NoOpStackMonitor {
    fn stack(&self) -> Range<*mut u32> {
        core::ptr::null_mut()..core::ptr::null_mut()
    }

    fn stack_size(&self) -> u32 {
        0
    }

    fn stack_painted(&self) -> u32 {
        0
    }

    fn repaint_stack(&self) {
        // No-op
    }
}
