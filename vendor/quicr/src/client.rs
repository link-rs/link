//! MoQ Client implementation using Embassy async runtime

use crate::config::ClientConfig;
use crate::error::{Error, Result};
use crate::ffi;
use crate::object::{FilterType, GroupOrder, TrackMode};
use crate::publish::{PublishTrack, PublishTrackBuilder};
use crate::runtime::{timeout, Arc, Duration, Mutex, Signal};
use crate::subscribe::{SubscribeTrack, SubscribeTrackBuilder, Subscription};
use crate::track::{FullTrackName, TrackNamespace};
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

/// Connection status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt-logging", derive(defmt::Format))]
pub enum Status {
    /// Connected and ready
    Ok,
    /// Connecting to relay
    Connecting,
    /// Ready for operations
    Ready,
    /// Disconnecting
    Disconnecting,
    /// Disconnected
    Disconnected,
    /// Error occurred
    Error,
    /// Idle timeout
    IdleTimeout,
    /// Shutting down
    Shutdown,
}

impl From<ffi::QuicrStatus> for Status {
    fn from(status: ffi::QuicrStatus) -> Self {
        match status {
            ffi::QuicrStatus_QUICR_STATUS_OK => Status::Ok,
            ffi::QuicrStatus_QUICR_STATUS_CONNECTING => Status::Connecting,
            ffi::QuicrStatus_QUICR_STATUS_READY => Status::Ready,
            ffi::QuicrStatus_QUICR_STATUS_DISCONNECTING => Status::Disconnecting,
            ffi::QuicrStatus_QUICR_STATUS_DISCONNECTED => Status::Disconnected,
            ffi::QuicrStatus_QUICR_STATUS_ERROR => Status::Error,
            ffi::QuicrStatus_QUICR_STATUS_IDLE_TIMEOUT => Status::IdleTimeout,
            ffi::QuicrStatus_QUICR_STATUS_SHUTDOWN => Status::Shutdown,
            _ => Status::Error,
        }
    }
}

impl Status {
    /// Check if the client is connected and ready
    pub fn is_ready(&self) -> bool {
        matches!(self, Status::Ready | Status::Ok)
    }

    /// Check if the client is connecting
    pub fn is_connecting(&self) -> bool {
        matches!(self, Status::Connecting)
    }

    /// Check if the client is disconnected
    pub fn is_disconnected(&self) -> bool {
        matches!(
            self,
            Status::Disconnected | Status::Error | Status::IdleTimeout | Status::Shutdown
        )
    }
}

/// Server setup information
#[derive(Debug, Clone)]
pub struct ServerSetup {
    /// MoQT version used by server
    pub moqt_version: u64,
    /// Server identifier
    pub server_id: String,
}

/// Callback data for the client
///
/// This struct is used to store references to signals that receive callbacks
/// from the underlying C++ library. For no_std builds, users must provide
/// static storage for this data.
pub struct ClientCallbackData {
    /// Signal for status changes
    pub status_signal: &'static Signal<Status>,
    /// Signal for server setup information
    pub server_setup_signal: &'static Signal<ServerSetup>,
    /// Endpoint identifier for logging
    #[cfg(feature = "std")]
    pub endpoint_id: String,
    /// Endpoint identifier for logging (no_std version)
    #[cfg(not(feature = "std"))]
    pub endpoint_id: heapless::String<64>,
}

impl ClientCallbackData {
    /// Create new callback data with the given signals
    #[cfg(feature = "std")]
    pub fn new(
        status_signal: &'static Signal<Status>,
        server_setup_signal: &'static Signal<ServerSetup>,
        endpoint_id: impl Into<String>,
    ) -> Self {
        Self {
            status_signal,
            server_setup_signal,
            endpoint_id: endpoint_id.into(),
        }
    }

    /// Create new callback data with the given signals (no_std version)
    #[cfg(not(feature = "std"))]
    pub fn new(
        status_signal: &'static Signal<Status>,
        server_setup_signal: &'static Signal<ServerSetup>,
        endpoint_id: &str,
    ) -> Self {
        let mut ep_id = heapless::String::new();
        let _ = ep_id.push_str(endpoint_id);
        Self {
            status_signal,
            server_setup_signal,
            endpoint_id: ep_id,
        }
    }
}

