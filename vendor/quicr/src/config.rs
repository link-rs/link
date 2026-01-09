//! Configuration types for the quicr client

#[cfg(not(feature = "std"))]
extern crate alloc;

use crate::error::{Error, Result};

#[cfg(not(feature = "std"))]
use alloc::ffi::CString;
#[cfg(feature = "std")]
use std::ffi::CString;

#[cfg(not(feature = "std"))]
use embassy_time::Duration;
#[cfg(feature = "std")]
use std::time::Duration;

#[cfg(not(feature = "std"))]
use alloc::string::String;
#[cfg(feature = "std")]
use std::string::String;

// Conditional logging - use defmt for no_std with defmt-logging feature
#[cfg(all(feature = "defmt-logging", not(feature = "std")))]
use defmt::{debug, trace};
#[cfg(all(not(feature = "defmt-logging"), feature = "std"))]
use log::{debug, trace};
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! debug {
    ($($arg:tt)*) => {};
}
#[cfg(all(not(feature = "defmt-logging"), not(feature = "std")))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}

/// Log level for the libquicr C++ library
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum LogLevel {
    /// Most verbose logging
    Trace = 0,
    /// Debug level logging
    Debug = 1,
    /// Informational messages
    Info = 2,
    /// Warning messages
    Warn = 3,
    /// Error messages
    Error = 4,
    /// Critical errors only
    Critical = 5,
    /// No logging
    #[default]
    Off = 6,
}

impl LogLevel {
    /// Returns the default log level for the current build profile.
    /// Debug builds default to `Debug`, release builds default to `Off`.
    #[must_use]
    pub fn default_for_build() -> Self {
        if cfg!(debug_assertions) {
            LogLevel::Debug
        } else {
            LogLevel::Off
        }
    }
}

/// Transport layer configuration
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// TLS certificate file path
    pub tls_cert_filename: Option<String>,
    /// TLS private key file path
    pub tls_key_filename: Option<String>,
    /// Initial queue size for time-based queue
    pub time_queue_init_queue_size: u32,
    /// Max duration for time queue in milliseconds
    pub time_queue_max_duration: u32,
    /// Bucket interval for time queue in milliseconds
    pub time_queue_bucket_interval: u32,
    /// Receive queue size
    pub time_queue_rx_size: u32,
    /// Log level for the libquicr C++ library
    pub log_level: LogLevel,
    /// QUIC congestion window minimum size
    pub quic_cwin_minimum: u64,
    /// QUIC WiFi shadow RTT in microseconds
    pub quic_wifi_shadow_rtt_us: u32,
    /// Idle timeout for transport connections
    pub idle_timeout: Duration,
    /// Use reset-and-wait strategy for congestion control
    pub use_reset_wait_strategy: bool,
    /// Use BBR congestion control (true) or NewReno (false)
    pub use_bbr: bool,
    /// Path for QUIC log files
    pub quic_qlog_path: Option<String>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        // Use smaller defaults for embedded platforms
        #[cfg(target_os = "espidf")]
        let (queue_size, rx_size, cwin_min) = (100, 100, 32768);
        #[cfg(not(target_os = "espidf"))]
        let (queue_size, rx_size, cwin_min) = (1000, 1000, 131072);

        Self {
            tls_cert_filename: None,
            tls_key_filename: None,
            time_queue_init_queue_size: queue_size,
            time_queue_max_duration: 2000,
            time_queue_bucket_interval: 1,
            time_queue_rx_size: rx_size,
            log_level: LogLevel::default_for_build(),
            quic_cwin_minimum: cwin_min,
            quic_wifi_shadow_rtt_us: 20000,
            idle_timeout: Duration::from_secs(30),
            use_reset_wait_strategy: false,
            use_bbr: true,
            quic_qlog_path: None,
        }
    }
}

impl TransportConfig {
    /// Create a new transport configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for transport configuration
    pub fn builder() -> TransportConfigBuilder {
        TransportConfigBuilder::default()
    }
}

/// Builder for TransportConfig
#[derive(Debug, Default)]
pub struct TransportConfigBuilder {
    config: TransportConfig,
}

impl TransportConfigBuilder {
    /// Set the TLS certificate file path
    pub fn tls_cert(mut self, path: impl Into<String>) -> Self {
        self.config.tls_cert_filename = Some(path.into());
        self
    }

