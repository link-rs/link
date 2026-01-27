//! Subscribe track functionality using Embassy async runtime

use crate::error::{Error, Result};
use crate::ffi;
use crate::object::{FilterType, GroupOrder, ObjectHeaders, ReceivedObject};
use crate::runtime::{DynReceiver, DynSender, Signal, TryReceiveError};
use crate::track::FullTrackName;
use bytes::Bytes;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};
use heapless::Vec as HeaplessVec;

// Conditional logging
#[cfg(all(feature = "defmt-logging", not(feature = "std")))]
use defmt::{debug, error, info, trace, warn};
#[cfg(all(not(feature = "defmt-logging"), feature = "std"))]
use log::{debug, error, info, trace, warn};
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! debug {
    ($($arg:tt)*) => {};
}
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! error {
    ($($arg:tt)*) => {};
}
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! info {
    ($($arg:tt)*) => {};
}
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! warn {
    ($($arg:tt)*) => {};
}

#[cfg(not(feature = "std"))]
use crate::ffi::c_void;
#[cfg(feature = "std")]
use std::ffi::c_void;

#[cfg(not(feature = "std"))]
use core::ffi::{c_char, CStr};
#[cfg(feature = "std")]
use std::ffi::{c_char, CStr};

#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::string::String;

/// Status of a subscribe track
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt-logging", derive(defmt::Format))]
pub enum SubscribeStatus {
    /// Subscription is active
    Ok,
    /// Not connected to relay
    NotConnected,
    /// Error occurred
    Error,
    /// Not authorized
    NotAuthorized,
    /// Not subscribed
    NotSubscribed,
    /// Waiting for response
    PendingResponse,
    /// Sending unsubscribe
    SendingUnsubscribe,
    /// Subscription paused
    Paused,
    /// New group requested
    NewGroupRequested,
    /// Subscription cancelled
    Cancelled,
    /// Done by FIN
    DoneByFin,
    /// Done by RESET
    DoneByReset,
}