/// Maximum number of tracks per client
const MAX_TRACKS: usize = 16;

/// MoQ Client for pub/sub operations
///
/// The client provides async methods for:
/// - Connecting to a relay server
/// - Publishing tracks
/// - Subscribing to tracks
/// - Managing namespaces
pub struct Client {
    inner: NonNull<ffi::QuicrClient>,
    #[allow(dead_code)]
    callback_data: &'static ClientCallbackData,
    #[allow(dead_code)]
    config: ClientConfig,
    status_signal: &'static Signal<Status>,
    server_setup_signal: &'static Signal<ServerSetup>,
    publish_tracks: Mutex<HeaplessVec<Arc<PublishTrack>, MAX_TRACKS>>,
    subscribe_tracks: Mutex<HeaplessVec<Arc<SubscribeTrack>, MAX_TRACKS>>,
    is_connected: AtomicBool,
}

// SAFETY: The underlying C++ object uses mutex protection for thread safety
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

impl Client {
    /// Create a new client with the given configuration and static signals
    ///
    /// # Arguments
    /// * `config` - Client configuration
    /// * `status_signal` - Static signal for status updates
    /// * `server_setup_signal` - Static signal for server setup info
    /// * `callback_data` - Static callback data storage
    pub fn new(
        config: ClientConfig,
        status_signal: &'static Signal<Status>,
        server_setup_signal: &'static Signal<ServerSetup>,
        callback_data: &'static ClientCallbackData,
    ) -> Result<Self> {
        info!(
            "Creating new client: endpoint_id={}, uri={}",
            config.endpoint_id.as_str(), config.connect_uri.as_str()
        );

        let callbacks = ffi::QuicrClientCallbacks {
            user_data: callback_data as *const _ as *mut c_void,
            on_status_changed: Some(client_status_changed_callback),
            on_server_setup: Some(client_server_setup_callback),
            on_error: Some(client_error_callback),
        };

        let ffi_config = config.to_ffi()?;
        let ffi_client_config = ffi_config.to_ffi_config();

        trace!("Calling FFI quicr_client_create");
        let inner = unsafe { ffi::quicr_client_create(&ffi_client_config, &callbacks) };

        let inner = NonNull::new(inner).ok_or_else(|| {
            error!("Failed to create FFI client - quicr_client_create returned null");
            Error::from_ffi()
        })?;

        info!("Client created successfully");
        Ok(Self {
            inner,
            callback_data,
            config,
            status_signal,
            server_setup_signal,
            publish_tracks: Mutex::new(HeaplessVec::new()),
            subscribe_tracks: Mutex::new(HeaplessVec::new()),
            is_connected: AtomicBool::new(false),
        })
    }

    /// Connect to the relay server with default 30 second timeout
    pub async fn connect(&self) -> Result<()> {
        self.connect_with_timeout(Duration::from_secs(30)).await
    }

