//! Embassy async runtime abstraction layer
//!
//! This module provides async primitives using Embassy that work on both
//! std (desktop/server) and no_std (embedded) platforms.

use core::future::Future;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{with_timeout, Timer};

// Re-export Duration and time types
pub use embassy_time::Duration;
pub use embassy_time::Instant;
pub use embassy_time::TimeoutError;

// Arc depends on std vs no_std
#[cfg(feature = "std")]
pub use std::sync::Arc;
#[cfg(not(feature = "std"))]
pub use alloc::sync::Arc;

// ============================================================================
// Synchronization primitives
// ============================================================================

/// Async mutex for shared state
pub type Mutex<T> = embassy_sync::mutex::Mutex<CriticalSectionRawMutex, T>;

/// Mutex guard type
pub type MutexGuard<'a, T> = embassy_sync::mutex::MutexGuard<'a, CriticalSectionRawMutex, T>;

/// RwLock - Embassy doesn't have a dedicated RwLock, so we use Mutex
/// For read-heavy workloads on std, consider using parking_lot::RwLock
pub type RwLock<T> = Mutex<T>;

// ============================================================================
// Channel types
// ============================================================================

/// Channel for async message passing
pub type Channel<T, const N: usize> = embassy_sync::channel::Channel<CriticalSectionRawMutex, T, N>;

/// Sender half of a channel
pub type Sender<'a, T, const N: usize> = embassy_sync::channel::Sender<'a, CriticalSectionRawMutex, T, N>;

/// Receiver half of a channel
pub type Receiver<'a, T, const N: usize> = embassy_sync::channel::Receiver<'a, CriticalSectionRawMutex, T, N>;

/// Dynamic sender (for storing in structs without const generic)
pub type DynSender<'a, T> = embassy_sync::channel::DynamicSender<'a, T>;

/// Dynamic receiver (for storing in structs without const generic)
pub type DynReceiver<'a, T> = embassy_sync::channel::DynamicReceiver<'a, T>;

/// One-shot signal for notifications
pub type Signal<T> = embassy_sync::signal::Signal<CriticalSectionRawMutex, T>;

// ============================================================================
// PubSub types
// ============================================================================

/// Publish-subscribe channel
pub type PubSubChannel<T, const CAP: usize, const SUBS: usize, const PUBS: usize> =
    embassy_sync::pubsub::PubSubChannel<CriticalSectionRawMutex, T, CAP, SUBS, PUBS>;

/// Publisher for pubsub channel
pub type PubSubPublisher<'a, T, const CAP: usize, const SUBS: usize, const PUBS: usize> =
    embassy_sync::pubsub::Publisher<'a, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>;

/// Subscriber for pubsub channel
pub type PubSubSubscriber<'a, T, const CAP: usize, const SUBS: usize, const PUBS: usize> =
    embassy_sync::pubsub::Subscriber<'a, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>;

/// Watch channel for state that can be read by multiple consumers
pub type Watch<T, const N: usize> = embassy_sync::watch::Watch<CriticalSectionRawMutex, T, N>;

// ============================================================================
// Timer and timeout functions
// ============================================================================

/// Sleep for a duration
#[inline]
pub async fn sleep(duration: Duration) {
    Timer::after(duration).await;
}

/// Sleep for milliseconds
#[inline]
pub async fn sleep_ms(ms: u64) {
    Timer::after(Duration::from_millis(ms)).await;
}

/// Sleep for seconds
#[inline]
pub async fn sleep_secs(secs: u64) {
    Timer::after(Duration::from_secs(secs)).await;
}

/// Run a future with a timeout
#[inline]
pub async fn timeout<F, T>(duration: Duration, future: F) -> Result<T, TimeoutError>
where
    F: Future<Output = T>,
{
    with_timeout(duration, future).await
}

/// Run a future with a timeout in milliseconds
#[inline]
pub async fn timeout_ms<F, T>(ms: u64, future: F) -> Result<T, TimeoutError>
where
    F: Future<Output = T>,
{
    with_timeout(Duration::from_millis(ms), future).await
}

/// Run a future with a timeout in seconds
#[inline]
pub async fn timeout_secs<F, T>(secs: u64, future: F) -> Result<T, TimeoutError>
where
    F: Future<Output = T>,
{
    with_timeout(Duration::from_secs(secs), future).await
}

/// Yield to the executor
#[inline]
pub async fn yield_now() {
    embassy_futures::yield_now().await;
}

// ============================================================================
// Future combinators
// ============================================================================

/// Select the first future to complete
pub use embassy_futures::select::{select, select3, select4};

/// Join multiple futures
pub use embassy_futures::join::{join, join3, join4};

// ============================================================================
// Helper macros
// ============================================================================

/// Create a static channel. Usage:
/// ```ignore
/// static_channel!(MY_CHANNEL: Channel<MyType, 16>);
/// ```
#[macro_export]
macro_rules! static_channel {
    ($name:ident: Channel<$type:ty, $size:literal>) => {
        static $name: $crate::runtime::Channel<$type, $size> =
            embassy_sync::channel::Channel::new();
    };
}

/// Create a static signal. Usage:
/// ```ignore
/// static_signal!(MY_SIGNAL: Signal<()>);
/// ```
#[macro_export]
macro_rules! static_signal {
    ($name:ident: Signal<$type:ty>) => {
        static $name: $crate::runtime::Signal<$type> =
            embassy_sync::signal::Signal::new();
    };
}

/// Create a static mutex. Usage:
/// ```ignore
/// static_mutex!(MY_MUTEX: Mutex<MyType> = MyType::new());
/// ```
#[macro_export]
macro_rules! static_mutex {
    ($name:ident: Mutex<$type:ty> = $init:expr) => {
        static $name: $crate::runtime::Mutex<$type> =
            embassy_sync::mutex::Mutex::new($init);
    };
}

/// Create a static watch. Usage:
/// ```ignore
/// static_watch!(MY_WATCH: Watch<MyType, 4> = MyType::default());
/// ```
#[macro_export]
macro_rules! static_watch {
    ($name:ident: Watch<$type:ty, $receivers:literal> = $init:expr) => {
        static $name: $crate::runtime::Watch<$type, $receivers> =
            embassy_sync::watch::Watch::new();
    };
}