    /// Set the TLS private key file path
    pub fn tls_key(mut self, path: impl Into<String>) -> Self {
        self.config.tls_key_filename = Some(path.into());
        self
    }

    /// Set the log level for the libquicr C++ library
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.config.log_level = level;
        self
    }

    /// Set the idle timeout
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.config.idle_timeout = timeout;
        self
    }

    /// Enable or disable BBR congestion control
    pub fn use_bbr(mut self, enabled: bool) -> Self {
        self.config.use_bbr = enabled;
        self
    }

    /// Set the QUIC log path
    pub fn quic_qlog_path(mut self, path: impl Into<String>) -> Self {
        self.config.quic_qlog_path = Some(path.into());
        self
    }

    /// Build the transport configuration
    pub fn build(self) -> TransportConfig {
        self.config
    }
}

/// Client configuration
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Unique endpoint identifier
    pub endpoint_id: String,
    /// URI to connect to (e.g., "moq://relay.example.com:4433")
    pub connect_uri: String,
    /// Transport layer configuration
    pub transport: TransportConfig,
    /// Metrics sampling interval
    pub metrics_sample_interval: Duration,
}

impl ClientConfig {
    /// Create a new client configuration
    pub fn new(endpoint_id: impl Into<String>, connect_uri: impl Into<String>) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            connect_uri: connect_uri.into(),
            transport: TransportConfig::default(),
            metrics_sample_interval: Duration::from_secs(5),
        }
    }

    /// Create a builder for client configuration
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    /// Convert to FFI representation
    pub(crate) fn to_ffi(&self) -> Result<FfiClientConfig<'_>> {
        let endpoint_id = CString::new(self.endpoint_id.as_str())
            .map_err(|_| Error::InvalidArgument("endpoint_id contains null bytes".into()))?;

        let connect_uri = CString::new(self.connect_uri.as_str())
            .map_err(|_| Error::InvalidArgument("connect_uri contains null bytes".into()))?;

        let tls_cert = self
            .transport
            .tls_cert_filename
            .as_ref()
            .map(|s| CString::new(s.as_str()))
            .transpose()
            .map_err(|_| Error::InvalidArgument("tls_cert contains null bytes".into()))?;

        let tls_key = self
            .transport
            .tls_key_filename
            .as_ref()
            .map(|s| CString::new(s.as_str()))
            .transpose()
            .map_err(|_| Error::InvalidArgument("tls_key contains null bytes".into()))?;

        let qlog_path = self
            .transport
            .quic_qlog_path
            .as_ref()
            .map(|s| CString::new(s.as_str()))
            .transpose()
            .map_err(|_| Error::InvalidArgument("qlog_path contains null bytes".into()))?;

        Ok(FfiClientConfig {
            endpoint_id,
            connect_uri,
            tls_cert,
            tls_key,
            qlog_path,
            transport: &self.transport,
            metrics_sample_ms: self.metrics_sample_interval.as_millis() as u64,
        })
    }
}

/// Holds CStrings for FFI lifetime management
pub(crate) struct FfiClientConfig<'a> {
    pub endpoint_id: CString,
    pub connect_uri: CString,
    pub tls_cert: Option<CString>,
    pub tls_key: Option<CString>,
    pub qlog_path: Option<CString>,
    pub transport: &'a TransportConfig,
    pub metrics_sample_ms: u64,
}

impl<'a> FfiClientConfig<'a> {
    pub fn to_ffi_config(&self) -> crate::ffi::QuicrClientConfig {
        crate::ffi::QuicrClientConfig {
            endpoint_id: self.endpoint_id.as_ptr(),
            connect_uri: self.connect_uri.as_ptr(),
            metrics_sample_ms: self.metrics_sample_ms,
            transport_config: crate::ffi::QuicrTransportConfig {
                tls_cert_filename: self
                    .tls_cert
                    .as_ref()
                    .map(|c| c.as_ptr())
                    .unwrap_or(core::ptr::null()),
                tls_key_filename: self
                    .tls_key
                    .as_ref()
                    .map(|c| c.as_ptr())
                    .unwrap_or(core::ptr::null()),
                time_queue_init_queue_size: self.transport.time_queue_init_queue_size,
                time_queue_max_duration: self.transport.time_queue_max_duration,
                time_queue_bucket_interval: self.transport.time_queue_bucket_interval,
                time_queue_rx_size: self.transport.time_queue_rx_size,
                log_level: self.transport.log_level as u32,
                quic_cwin_minimum: self.transport.quic_cwin_minimum,
                quic_wifi_shadow_rtt_us: self.transport.quic_wifi_shadow_rtt_us,
                idle_timeout_ms: self.transport.idle_timeout.as_millis() as u64,
                use_reset_wait_strategy: self.transport.use_reset_wait_strategy,
                use_bbr: self.transport.use_bbr,
                quic_qlog_path: self
                    .qlog_path
                    .as_ref()
                    .map(|c| c.as_ptr())
                    .unwrap_or(core::ptr::null()),
            },
        }
    }
}