    /// Connect to the relay server with a custom timeout
    pub async fn connect_with_timeout(&self, connect_timeout: Duration) -> Result<()> {
        info!("Connecting to relay (timeout: {:?})", connect_timeout);

        trace!("Calling FFI quicr_client_connect");
        let status = unsafe { ffi::quicr_client_connect(self.inner.as_ptr()) };
        let status: Status = status.into();
        debug!("Initial connection status: {:?}", status);

        if status.is_disconnected() {
            error!("Connection failed immediately with status: {:?}", status);
            return Err(Error::connection_failed("immediate failure"));
        }

        // Wait for connection to be ready using signal
        let result = timeout(connect_timeout, async {
            loop {
                let status = self.status_signal.wait().await;
                match status {
                    Status::Ready | Status::Ok => {
                        info!("Connection established successfully");
                        self.is_connected.store(true, Ordering::SeqCst);
                        return Ok(());
                    }
                    Status::Error | Status::Disconnected | Status::IdleTimeout => {
                        error!("Connection failed during handshake");
                        return Err(Error::connection_failed("handshake failed"));
                    }
                    Status::Connecting => {
                        trace!("Still connecting...");
                        continue;
                    }
                    _ => {
                        debug!("Received status during connection: {:?}", status);
                        continue;
                    }
                }
            }
        })
        .await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                error!("Connection timed out after {:?}", connect_timeout);
                Err(Error::Timeout)
            }
        }
    }

    /// Disconnect from the relay server
    pub async fn disconnect(&self) -> Result<()> {
        info!("Disconnecting from relay");
        self.is_connected.store(false, Ordering::SeqCst);

        // Unregister all tracks
        {
            let publish_tracks = self.publish_tracks.lock().await;
            for track in publish_tracks.iter() {
                if track.is_registered() {
                    trace!("Unpublishing track: {:?}", track.track_name());
                    unsafe {
                        ffi::quicr_client_unpublish_track(self.inner.as_ptr(), track.as_ptr());
                    }
                    track.set_registered(false);
                }
            }
        }

        {
            let subscribe_tracks = self.subscribe_tracks.lock().await;
            for track in subscribe_tracks.iter() {
                if track.is_registered() {
                    trace!("Unsubscribing track: {:?}", track.track_name());
                    unsafe {
                        ffi::quicr_client_unsubscribe_track(self.inner.as_ptr(), track.as_ptr());
                    }
                    track.set_registered(false);
                }
            }
        }

        trace!("Calling FFI quicr_client_disconnect");
        unsafe {
            ffi::quicr_client_disconnect(self.inner.as_ptr());
        }

        info!("Disconnected from relay");
        Ok(())
    }

    /// Get the current connection status
    pub fn status(&self) -> Status {
        unsafe { ffi::quicr_client_get_status(self.inner.as_ptr()) }.into()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::SeqCst) && self.status().is_ready()
    }

    /// Wait for server setup information
    pub async fn wait_server_setup(&self) -> ServerSetup {
        self.server_setup_signal.wait().await
    }

    /// Register a publish track with the client
    pub async fn publish_track(&self, track: PublishTrack) -> Result<Arc<PublishTrack>> {
        info!("[PUBLISH_TRACK] Registering publish track: {}", track.track_name());

        if !self.is_connected() {
            warn!("[PUBLISH_TRACK] Cannot publish track - not connected");
            return Err(Error::NotConnected);
        }

        trace!("[PUBLISH_TRACK] Calling FFI quicr_client_publish_track");
        unsafe {
            ffi::quicr_client_publish_track(self.inner.as_ptr(), track.as_ptr());
        }
        track.set_registered(true);

        let track = Arc::new(track);
        let mut tracks = self.publish_tracks.lock().await;
        tracks.push(Arc::clone(&track)).map_err(|_| Error::ResourceExhausted)?;

        Ok(track)
    }

    /// Unpublish a track
    pub async fn unpublish_track(&self, track: &PublishTrack) -> Result<()> {
        debug!("Unpublishing track: {:?}", track.track_name());

        if track.is_registered() {
            trace!("Calling FFI quicr_client_unpublish_track");
            unsafe {
                ffi::quicr_client_unpublish_track(self.inner.as_ptr(), track.as_ptr());
            }
            track.set_registered(false);
        }

        Ok(())
    }

    /// Register a subscribe track with the client
    pub async fn subscribe_track(&self, track: SubscribeTrack) -> Result<Arc<SubscribeTrack>> {
        info!("[SUBSCRIBE_TRACK] Sending SUBSCRIBE for track: {}", track.track_name());

        if !self.is_connected() {
            warn!("[SUBSCRIBE_TRACK] Cannot subscribe to track - not connected");
            return Err(Error::NotConnected);
        }

        trace!("[SUBSCRIBE_TRACK] Calling FFI quicr_client_subscribe_track");
        unsafe {
            ffi::quicr_client_subscribe_track(self.inner.as_ptr(), track.as_ptr());
        }
        track.set_registered(true);

        let track = Arc::new(track);
        let mut tracks = self.subscribe_tracks.lock().await;
        tracks.push(Arc::clone(&track)).map_err(|_| Error::ResourceExhausted)?;

        Ok(track)
    }

    /// Unsubscribe from a track
    pub async fn unsubscribe_track(&self, track: &SubscribeTrack) -> Result<()> {
        debug!("Unsubscribing from track: {:?}", track.track_name());

        if track.is_registered() {
            trace!("Calling FFI quicr_client_unsubscribe_track");
            unsafe {
                ffi::quicr_client_unsubscribe_track(self.inner.as_ptr(), track.as_ptr());
            }
            track.set_registered(false);
        }

        Ok(())
    }

    /// Publish (announce) a namespace
    pub fn publish_namespace(&self, namespace: &TrackNamespace) {
        info!("[ANNOUNCE] Sending ANNOUNCE for namespace: {}", namespace);
        let (ffi_entries, ffi_ns) = namespace.to_ffi();
        let _entries = ffi_entries;
        unsafe {
            ffi::quicr_client_publish_namespace(self.inner.as_ptr(), &ffi_ns);
        }
    }

    /// Unpublish (unannounce) a namespace
    pub fn unpublish_namespace(&self, namespace: &TrackNamespace) {
        let (ffi_entries, ffi_ns) = namespace.to_ffi();
        let _entries = ffi_entries;
        unsafe {
            ffi::quicr_client_unpublish_namespace(self.inner.as_ptr(), &ffi_ns);
        }
    }

    /// Subscribe to a namespace
    pub fn subscribe_namespace(&self, namespace: &TrackNamespace) {
        let (ffi_entries, ffi_ns) = namespace.to_ffi();
        let _entries = ffi_entries;
        unsafe {
            ffi::quicr_client_subscribe_namespace(self.inner.as_ptr(), &ffi_ns);
        }
    }

    /// Unsubscribe from a namespace
    pub fn unsubscribe_namespace(&self, namespace: &TrackNamespace) {
        let (ffi_entries, ffi_ns) = namespace.to_ffi();
        let _entries = ffi_entries;
        unsafe {
            ffi::quicr_client_unsubscribe_namespace(self.inner.as_ptr(), &ffi_ns);
        }
    }

    /// Create and register a publish track with default settings
    pub async fn publish(&self, track_name: FullTrackName) -> Result<Arc<PublishTrack>> {
        let track = PublishTrackBuilder::new(track_name)
            .track_mode(TrackMode::Stream)
            .default_priority(0)
            .default_ttl(1000)
            .build()?;

        self.publish_track(track).await
    }

    /// Create and register a subscribe track, returning a subscription
    pub async fn subscribe(&self, track_name: FullTrackName) -> Result<Subscription> {
        info!("[SUBSCRIBE] Creating subscription for track: {}", track_name);

        if !self.is_connected() {
            warn!("[SUBSCRIBE] Cannot subscribe - not connected");
            return Err(Error::NotConnected);
        }

        let track = SubscribeTrackBuilder::new(track_name)
            .priority(128)
            .group_order(GroupOrder::Ascending)
            .filter_type(FilterType::LargestObject)
            .build()?;

        unsafe {
            ffi::quicr_client_subscribe_track(self.inner.as_ptr(), track.as_ptr());
        }
        track.set_registered(true);

        Ok(Subscription::new(track))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        debug!("Destroying client");
        trace!("Calling FFI quicr_client_destroy");
        unsafe {
            ffi::quicr_client_destroy(self.inner.as_ptr());
        }
    }
}

