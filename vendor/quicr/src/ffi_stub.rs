// FFI stub implementations for development without the C++ toolchain
//
// This module provides mock implementations of all FFI types and functions
// when the `ffi-stub` feature is enabled. Use for development/testing only.

// Re-export c_* types
#[cfg(not(feature = "std"))]
pub use core::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_ushort, c_void};

#[cfg(feature = "std")]
pub use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_ushort, c_void};

// ============================================================================
// Status types
// ============================================================================

pub type QuicrStatus = i32;
pub const QuicrStatus_QUICR_STATUS_OK: QuicrStatus = 0;
pub const QuicrStatus_QUICR_STATUS_CONNECTING: QuicrStatus = 1;
pub const QuicrStatus_QUICR_STATUS_READY: QuicrStatus = 2;
pub const QuicrStatus_QUICR_STATUS_DISCONNECTING: QuicrStatus = 3;
pub const QuicrStatus_QUICR_STATUS_DISCONNECTED: QuicrStatus = 4;
pub const QuicrStatus_QUICR_STATUS_ERROR: QuicrStatus = 5;
pub const QuicrStatus_QUICR_STATUS_IDLE_TIMEOUT: QuicrStatus = 6;
pub const QuicrStatus_QUICR_STATUS_SHUTDOWN: QuicrStatus = 7;

pub type QuicrPublishStatus = i32;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_OK: QuicrPublishStatus = 0;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_NOT_CONNECTED: QuicrPublishStatus = 1;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_NOT_ANNOUNCED: QuicrPublishStatus = 2;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_PENDING_ANNOUNCE: QuicrPublishStatus = 3;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_ANNOUNCE_NOT_AUTHORIZED: QuicrPublishStatus = 4;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_NO_SUBSCRIBERS: QuicrPublishStatus = 5;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_SENDING_UNANNOUNCE: QuicrPublishStatus = 6;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_SUBSCRIPTION_UPDATED: QuicrPublishStatus = 7;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_NEW_GROUP_REQUESTED: QuicrPublishStatus = 8;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_PENDING_PUBLISH_OK: QuicrPublishStatus = 9;
pub const QuicrPublishStatus_QUICR_PUBLISH_STATUS_PAUSED: QuicrPublishStatus = 10;

pub type QuicrPublishObjectStatus = i32;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OK: QuicrPublishObjectStatus = 0;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR: QuicrPublishObjectStatus = 1;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NOT_AUTHORIZED: QuicrPublishObjectStatus = 2;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NOT_ANNOUNCED: QuicrPublishObjectStatus = 3;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NO_SUBSCRIBERS: QuicrPublishObjectStatus = 4;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PAYLOAD_LENGTH_EXCEEDED: QuicrPublishObjectStatus = 5;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PREVIOUS_OBJECT_TRUNCATED: QuicrPublishObjectStatus = 6;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_NO_PREVIOUS_OBJECT: QuicrPublishObjectStatus = 7;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_COMPLETE: QuicrPublishObjectStatus = 8;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_CONTINUATION_DATA_NEEDED: QuicrPublishObjectStatus = 9;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_INCOMPLETE: QuicrPublishObjectStatus = 10;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_TOO_LARGE: QuicrPublishObjectStatus = 11;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_GROUP: QuicrPublishObjectStatus = 12;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_TRACK: QuicrPublishObjectStatus = 13;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PAUSED: QuicrPublishObjectStatus = 14;
pub const QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_PENDING_PUBLISH_OK: QuicrPublishObjectStatus = 15;

pub type QuicrSubscribeStatus = i32;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_OK: QuicrSubscribeStatus = 0;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_CONNECTED: QuicrSubscribeStatus = 1;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_ERROR: QuicrSubscribeStatus = 2;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_AUTHORIZED: QuicrSubscribeStatus = 3;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED: QuicrSubscribeStatus = 4;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_PENDING_RESPONSE: QuicrSubscribeStatus = 5;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_SENDING_UNSUBSCRIBE: QuicrSubscribeStatus = 6;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_PAUSED: QuicrSubscribeStatus = 7;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_NEW_GROUP_REQUESTED: QuicrSubscribeStatus = 8;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_CANCELLED: QuicrSubscribeStatus = 9;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_DONE_BY_FIN: QuicrSubscribeStatus = 10;
pub const QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_DONE_BY_RESET: QuicrSubscribeStatus = 11;

pub type QuicrObjectStatus = i32;
pub const QuicrObjectStatus_QUICR_OBJECT_STATUS_AVAILABLE: QuicrObjectStatus = 0;
pub const QuicrObjectStatus_QUICR_OBJECT_STATUS_DOES_NOT_EXIST: QuicrObjectStatus = 1;
pub const QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_GROUP: QuicrObjectStatus = 3;
pub const QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_TRACK: QuicrObjectStatus = 4;
pub const QuicrObjectStatus_QUICR_OBJECT_STATUS_END_OF_SUBGROUP: QuicrObjectStatus = 5;