/// Builder for ClientConfig
#[derive(Debug, Default)]
pub struct ClientConfigBuilder {
    endpoint_id: Option<String>,
    connect_uri: Option<String>,
    transport: TransportConfig,
    metrics_sample_interval: Duration,
}

impl ClientConfigBuilder {
    /// Set the endpoint ID
    pub fn endpoint_id(mut self, id: impl Into<String>) -> Self {
        self.endpoint_id = Some(id.into());
        self
    }

    /// Set the connection URI
    pub fn connect_uri(mut self, uri: impl Into<String>) -> Self {
        self.connect_uri = Some(uri.into());
        self
    }

    /// Set the transport configuration
    pub fn transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Set the metrics sampling interval
    pub fn metrics_sample_interval(mut self, interval: Duration) -> Self {
        self.metrics_sample_interval = interval;
        self
    }

    /// Set the log level for the libquicr C++ library
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.transport.log_level = level;
        self
    }

    /// Set TLS certificate and key files
    pub fn tls(mut self, cert: impl Into<String>, key: impl Into<String>) -> Self {
        self.transport.tls_cert_filename = Some(cert.into());
        self.transport.tls_key_filename = Some(key.into());
        self
    }

    /// Build the client configuration
    pub fn build(self) -> Result<ClientConfig> {
        debug!("Building client configuration");

        let endpoint_id = self
            .endpoint_id
            .ok_or_else(|| Error::ConfigError("endpoint_id is required".into()))?;

        let connect_uri = self
            .connect_uri
            .ok_or_else(|| Error::ConfigError("connect_uri is required".into()))?;

        let config = ClientConfig {
            endpoint_id: endpoint_id.clone(),
            connect_uri: connect_uri.clone(),
            transport: self.transport,
            metrics_sample_interval: {
                #[cfg(feature = "std")]
                let is_zero = self.metrics_sample_interval.is_zero();
                #[cfg(not(feature = "std"))]
                let is_zero = self.metrics_sample_interval.as_ticks() == 0;
                if is_zero {
                    Duration::from_secs(5)
                } else {
                    self.metrics_sample_interval
                }
            },
        };

        trace!(
            "Config built: endpoint_id={}, uri={}, log_level={:?}, bbr={}",
            endpoint_id,
            connect_uri,
            config.transport.log_level,
            config.transport.use_bbr
        );

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_config_builder() {
        let config = ClientConfig::builder()
            .endpoint_id("test-client")
            .connect_uri("moqt://localhost:4433")
            .log_level(LogLevel::Debug)
            .build()
            .unwrap();

        assert_eq!(config.endpoint_id, "test-client");
        assert_eq!(config.connect_uri, "moqt://localhost:4433");
        assert_eq!(config.transport.log_level, LogLevel::Debug);
    }

    #[test]
    fn test_config_builder_missing_endpoint() {
        let result = ClientConfig::builder()
            .connect_uri("moqt://localhost:4433")
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_transport_config_defaults() {
        let config = TransportConfig::default();
        // Debug builds default to Debug, release to Off
        let expected = LogLevel::default_for_build();
        assert_eq!(config.log_level, expected);
        assert!(config.use_bbr);
        assert_eq!(config.idle_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_log_level_values() {
        // Ensure log levels match FFI enum values
        assert_eq!(LogLevel::Trace as u32, 0);
        assert_eq!(LogLevel::Debug as u32, 1);
        assert_eq!(LogLevel::Info as u32, 2);
        assert_eq!(LogLevel::Warn as u32, 3);
        assert_eq!(LogLevel::Error as u32, 4);
        assert_eq!(LogLevel::Critical as u32, 5);
        assert_eq!(LogLevel::Off as u32, 6);
    }
}