// ============================================================================
// Callback functions
// ============================================================================

extern "C" fn client_status_changed_callback(
    user_data: *mut c_void,
    status: ffi::QuicrStatus,
) {
    if user_data.is_null() {
        warn!("Client status callback received null user_data");
        return;
    }

    let rust_status: Status = status.into();
    debug!("Client status changed: {:?}", rust_status);

    let data = unsafe { &*(user_data as *const ClientCallbackData) };
    data.status_signal.signal(rust_status);
}

extern "C" fn client_server_setup_callback(
    user_data: *mut c_void,
    moqt_version: u64,
    server_id: *const c_char,
) {
    if user_data.is_null() {
        warn!("Server setup callback received null user_data");
        return;
    }

    let data = unsafe { &*(user_data as *const ClientCallbackData) };

    let server_id_str = if server_id.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(server_id).to_string_lossy().into_owned() }
    };

    let setup = ServerSetup {
        moqt_version,
        server_id: server_id_str,
    };

    data.server_setup_signal.signal(setup);
}

extern "C" fn client_error_callback(
    user_data: *mut c_void,
    error_msg: *const c_char,
) {
    #[cfg(feature = "std")]
    let endpoint_id: String = if user_data.is_null() {
        "<unknown>".to_string()
    } else {
        let data = unsafe { &*(user_data as *const ClientCallbackData) };
        data.endpoint_id.clone()
    };

    #[cfg(not(feature = "std"))]
    let endpoint_id: &str = if user_data.is_null() {
        "<unknown>"
    } else {
        let data = unsafe { &*(user_data as *const ClientCallbackData) };
        data.endpoint_id.as_str()
    };

    let msg = if error_msg.is_null() {
        "<null error message>"
    } else {
        unsafe { CStr::from_ptr(error_msg).to_str().unwrap_or("<invalid utf8>") }
    };

    error!("C++ exception in client {}: {}", endpoint_id, msg);
}