impl From<ffi::QuicrSubscribeStatus> for SubscribeStatus {
    fn from(status: ffi::QuicrSubscribeStatus) -> Self {
        match status {
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_OK => SubscribeStatus::Ok,
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_CONNECTED => {
                SubscribeStatus::NotConnected
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_ERROR => SubscribeStatus::Error,
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_AUTHORIZED => {
                SubscribeStatus::NotAuthorized
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED => {
                SubscribeStatus::NotSubscribed
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_PENDING_RESPONSE => {
                SubscribeStatus::PendingResponse
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_SENDING_UNSUBSCRIBE => {
                SubscribeStatus::SendingUnsubscribe
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_PAUSED => SubscribeStatus::Paused,
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NEW_GROUP_REQUESTED => {
                SubscribeStatus::NewGroupRequested
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_CANCELLED => {
                SubscribeStatus::Cancelled
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_DONE_BY_FIN => {
                SubscribeStatus::DoneByFin
            }
            ffi::QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_DONE_BY_RESET => {
                SubscribeStatus::DoneByReset
            }
            _ => SubscribeStatus::NotSubscribed,
        }
    }
}

impl SubscribeStatus {
    /// Check if the subscription is active
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            SubscribeStatus::Ok | SubscribeStatus::NewGroupRequested
        )
    }

    /// Check if the subscription has ended
    pub fn is_done(&self) -> bool {
        matches!(
            self,
            SubscribeStatus::Cancelled
                | SubscribeStatus::DoneByFin
                | SubscribeStatus::DoneByReset
                | SubscribeStatus::Error
                | SubscribeStatus::NotAuthorized
        )
    }
}

/// Callback data for subscribe track
///
/// This struct stores the signal and metadata for subscribe track callbacks.
/// For no_std builds, users must provide static storage for this data.
pub struct SubscribeCallbackData {
    /// Signal for subscribe status changes
    pub status_signal: &'static Signal<SubscribeStatus>,
    /// Sender for received objects
    pub object_sender: DynSender<'static, ReceivedObject>,
    /// Track name for logging (std version)
    #[cfg(feature = "std")]
    pub track_name: String,
    /// Track name for logging (no_std version)
    #[cfg(not(feature = "std"))]
    pub track_name: heapless::String<64>,
}

impl SubscribeCallbackData {
    /// Create new subscribe callback data
    #[cfg(feature = "std")]
    pub fn new(
        status_signal: &'static Signal<SubscribeStatus>,
        object_sender: DynSender<'static, ReceivedObject>,
        track_name: impl Into<String>,
    ) -> Self {
        Self {
            status_signal,
            object_sender,
            track_name: track_name.into(),
        }
    }

    /// Create new subscribe callback data (no_std version)
    #[cfg(not(feature = "std"))]
    pub fn new(
        status_signal: &'static Signal<SubscribeStatus>,
        object_sender: DynSender<'static, ReceivedObject>,
        track_name: &str,
    ) -> Self {
        let mut name = heapless::String::new();
        let _ = name.push_str(track_name);
        Self {
            status_signal,
            object_sender,
            track_name: name,
        }
    }
}

/// Handle to a subscribe track
pub struct SubscribeTrack {
    handler: NonNull<ffi::QuicrSubscribeTrackHandler>,
    track_name: FullTrackName,
    #[allow(dead_code)]
    callback_data: &'static SubscribeCallbackData,
    #[allow(dead_code)]
    ffi_entries: HeaplessVec<ffi::QuicrBytes, 8>,
    status_signal: &'static Signal<SubscribeStatus>,
    object_receiver: DynReceiver<'static, ReceivedObject>,
    is_registered: AtomicBool,
}

// SAFETY: The underlying C++ object is thread-safe with mutex protection
unsafe impl Send for SubscribeTrack {}
unsafe impl Sync for SubscribeTrack {}

impl SubscribeTrack {
    /// Create a new subscribe track with static signals
    pub fn new(
        track_name: FullTrackName,
        priority: u8,
        group_order: GroupOrder,
        filter_type: FilterType,
        status_signal: &'static Signal<SubscribeStatus>,
        object_receiver: DynReceiver<'static, ReceivedObject>,
        callback_data: &'static SubscribeCallbackData,
    ) -> Result<Self> {
        debug!(
            "Creating subscribe track: {:?}, priority={}, order={:?}, filter={:?}",
            track_name, priority, group_order, filter_type
        );

        let callbacks = ffi::QuicrSubscribeTrackCallbacks {
            user_data: callback_data as *const _ as *mut c_void,
            on_status_changed: Some(subscribe_status_changed_callback),
            on_object_received: Some(object_received_callback),
            on_error: Some(subscribe_error_callback),
        };

        // Build FFI entries - IMPORTANT: we must keep ffi_entries alive and
        // build ffi_track_name to point to it (not to a temporary Vec)
        let (ffi_entries_vec, _) = track_name.to_ffi();
        let mut ffi_entries: HeaplessVec<ffi::QuicrBytes, 8> = HeaplessVec::new();
        for entry in ffi_entries_vec {
            let _ = ffi_entries.push(entry);
        }

        // Build ffi_track_name pointing to our HeaplessVec's buffer
        let ffi_track_name = ffi::QuicrFullTrackName {
            name_space: ffi::QuicrTrackNamespace {
                entries: ffi_entries.as_ptr() as *mut _,
                num_entries: ffi_entries.len(),
            },
            name: ffi::QuicrBytes {
                data: track_name.name.as_ptr(),
                len: track_name.name.len(),
            },
        };

        trace!("Calling FFI quicr_subscribe_track_create");
        let handler = unsafe {
            ffi::quicr_subscribe_track_create(
                &ffi_track_name,
                priority,
                group_order.into(),
                filter_type.into(),
                &callbacks,
            )
        };

        let handler = NonNull::new(handler).ok_or_else(|| {
            error!("Failed to create subscribe track - FFI returned null");
            Error::from_ffi()
        })?;

        info!("Subscribe track created: {:?}", track_name);
        Ok(Self {
            handler,
            track_name,
            callback_data,
            ffi_entries,
            status_signal,
            object_receiver,
            is_registered: AtomicBool::new(false),
        })
    }

    /// Get the track name
    pub fn track_name(&self) -> &FullTrackName {
        &self.track_name
    }

    /// Get the current subscribe status
    pub fn status(&self) -> SubscribeStatus {
        unsafe { ffi::quicr_subscribe_track_get_status(self.handler.as_ptr()) }.into()
    }

    /// Receive the next object
    pub async fn recv(&mut self) -> ReceivedObject {
        self.object_receiver.receive().await
    }

    /// Try to receive the next object without blocking
    ///
    /// Returns `Ok(object)` if an object is available, or `Err(TryReceiveError::Empty)` if not.
    pub fn try_recv(&mut self) -> core::result::Result<ReceivedObject, TryReceiveError> {
        self.object_receiver.try_receive()
    }

    /// Wait for status change
    pub async fn wait_status_change(&self) -> SubscribeStatus {
        self.status_signal.wait().await
    }

    /// Wait until subscription is active
    pub async fn wait_ready(&self) -> Result<()> {
        debug!(
            "Waiting for subscription to be ready: {:?}",
            self.track_name
        );
        loop {
            let status = self.status();
            match status {
                SubscribeStatus::Ok | SubscribeStatus::NewGroupRequested => {
                    info!("Subscription ready: {:?}", self.track_name);
                    return Ok(());
                }
                SubscribeStatus::Error
                | SubscribeStatus::NotAuthorized
                | SubscribeStatus::Cancelled
                | SubscribeStatus::DoneByFin
                | SubscribeStatus::DoneByReset => {
                    error!(
                        "Subscription failed to become ready: {:?}, status={:?}",
                        self.track_name, status
                    );
                    return Err(Error::subscribe("track failed to become ready"));
                }
                _ => {
                    trace!("Subscription status: {:?}, waiting...", status);
                    self.wait_status_change().await;
                }
            }
        }
    }

    /// Pause receiving data
    pub fn pause(&self) {
        debug!("Pausing subscription: {:?}", self.track_name);
        unsafe {
            ffi::quicr_subscribe_track_pause(self.handler.as_ptr());
        }
    }

    /// Resume receiving data
    pub fn resume(&self) {
        debug!("Resuming subscription: {:?}", self.track_name);
        unsafe {
            ffi::quicr_subscribe_track_resume(self.handler.as_ptr());
        }
    }

    /// Get the raw handler pointer (for internal use)
    pub(crate) fn as_ptr(&self) -> *mut ffi::QuicrSubscribeTrackHandler {
        self.handler.as_ptr()
    }

    /// Mark as registered with client
    pub(crate) fn set_registered(&self, registered: bool) {
        self.is_registered.store(registered, Ordering::SeqCst);
    }

    /// Check if registered
    pub(crate) fn is_registered(&self) -> bool {
        self.is_registered.load(Ordering::SeqCst)
    }
}

impl Drop for SubscribeTrack {
    fn drop(&mut self) {
        debug!("Destroying subscribe track: {:?}", self.track_name);
        trace!("Calling FFI quicr_subscribe_track_destroy");
        unsafe {
            ffi::quicr_subscribe_track_destroy(self.handler.as_ptr());
        }
    }
}

// ============================================================================
// Callback functions
// ============================================================================

extern "C" fn subscribe_status_changed_callback(
    user_data: *mut c_void,
    status: ffi::QuicrSubscribeStatus,
) {
    if user_data.is_null() {
        warn!("Subscribe status callback received null user_data");
        return;
    }

    let rust_status: SubscribeStatus = status.into();
    let data = unsafe { &*(user_data as *const SubscribeCallbackData) };

    #[cfg(feature = "std")]
    debug!(
        "[SUBSCRIBE STATUS] {} -> {:?}",
        data.track_name, rust_status
    );
    #[cfg(not(feature = "std"))]
    debug!(
        "[SUBSCRIBE STATUS] {} -> {:?}",
        data.track_name.as_str(),
        rust_status
    );

    data.status_signal.signal(rust_status);
}

extern "C" fn object_received_callback(
    user_data: *mut c_void,
    headers: *const ffi::QuicrObjectHeaders,
    data: *const u8,
    data_len: usize,
) {
    if user_data.is_null() || headers.is_null() {
        warn!("Object received callback got null pointer");
        return;
    }

    let callback_data = unsafe { &*(user_data as *const SubscribeCallbackData) };
    let headers = unsafe { &*headers };

    let payload = if !data.is_null() && data_len > 0 {
        Bytes::copy_from_slice(unsafe { core::slice::from_raw_parts(data, data_len) })
    } else {
        Bytes::new()
    };

    let obj_headers = ObjectHeaders::from(headers);
    trace!(
        "Object received: group={}, object={}, payload_size={}",
        obj_headers.group_id,
        obj_headers.object_id,
        payload.len()
    );

    let object = ReceivedObject {
        headers: obj_headers,
        data: payload,
    };

    if callback_data.object_sender.try_send(object).is_err() {
        warn!("Object receive buffer full or closed - dropping object");
    }
}

extern "C" fn subscribe_error_callback(user_data: *mut c_void, error_msg: *const c_char) {
    #[cfg(feature = "std")]
    let track_name: String = if user_data.is_null() {
        "<unknown>".to_string()
    } else {
        let data = unsafe { &*(user_data as *const SubscribeCallbackData) };
        data.track_name.clone()
    };

    #[cfg(not(feature = "std"))]
    let track_name: &str = if user_data.is_null() {
        "<unknown>"
    } else {
        let data = unsafe { &*(user_data as *const SubscribeCallbackData) };
        data.track_name.as_str()
    };

    let msg = if error_msg.is_null() {
        "<null error message>"
    } else {
        unsafe {
            CStr::from_ptr(error_msg)
                .to_str()
                .unwrap_or("<invalid utf8>")
        }
    };

    error!("C++ exception in subscribe track {}: {}", track_name, msg);
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for creating subscribe tracks
pub struct SubscribeTrackBuilder {
    track_name: FullTrackName,
    priority: u8,
    group_order: GroupOrder,
    filter_type: FilterType,
}

impl SubscribeTrackBuilder {
    /// Create a new builder
    pub fn new(track_name: FullTrackName) -> Self {
        Self {
            track_name,
            priority: 128,
            group_order: GroupOrder::Ascending,
            filter_type: FilterType::LargestObject,
        }
    }

    /// Set the priority
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set the group order
    pub fn group_order(mut self, order: GroupOrder) -> Self {
        self.group_order = order;
        self
    }

    /// Set the filter type
    pub fn filter_type(mut self, filter: FilterType) -> Self {
        self.filter_type = filter;
        self
    }

    /// Build the subscribe track with explicit signals (for no_std or when you need control)
    pub fn build_with_signals(
        self,
        status_signal: &'static Signal<SubscribeStatus>,
        object_receiver: DynReceiver<'static, ReceivedObject>,
        callback_data: &'static SubscribeCallbackData,
    ) -> Result<SubscribeTrack> {
        SubscribeTrack::new(
            self.track_name,
            self.priority,
            self.group_order,
            self.filter_type,
            status_signal,
            object_receiver,
            callback_data,
        )
    }

    /// Build the subscribe track (requires std feature)
    /// This is a convenience method - users should use `build_with_signals` for full control
    pub fn build(self) -> Result<SubscribeTrack> {
        #[cfg(feature = "std")]
        {
            use crate::runtime::Channel;

            // Create leaked channel for object delivery
            const BUFFER_SIZE: usize = 64;
            let channel: &'static Channel<ReceivedObject, BUFFER_SIZE> =
                Box::leak(Box::new(Channel::new()));

            let status_signal: &'static Signal<SubscribeStatus> =
                Box::leak(Box::new(Signal::new()));

            let track_name_str = format!("{:?}", self.track_name);

            let callback_data: &'static SubscribeCallbackData =
                Box::leak(Box::new(SubscribeCallbackData {
                    status_signal,
                    object_sender: channel.sender().into(),
                    track_name: track_name_str,
                }));

            let object_receiver = channel.receiver().into();

            self.build_with_signals(status_signal, object_receiver, callback_data)
        }

        #[cfg(not(feature = "std"))]
        {
            Err(Error::config(
                "build() requires std feature; use build_with_signals() for no_std",
            ))
        }
    }

    /// Get the configured track name
    pub fn track_name(&self) -> &FullTrackName {
        &self.track_name
    }

    /// Get the configured priority
    pub fn get_priority(&self) -> u8 {
        self.priority
    }

    /// Get the configured group order
    pub fn get_group_order(&self) -> GroupOrder {
        self.group_order
    }

    /// Get the configured filter type
    pub fn get_filter_type(&self) -> FilterType {
        self.filter_type
    }
}

// ============================================================================
// Subscription wrapper
// ============================================================================

/// A subscription stream that provides async iteration over received objects
pub struct Subscription {
    track: SubscribeTrack,
}

impl Subscription {
    /// Create a new subscription from an owned track
    pub fn new(track: SubscribeTrack) -> Self {
        Self { track }
    }

    /// Receive the next object
    pub async fn recv(&mut self) -> ReceivedObject {
        self.track.recv().await
    }

    /// Try to receive the next object without blocking
    pub fn try_recv(&mut self) -> core::result::Result<ReceivedObject, TryReceiveError> {
        self.track.try_recv()
    }

    /// Pause the subscription
    pub fn pause(&self) {
        self.track.pause();
    }

    /// Resume the subscription
    pub fn resume(&self) {
        self.track.resume();
    }

    /// Check if the subscription is still active
    pub fn is_active(&self) -> bool {
        self.track.status().is_active()
    }

    /// Check if the subscription has ended
    pub fn is_done(&self) -> bool {
        self.track.status().is_done()
    }

    /// Get the underlying track
    pub fn track(&self) -> &SubscribeTrack {
        &self.track
    }

    /// Get mutable access to the underlying track
    pub fn track_mut(&mut self) -> &mut SubscribeTrack {
        &mut self.track
    }

    /// Get the current subscription status
    pub fn status(&self) -> SubscribeStatus {
        self.track.status()
    }

    /// Wait until subscription is ready
    pub async fn wait_ready(&self) -> Result<()> {
        self.track.wait_ready().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscribe_status_is_active() {
        assert!(SubscribeStatus::Ok.is_active());
        assert!(SubscribeStatus::NewGroupRequested.is_active());
        assert!(!SubscribeStatus::Paused.is_active());
        assert!(!SubscribeStatus::Cancelled.is_active());
    }

    #[test]
    fn test_subscribe_status_is_done() {
        assert!(SubscribeStatus::Cancelled.is_done());
        assert!(SubscribeStatus::DoneByFin.is_done());
        assert!(SubscribeStatus::DoneByReset.is_done());
        assert!(SubscribeStatus::Error.is_done());
        assert!(!SubscribeStatus::Ok.is_done());
    }
}