pub type QuicrTrackMode = i32;
pub const QuicrTrackMode_QUICR_TRACK_MODE_DATAGRAM: QuicrTrackMode = 0;
pub const QuicrTrackMode_QUICR_TRACK_MODE_STREAM: QuicrTrackMode = 1;

pub type QuicrGroupOrder = i32;
pub const QuicrGroupOrder_QUICR_GROUP_ORDER_ASCENDING: QuicrGroupOrder = 0;
pub const QuicrGroupOrder_QUICR_GROUP_ORDER_DESCENDING: QuicrGroupOrder = 1;

pub type QuicrFilterType = i32;
pub const QuicrFilterType_QUICR_FILTER_TYPE_NEXT_GROUP_START: QuicrFilterType = 1;
pub const QuicrFilterType_QUICR_FILTER_TYPE_LARGEST_OBJECT: QuicrFilterType = 2;
pub const QuicrFilterType_QUICR_FILTER_TYPE_ABSOLUTE_START: QuicrFilterType = 3;
pub const QuicrFilterType_QUICR_FILTER_TYPE_ABSOLUTE_RANGE: QuicrFilterType = 4;

// ============================================================================
// Data structures
// ============================================================================

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrBytes {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrTrackNamespace {
    pub entries: *mut QuicrBytes,
    pub num_entries: usize,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrFullTrackName {
    pub name_space: QuicrTrackNamespace,
    pub name: QuicrBytes,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrObjectHeaders {
    pub group_id: u64,
    pub object_id: u64,
    pub subgroup_id: u64,
    pub payload_length: u64,
    pub status: QuicrObjectStatus,
    pub priority: u8,
    pub has_priority: bool,
    pub ttl: u16,
    pub has_ttl: bool,
    pub track_mode: QuicrTrackMode,
    pub has_track_mode: bool,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrTransportConfig {
    pub tls_cert_filename: *const c_char,
    pub tls_key_filename: *const c_char,
    pub time_queue_init_queue_size: u32,
    pub time_queue_max_duration: u32,
    pub time_queue_bucket_interval: u32,
    pub time_queue_rx_size: u32,
    pub log_level: u32,
    pub quic_cwin_minimum: u64,
    pub quic_wifi_shadow_rtt_us: u32,
    pub idle_timeout_ms: u64,
    pub use_reset_wait_strategy: bool,
    pub use_bbr: bool,
    pub quic_qlog_path: *const c_char,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrClientConfig {
    pub endpoint_id: *const c_char,
    pub connect_uri: *const c_char,
    pub metrics_sample_ms: u64,
    pub transport_config: QuicrTransportConfig,
}

// ============================================================================
// Opaque handler types
// ============================================================================

#[repr(C)]
pub struct QuicrClient {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct QuicrPublishTrackHandler {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct QuicrSubscribeTrackHandler {
    _opaque: [u8; 0],
}

// ============================================================================
// Callback types
// ============================================================================

pub type QuicrStatusChangedCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, status: QuicrStatus)>;

pub type QuicrServerSetupCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, moqt_version: u64, server_id: *const c_char)>;

pub type QuicrErrorCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, error_msg: *const c_char)>;

pub type QuicrPublishStatusChangedCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, status: QuicrPublishStatus)>;

pub type QuicrSubscribeStatusChangedCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, status: QuicrSubscribeStatus)>;

pub type QuicrObjectReceivedCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        headers: *const QuicrObjectHeaders,
        data: *const u8,
        data_len: usize,
    ),
>;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrClientCallbacks {
    pub user_data: *mut c_void,
    pub on_status_changed: QuicrStatusChangedCallback,
    pub on_server_setup: QuicrServerSetupCallback,
    pub on_error: QuicrErrorCallback,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrPublishTrackCallbacks {
    pub user_data: *mut c_void,
    pub on_status_changed: QuicrPublishStatusChangedCallback,
    pub on_error: QuicrErrorCallback,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct QuicrSubscribeTrackCallbacks {
    pub user_data: *mut c_void,
    pub on_status_changed: QuicrSubscribeStatusChangedCallback,
    pub on_object_received: QuicrObjectReceivedCallback,
    pub on_error: QuicrErrorCallback,
}

// ============================================================================
// Stub function implementations
// ============================================================================

// Global error message storage
static mut LAST_ERROR: [u8; 256] = [0u8; 256];

/// Get the last error message
#[no_mangle]
pub unsafe extern "C" fn quicr_last_error() -> *const c_char {
    // SAFETY: This is a read-only access to a static, and we're in an unsafe fn
    unsafe { LAST_ERROR.as_ptr() as *const c_char }
}

// --- Client functions ---

#[no_mangle]
pub unsafe extern "C" fn quicr_client_create(
    _config: *const QuicrClientConfig,
    _callbacks: *const QuicrClientCallbacks,
) -> *mut QuicrClient {
    // Return a non-null "stub" pointer (just a fixed address for stub purposes)
    // In real code this would allocate, for stub we use a static dummy
    static mut STUB_CLIENT: u8 = 0;
    core::ptr::addr_of_mut!(STUB_CLIENT) as *mut QuicrClient
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_destroy(_client: *mut QuicrClient) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_connect(_client: *mut QuicrClient) -> QuicrStatus {
    QuicrStatus_QUICR_STATUS_READY
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_disconnect(_client: *mut QuicrClient) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_get_status(_client: *mut QuicrClient) -> QuicrStatus {
    QuicrStatus_QUICR_STATUS_READY
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_publish_track(
    _client: *mut QuicrClient,
    _track: *mut QuicrPublishTrackHandler,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_unpublish_track(
    _client: *mut QuicrClient,
    _track: *mut QuicrPublishTrackHandler,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_subscribe_track(
    _client: *mut QuicrClient,
    _track: *mut QuicrSubscribeTrackHandler,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_unsubscribe_track(
    _client: *mut QuicrClient,
    _track: *mut QuicrSubscribeTrackHandler,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_publish_namespace(
    _client: *mut QuicrClient,
    _namespace: *const QuicrTrackNamespace,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_unpublish_namespace(
    _client: *mut QuicrClient,
    _namespace: *const QuicrTrackNamespace,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_subscribe_namespace(
    _client: *mut QuicrClient,
    _namespace: *const QuicrTrackNamespace,
) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_client_unsubscribe_namespace(
    _client: *mut QuicrClient,
    _namespace: *const QuicrTrackNamespace,
) {
    // No-op in stub mode
}

// --- Publish track functions ---

#[no_mangle]
pub unsafe extern "C" fn quicr_publish_track_create(
    _track_name: *const QuicrFullTrackName,
    _track_mode: QuicrTrackMode,
    _default_priority: u8,
    _default_ttl: u32,
    _callbacks: *const QuicrPublishTrackCallbacks,
) -> *mut QuicrPublishTrackHandler {
    static mut STUB_PUBLISH_TRACK: u8 = 0;
    core::ptr::addr_of_mut!(STUB_PUBLISH_TRACK) as *mut QuicrPublishTrackHandler
}

#[no_mangle]
pub unsafe extern "C" fn quicr_publish_track_destroy(_track: *mut QuicrPublishTrackHandler) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_publish_track_get_status(
    _track: *mut QuicrPublishTrackHandler,
) -> QuicrPublishStatus {
    QuicrPublishStatus_QUICR_PUBLISH_STATUS_OK
}

#[no_mangle]
pub unsafe extern "C" fn quicr_publish_track_can_publish(
    _track: *mut QuicrPublishTrackHandler,
) -> bool {
    true
}

#[no_mangle]
pub unsafe extern "C" fn quicr_publish_track_publish_object(
    _track: *mut QuicrPublishTrackHandler,
    _headers: *const QuicrObjectHeaders,
    _data: *const u8,
    _data_len: usize,
) -> QuicrPublishObjectStatus {
    QuicrPublishObjectStatus_QUICR_PUBLISH_OBJECT_STATUS_OK
}

// --- Subscribe track functions ---

#[no_mangle]
pub unsafe extern "C" fn quicr_subscribe_track_create(
    _track_name: *const QuicrFullTrackName,
    _priority: u8,
    _group_order: QuicrGroupOrder,
    _filter_type: QuicrFilterType,
    _callbacks: *const QuicrSubscribeTrackCallbacks,
) -> *mut QuicrSubscribeTrackHandler {
    static mut STUB_SUBSCRIBE_TRACK: u8 = 0;
    core::ptr::addr_of_mut!(STUB_SUBSCRIBE_TRACK) as *mut QuicrSubscribeTrackHandler
}

#[no_mangle]
pub unsafe extern "C" fn quicr_subscribe_track_destroy(_track: *mut QuicrSubscribeTrackHandler) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_subscribe_track_get_status(
    _track: *mut QuicrSubscribeTrackHandler,
) -> QuicrSubscribeStatus {
    QuicrSubscribeStatus_QUICR_SUBSCRIBE_STATUS_OK
}

#[no_mangle]
pub unsafe extern "C" fn quicr_subscribe_track_pause(_track: *mut QuicrSubscribeTrackHandler) {
    // No-op in stub mode
}

#[no_mangle]
pub unsafe extern "C" fn quicr_subscribe_track_resume(_track: *mut QuicrSubscribeTrackHandler) {
    // No-op in stub mode
}