// ============================================================================
// Client Builder
// ============================================================================

/// Builder for creating a client
pub struct ClientBuilder {
    config_builder: crate::config::ClientConfigBuilder,
}

impl ClientBuilder {
    /// Create a new client builder
    pub fn new() -> Self {
        Self {
            config_builder: ClientConfig::builder(),
        }
    }

    /// Set the endpoint ID
    pub fn endpoint_id(mut self, id: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.endpoint_id(id);
        self
    }

    /// Set the connection URI
    pub fn connect_uri(mut self, uri: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.connect_uri(uri);
        self
    }

    /// Set the log level for the libquicr C++ library
    pub fn log_level(mut self, level: crate::config::LogLevel) -> Self {
        self.config_builder = self.config_builder.log_level(level);
        self
    }

    /// Set TLS certificate and key
    pub fn tls(mut self, cert: impl Into<String>, key: impl Into<String>) -> Self {
        self.config_builder = self.config_builder.tls(cert, key);
        self
    }

    /// Set the time queue max duration in milliseconds
    pub fn time_queue_max_duration(mut self, ms: u32) -> Self {
        self.config_builder = self.config_builder.time_queue_max_duration(ms);
        self
    }

    /// Set the tick service sleep delay in microseconds
    pub fn tick_service_sleep_delay_us(mut self, us: u64) -> Self {
        self.config_builder = self.config_builder.tick_service_sleep_delay_us(us);
        self
    }

    /// Build the client configuration
    ///
    /// Use `Client::new()` with the returned config and your static signals
    pub fn build_config(self) -> Result<ClientConfig> {
        self.config_builder.build()
    }

    /// Build the client with static signals (for no_std)
    ///
    /// This method allows creating a client with user-provided static storage
    /// for signals and callback data, which is required for no_std builds.
    ///
    /// # Arguments
    /// * `status_signal` - Static signal for status updates
    /// * `server_setup_signal` - Static signal for server setup info
    /// * `callback_data` - Static callback data storage
    pub fn build_with_signals(
        self,
        status_signal: &'static Signal<Status>,
        server_setup_signal: &'static Signal<ServerSetup>,
        callback_data: &'static ClientCallbackData,
    ) -> Result<Client> {
        let config = self.config_builder.build()?;
        Client::new(config, status_signal, server_setup_signal, callback_data)
    }

    /// Build the client with automatically allocated static signals
    ///
    /// This is a convenience method for std builds that allocates the required
    /// static signals using Box::leak. For no_std builds, use `build_config()`
    /// and `Client::new()` with your own static allocations.
    #[cfg(feature = "std")]
    pub fn build(self) -> Result<Client> {
        let config = self.config_builder.build()?;
        let endpoint_id = config.endpoint_id.clone();

        let status_signal: &'static Signal<Status> =
            Box::leak(Box::new(Signal::new()));
        let server_setup_signal: &'static Signal<ServerSetup> =
            Box::leak(Box::new(Signal::new()));
        let callback_data: &'static ClientCallbackData =
            Box::leak(Box::new(ClientCallbackData {
                status_signal,
                server_setup_signal,
                endpoint_id,
            }));

        Client::new(config, status_signal, server_setup_signal, callback_data)
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_is_ready() {
        assert!(Status::Ready.is_ready());
        assert!(Status::Ok.is_ready());
        assert!(!Status::Connecting.is_ready());
        assert!(!Status::Disconnected.is_ready());
    }

    #[test]
    fn test_status_is_disconnected() {
        assert!(Status::Disconnected.is_disconnected());
        assert!(Status::Error.is_disconnected());
        assert!(Status::IdleTimeout.is_disconnected());
        assert!(!Status::Ready.is_disconnected());
    }
}
