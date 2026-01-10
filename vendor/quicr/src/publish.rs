//! Publish track functionality using Embassy async runtime

use crate::error::{Error, Result};
use crate::ffi;
use crate::object::{ObjectHeaders, TrackMode};
use crate::runtime::{Arc, Signal};
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
macro_rules! debug { ($($arg:tt)*) => {}; }
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! error { ($($arg:tt)*) => {}; }
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! info { ($($arg:tt)*) => {}; }
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! trace { ($($arg:tt)*) => {}; }
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! warn { ($($arg:tt)*) => {}; }

#[cfg(feature = "std")]
use std::ffi::c_void;
#[cfg(not(feature = "std"))]
use crate::ffi::c_void;

#[cfg(feature = "std")]
use std::ffi::{c_char, CStr};
#[cfg(not(feature = "std"))]
use core::ffi::{c_char, CStr};

#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::string::String;

/// Status of a publish track
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt-logging", derive(defmt::Format))]
pub enum PublishStatus {
    /// Track is ready to publish
    Ok,
    /// Not connected to relay
    NotConnected,
    /// Track not announced
    NotAnnounced,
    /// Waiting for announce response
    PendingAnnounce,
    /// Announce was not authorized
    AnnounceNotAuthorized,
    /// No subscribers for this track
    NoSubscribers,
    /// Sending unannounce
    SendingUnannounce,
    /// Subscription was updated
    SubscriptionUpdated,
    /// New group was requested
    NewGroupRequested,
    /// Pending publish ok
    PendingPublishOk,
    /// Publishing is paused
    Paused,
}

impl From<ffi::QuicrPublishStatus> for PublishStatus {
    fn from(status: ffi::QuicrPublishStatus) -> Self {
        match status {
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_OK => PublishStatus::Ok,
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_NOT_CONNECTED => PublishStatus::NotConnected,
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_NOT_ANNOUNCED => PublishStatus::NotAnnounced,
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_PENDING_ANNOUNCE => PublishStatus::PendingAnnounce,
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_ANNOUNCE_NOT_AUTHORIZED => {
                PublishStatus::AnnounceNotAuthorized
            }
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_NO_SUBSCRIBERS => PublishStatus::NoSubscribers,
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_SENDING_UNANNOUNCE => {
                PublishStatus::SendingUnannounce
            }
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_SUBSCRIPTION_UPDATED => {
                PublishStatus::SubscriptionUpdated
            }
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_NEW_GROUP_REQUESTED => {
                PublishStatus::NewGroupRequested
            }
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_PENDING_PUBLISH_OK => {
                PublishStatus::PendingPublishOk
            }
            ffi::QuicrPublishStatus_QUICR_PUBLISH_STATUS_PAUSED => PublishStatus::Paused,
            _ => PublishStatus::NotConnected,
        }
    }
}

/// Status returned when publishing an object
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt-logging", derive(defmt::Format))]
pub enum PublishObjectStatus {
    /// Object was published successfully
    Ok,
    /// Internal error occurred
    InternalError,
    /// Not authorized to publish
    NotAuthorized,
    /// Track not announced
    NotAnnounced,
    /// No subscribers
    NoSubscribers,
    /// Payload length exceeded
    PayloadLengthExceeded,
    /// Previous object was truncated
    PreviousObjectTruncated,
    /// No previous object
    NoPreviousObject,
    /// Object data complete
    ObjectDataComplete,
    /// Continuation data needed
    ContinuationDataNeeded,
    /// Object data incomplete
    ObjectDataIncomplete,
    /// Object data too large
    ObjectDataTooLarge,
    /// Must start new group
    MustStartNewGroup,
    /// Must start new track
    MustStartNewTrack,
    /// Publishing paused
    Paused,
    /// Pending publish ok
    PendingPublishOk,
}

impl From<ffi::QuicrPublishObjectStatus> for PublishObjectStatus {
    fn from(status: ffi::QuicrPublishObjectStatus) -> Self {
        match status {
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OK => PublishObjectStatus::Ok,
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR => {
                PublishObjectStatus::InternalError
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NOT_AUTHORIZED => {
                PublishObjectStatus::NotAuthorized
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NOT_ANNOUNCED => {
                PublishObjectStatus::NotAnnounced
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NO_SUBSCRIBERS => {
                PublishObjectStatus::NoSubscribers
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PAYLOAD_LENGTH_EXCEEDED => {
                PublishObjectStatus::PayloadLengthExceeded
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PREVIOUS_OBJECT_TRUNCATED => {
                PublishObjectStatus::PreviousObjectTruncated
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NO_PREVIOUS_OBJECT => {
                PublishObjectStatus::NoPreviousObject
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_COMPLETE => {
                PublishObjectStatus::ObjectDataComplete
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_CONTINUATION_DATA_NEEDED => {
                PublishObjectStatus::ContinuationDataNeeded
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_INCOMPLETE => {
                PublishObjectStatus::ObjectDataIncomplete
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_TOO_LARGE => {
                PublishObjectStatus::ObjectDataTooLarge
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_GROUP => {
                PublishObjectStatus::MustStartNewGroup
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_TRACK => {
                PublishObjectStatus::MustStartNewTrack
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PAUSED => {
                PublishObjectStatus::Paused
            }
            ffi::QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PENDING_PUBLISH_OK => {
                PublishObjectStatus::PendingPublishOk
            }
            _ => PublishObjectStatus::InternalError,
        }
    }
}

impl PublishObjectStatus {
    /// Check if the status indicates success
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            PublishObjectStatus::Ok | PublishObjectStatus::ObjectDataComplete
        )
    }

    /// Check if publishing can continue
    pub fn can_continue(&self) -> bool {
        matches!(
            self,
            PublishObjectStatus::Ok
                | PublishObjectStatus::ObjectDataComplete
                | PublishObjectStatus::ContinuationDataNeeded
                | PublishObjectStatus::NoSubscribers
        )
    }
}

/// Callback data for publish track
///
/// This struct stores the signal and metadata for publish track callbacks.
/// For no_std builds, users must provide static storage for this data.
pub struct PublishCallbackData {
    /// Signal for publish status changes
    pub status_signal: &'static Signal<PublishStatus>,
    /// Track name for logging (std version)
    #[cfg(feature = "std")]
    pub track_name: String,
    /// Track name for logging (no_std version)
    #[cfg(not(feature = "std"))]
    pub track_name: heapless::String<64>,
}

impl PublishCallbackData {
    /// Create new publish callback data
    #[cfg(feature = "std")]
    pub fn new(status_signal: &'static Signal<PublishStatus>, track_name: impl Into<String>) -> Self {
        Self {
            status_signal,
            track_name: track_name.into(),
        }
    }

    /// Create new publish callback data (no_std version)
    #[cfg(not(feature = "std"))]
    pub fn new(status_signal: &'static Signal<PublishStatus>, track_name: &str) -> Self {
        let mut name = heapless::String::new();
        let _ = name.push_str(track_name);
        Self {
            status_signal,
            track_name: name,
        }
    }
}

/// Handle to a publish track
pub struct PublishTrack {
    handler: NonNull<ffi::QuicrPublishTrackHandler>,
    track_name: FullTrackName,
    #[allow(dead_code)]
    callback_data: &'static PublishCallbackData,
    #[allow(dead_code)]
    ffi_entries: HeaplessVec<ffi::QuicrBytes, 8>,
    status_signal: &'static Signal<PublishStatus>,
    is_registered: AtomicBool,
}

// SAFETY: The underlying C++ object is thread-safe with mutex protection
unsafe impl Send for PublishTrack {}
unsafe impl Sync for PublishTrack {}

impl PublishTrack {
    /// Create a new publish track with static signal
    pub fn new(
        track_name: FullTrackName,
        track_mode: TrackMode,
        default_priority: u8,
        default_ttl: u32,
        status_signal: &'static Signal<PublishStatus>,
        callback_data: &'static PublishCallbackData,
    ) -> Result<Self> {
        debug!(
            "Creating publish track: {:?}, mode={:?}, priority={}, ttl={}",
            track_name, track_mode, default_priority, default_ttl
        );

        let callbacks = ffi::QuicrPublishTrackCallbacks {
            user_data: callback_data as *const _ as *mut c_void,
            on_status_changed: Some(publish_status_changed_callback),
            on_error: Some(publish_error_callback),
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

        trace!("Calling FFI quicr_publish_track_create");
        let handler = unsafe {
            ffi::quicr_publish_track_create(
                &ffi_track_name,
                track_mode.into(),
                default_priority,
                default_ttl,
                &callbacks,
            )
        };

        let handler = NonNull::new(handler).ok_or_else(|| {
            error!("Failed to create publish track - FFI returned null");
            Error::from_ffi()
        })?;

        info!("Publish track created: {:?}", track_name);
        Ok(Self {
            handler,
            track_name,
            callback_data,
            ffi_entries,
            status_signal,
            is_registered: AtomicBool::new(false),
        })
    }

    /// Get the track name
    pub fn track_name(&self) -> &FullTrackName {
        &self.track_name
    }

    /// Get the current publish status
    pub fn status(&self) -> PublishStatus {
        unsafe { ffi::quicr_publish_track_get_status(self.handler.as_ptr()) }.into()
    }

    /// Wait for status change
    pub async fn wait_status_change(&self) -> PublishStatus {
        self.status_signal.wait().await
    }

    /// Wait until ready to publish
    pub async fn wait_ready(&self) -> Result<()> {
        debug!("Waiting for publish track to be ready: {:?}", self.track_name);
        loop {
            let status = self.status();
            match status {
                PublishStatus::Ok
                | PublishStatus::SubscriptionUpdated
                | PublishStatus::NewGroupRequested => {
                    info!("Publish track ready: {:?}", self.track_name);
                    return Ok(());
                }
                PublishStatus::NotConnected
                | PublishStatus::AnnounceNotAuthorized
                | PublishStatus::SendingUnannounce => {
                    error!(
                        "Publish track failed to become ready: {:?}, status={:?}",
                        self.track_name, status
                    );
                    return Err(Error::publish("track failed to become ready"));
                }
                _ => {
                    trace!("Publish track status: {:?}, waiting...", status);
                    self.wait_status_change().await;
                }
            }
        }
    }

    /// Check if publishing is currently allowed
    pub fn can_publish(&self) -> bool {
        unsafe { ffi::quicr_publish_track_can_publish(self.handler.as_ptr()) }
    }

    /// Publish an object
    pub fn publish(&self, headers: &ObjectHeaders, data: impl AsRef<[u8]>) -> Result<PublishObjectStatus> {
        let data = data.as_ref();
        trace!(
            "Publishing object: group={}, object={}, payload_size={}",
            headers.group_id, headers.object_id, data.len()
        );

        let mut ffi_headers = headers.to_ffi();
        ffi_headers.payload_length = data.len() as u64;

        let status = unsafe {
            ffi::quicr_publish_track_publish_object(
                self.handler.as_ptr(),
                &ffi_headers,
                data.as_ptr(),
                data.len(),
            )
        };

        let status: PublishObjectStatus = status.into();
        if status == PublishObjectStatus::InternalError {
            let error = Error::from_ffi();
            error!(
                "Publish failed with internal error (group={}, object={})",
                headers.group_id, headers.object_id
            );
            return Err(error);
        } else if !status.is_ok() && !matches!(status, PublishObjectStatus::NoSubscribers) {
            warn!(
                "Publish returned non-ok status: {:?} (group={}, object={})",
                status, headers.group_id, headers.object_id
            );
        } else {
            trace!(
                "Object published: group={}, object={}, status={:?}",
                headers.group_id, headers.object_id, status
            );
        }

        Ok(status)
    }

    /// Publish bytes data
    pub fn publish_bytes(&self, headers: &ObjectHeaders, data: Bytes) -> Result<PublishObjectStatus> {
        self.publish(headers, data.as_ref())
    }

    /// Get the raw handler pointer (for internal use)
    pub(crate) fn as_ptr(&self) -> *mut ffi::QuicrPublishTrackHandler {
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

impl Drop for PublishTrack {
    fn drop(&mut self) {
        debug!("Destroying publish track: {:?}", self.track_name);
        trace!("Calling FFI quicr_publish_track_destroy");
        unsafe {
            ffi::quicr_publish_track_destroy(self.handler.as_ptr());
        }
    }
}

// ============================================================================
// Callback functions
// ============================================================================

extern "C" fn publish_status_changed_callback(
    user_data: *mut c_void,
    status: ffi::QuicrPublishStatus,
) {
    if user_data.is_null() {
        warn!("Publish status callback received null user_data");
        return;
    }

    let rust_status: PublishStatus = status.into();
    let data = unsafe { &*(user_data as *const PublishCallbackData) };

    #[cfg(feature = "std")]
    info!("[PUBLISH STATUS] {} -> {:?}", data.track_name, rust_status);
    #[cfg(not(feature = "std"))]
    info!("[PUBLISH STATUS] {} -> {:?}", data.track_name.as_str(), rust_status);

    data.status_signal.signal(rust_status);
}

extern "C" fn publish_error_callback(
    user_data: *mut c_void,
    error_msg: *const c_char,
) {
    #[cfg(feature = "std")]
    let track_name: String = if user_data.is_null() {
        "<unknown>".to_string()
    } else {
        let data = unsafe { &*(user_data as *const PublishCallbackData) };
        data.track_name.clone()
    };

    #[cfg(not(feature = "std"))]
    let track_name: &str = if user_data.is_null() {
        "<unknown>"
    } else {
        let data = unsafe { &*(user_data as *const PublishCallbackData) };
        data.track_name.as_str()
    };

    let msg = if error_msg.is_null() {
        "<null error message>"
    } else {
        unsafe { CStr::from_ptr(error_msg).to_str().unwrap_or("<invalid utf8>") }
    };

    error!("C++ exception in publish track {}: {}", track_name, msg);
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for creating publish tracks
pub struct PublishTrackBuilder {
    track_name: FullTrackName,
    track_mode: TrackMode,
    default_priority: u8,
    default_ttl: u32,
}

impl PublishTrackBuilder {
    /// Create a new builder
    pub fn new(track_name: FullTrackName) -> Self {
        Self {
            track_name,
            track_mode: TrackMode::Stream,
            default_priority: 128,
            default_ttl: 1000,
        }
    }

    /// Set the track mode
    pub fn track_mode(mut self, mode: TrackMode) -> Self {
        self.track_mode = mode;
        self
    }

    /// Set the default priority
    pub fn default_priority(mut self, priority: u8) -> Self {
        self.default_priority = priority;
        self
    }

    /// Set the default TTL in milliseconds
    pub fn default_ttl(mut self, ttl: u32) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Build the publish track with static signals
    pub fn build_with_signal(
        self,
        status_signal: &'static Signal<PublishStatus>,
        callback_data: &'static PublishCallbackData,
    ) -> Result<PublishTrack> {
        PublishTrack::new(
            self.track_name,
            self.track_mode,
            self.default_priority,
            self.default_ttl,
            status_signal,
            callback_data,
        )
    }

    /// Build the publish track (requires static allocation macros)
    /// This is a convenience method - users should use `build_with_signal` for full control
    pub fn build(self) -> Result<PublishTrack> {
        // For std builds, we can use a leaked box
        // For no_std, users must provide static storage
        #[cfg(feature = "std")]
        {
            use crate::runtime::Signal;

            let status_signal: &'static Signal<PublishStatus> =
                Box::leak(Box::new(Signal::new()));

            #[cfg(feature = "std")]
            let track_name_str = format!("{:?}", self.track_name);

            let callback_data: &'static PublishCallbackData =
                Box::leak(Box::new(PublishCallbackData {
                    status_signal,
                    track_name: track_name_str,
                }));

            self.build_with_signal(status_signal, callback_data)
        }

        #[cfg(not(feature = "std"))]
        {
            Err(Error::config("build() requires std feature; use build_with_signal() for no_std"))
        }
    }

    /// Get the configured track name
    pub fn track_name(&self) -> &FullTrackName {
        &self.track_name
    }

    /// Get the configured track mode
    pub fn get_track_mode(&self) -> TrackMode {
        self.track_mode
    }

    /// Get the configured default priority
    pub fn get_default_priority(&self) -> u8 {
        self.default_priority
    }

    /// Get the configured default TTL
    pub fn get_default_ttl(&self) -> u32 {
        self.default_ttl
    }
}

/// A publisher that wraps a publish track with an Arc for shared ownership
pub struct Publisher {
    track: Arc<PublishTrack>,
    group_id: u64,
    object_id: u64,
}

impl Publisher {
    /// Create a new publisher
    pub fn new(track: PublishTrack) -> Self {
        debug!("Creating publisher for track: {:?}", track.track_name());
        Self {
            track: Arc::new(track),
            group_id: 0,
            object_id: 0,
        }
    }

    /// Get a clone of the track for shared use
    pub fn track(&self) -> Arc<PublishTrack> {
        Arc::clone(&self.track)
    }

    /// Check if ready to publish
    pub fn can_publish(&self) -> bool {
        self.track.can_publish()
    }

    /// Publish a message with auto-incrementing IDs
    pub fn publish(&mut self, data: impl AsRef<[u8]>) -> Result<PublishObjectStatus> {
        let headers = ObjectHeaders::new(self.group_id, self.object_id);
        let status = self.track.publish(&headers, data)?;

        if status.is_ok() {
            self.object_id += 1;
        }

        Ok(status)
    }

    /// Start a new group
    pub fn new_group(&mut self) {
        self.group_id += 1;
        self.object_id = 0;
        debug!(
            "Publisher started new group: group_id={}",
            self.group_id
        );
    }

    /// Get current group ID
    pub fn group_id(&self) -> u64 {
        self.group_id
    }

    /// Get current object ID
    pub fn object_id(&self) -> u64 {
        self.object_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_object_status_is_ok() {
        assert!(PublishObjectStatus::Ok.is_ok());
        assert!(PublishObjectStatus::ObjectDataComplete.is_ok());
        assert!(!PublishObjectStatus::InternalError.is_ok());
        assert!(!PublishObjectStatus::NoSubscribers.is_ok());
    }

    #[test]
    fn test_publish_object_status_can_continue() {
        assert!(PublishObjectStatus::Ok.can_continue());
        assert!(PublishObjectStatus::ObjectDataComplete.can_continue());
        assert!(PublishObjectStatus::ContinuationDataNeeded.can_continue());
        assert!(PublishObjectStatus::NoSubscribers.can_continue());
        assert!(!PublishObjectStatus::InternalError.can_continue());
        assert!(!PublishObjectStatus::NotAuthorized.can_continue());
    }
}
