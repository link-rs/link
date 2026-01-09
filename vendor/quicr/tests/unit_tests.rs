//! Unit tests for quicr.rs
//!
//! Note: Integration tests requiring a running relay server are in a separate file.

use quicr::*;

mod track_tests {
    use super::*;

    #[test]
    fn test_track_namespace_from_strings() {
        let ns = TrackNamespace::from_strings(&["chat", "room1"]);
        assert_eq!(ns.len(), 2);
        assert!(!ns.is_empty());
    }

    #[test]
    fn test_track_namespace_from_bytes() {
        let ns = TrackNamespace::from_bytes(&[b"chat", b"room1"]);
        assert_eq!(ns.len(), 2);
    }

    #[test]
    fn test_empty_namespace() {
        let ns = TrackNamespace::new();
        assert!(ns.is_empty());
        assert_eq!(ns.len(), 0);
    }

    #[test]
    fn test_namespace_push() {
        let mut ns = TrackNamespace::new();
        ns.push("chat");
        ns.push("room1");
        assert_eq!(ns.len(), 2);
    }

    #[test]
    fn test_namespace_prefix() {
        let ns1 = TrackNamespace::from_strings(&["chat"]);
        let ns2 = TrackNamespace::from_strings(&["chat", "room1"]);
        let ns3 = TrackNamespace::from_strings(&["video", "room1"]);

        assert!(ns1.is_prefix_of(&ns2));
        assert!(!ns2.is_prefix_of(&ns1));
        assert!(!ns1.is_prefix_of(&ns3));
    }

    #[test]
    fn test_namespace_display() {
        let ns = TrackNamespace::from_strings(&["chat", "room1"]);
        assert_eq!(ns.to_string(), "chat/room1");
    }

    #[test]
    fn test_full_track_name() {
        let track = FullTrackName::from_strings(&["chat", "room1"], "messages");
        assert_eq!(track.namespace.len(), 2);
        assert_eq!(track.to_string(), "chat/room1/messages");
    }

    #[test]
    fn test_full_track_name_equality() {
        let track1 = FullTrackName::from_strings(&["chat", "room1"], "messages");
        let track2 = FullTrackName::from_strings(&["chat", "room1"], "messages");
        let track3 = FullTrackName::from_strings(&["chat", "room2"], "messages");

        assert_eq!(track1, track2);
        assert_ne!(track1, track3);
    }
}

mod object_tests {
    use super::*;
    use quicr::object::{GroupOrder, FilterType, TrackMode};

    #[test]
    fn test_object_headers_new() {
        let headers = ObjectHeaders::new(1, 2);
        assert_eq!(headers.group_id, 1);
        assert_eq!(headers.object_id, 2);
        assert_eq!(headers.subgroup_id, 0);
        assert_eq!(headers.status, ObjectStatus::Available);
        assert!(headers.priority.is_none());
        assert!(headers.ttl.is_none());
    }

    #[test]
    fn test_object_headers_builder() {
        let headers = ObjectHeaders::builder()
            .group_id(5)
            .object_id(10)
            .subgroup_id(1)
            .status(ObjectStatus::Available)
            .priority(50)
            .ttl(1000)
            .track_mode(TrackMode::Stream)
            .build(256);

        assert_eq!(headers.group_id, 5);
        assert_eq!(headers.object_id, 10);
        assert_eq!(headers.subgroup_id, 1);
        assert_eq!(headers.payload_length, 256);
        assert_eq!(headers.priority, Some(50));
        assert_eq!(headers.ttl, Some(1000));
        assert_eq!(headers.track_mode, Some(TrackMode::Stream));
    }

    #[test]
    fn test_object_status_values() {
        assert_eq!(ObjectStatus::Available as u8, 0);
        assert_eq!(ObjectStatus::DoesNotExist as u8, 1);
        assert_eq!(ObjectStatus::EndOfGroup as u8, 3);
        assert_eq!(ObjectStatus::EndOfTrack as u8, 4);
        assert_eq!(ObjectStatus::EndOfSubGroup as u8, 5);
    }

    #[test]
    fn test_track_mode_default() {
        assert_eq!(TrackMode::default(), TrackMode::Stream);
    }

    #[test]
    fn test_group_order_default() {
        assert_eq!(GroupOrder::default(), GroupOrder::Ascending);
    }

    #[test]
    fn test_filter_type_default() {
        assert_eq!(FilterType::default(), FilterType::LargestObject);
    }
}

mod config_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_transport_config_default() {
        let config = TransportConfig::default();
        // Debug builds default to Debug, release to Off
        let expected = quicr::LogLevel::default_for_build();
        assert_eq!(config.log_level, expected);
        assert!(config.use_bbr);
        assert_eq!(config.idle_timeout, Duration::from_secs(30));
        assert_eq!(config.quic_cwin_minimum, 131072);
    }

    #[test]
    fn test_transport_config_builder() {
        let config = TransportConfig::builder()
            .log_level(quicr::LogLevel::Debug)
            .use_bbr(false)
            .idle_timeout(Duration::from_secs(60))
            .tls_cert("/path/to/cert.pem")
            .tls_key("/path/to/key.pem")
            .build();

        assert_eq!(config.log_level, quicr::LogLevel::Debug);
        assert!(!config.use_bbr);
        assert_eq!(config.idle_timeout, Duration::from_secs(60));
        assert_eq!(config.tls_cert_filename, Some("/path/to/cert.pem".into()));
        assert_eq!(config.tls_key_filename, Some("/path/to/key.pem".into()));
    }

    #[test]
    fn test_client_config_builder_success() {
        let config = ClientConfig::builder()
            .endpoint_id("test-client")
            .connect_uri("moqt://localhost:4433")
            .log_level(quicr::LogLevel::Debug)
            .build()
            .unwrap();

        assert_eq!(config.endpoint_id, "test-client");
        assert_eq!(config.connect_uri, "moqt://localhost:4433");
        assert_eq!(config.transport.log_level, quicr::LogLevel::Debug);
    }

    #[test]
    fn test_client_config_builder_missing_endpoint() {
        let result = ClientConfig::builder()
            .connect_uri("moqt://localhost:4433")
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_client_config_builder_missing_uri() {
        let result = ClientConfig::builder()
            .endpoint_id("test-client")
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_client_config_with_tls() {
        let config = ClientConfig::builder()
            .endpoint_id("secure-client")
            .connect_uri("moqt://secure.example.com:4433")
            .tls("/path/cert.pem", "/path/key.pem")
            .build()
            .unwrap();

        assert_eq!(
            config.transport.tls_cert_filename,
            Some("/path/cert.pem".into())
        );
        assert_eq!(
            config.transport.tls_key_filename,
            Some("/path/key.pem".into())
        );
    }
}

mod error_tests {
    use super::*;

    #[test]
    fn test_error_is_recoverable() {
        assert!(Error::Timeout.is_recoverable());
        assert!(Error::NotConnected.is_recoverable());
        assert!(Error::ChannelClosed.is_recoverable());
        assert!(!Error::NotAuthorized.is_recoverable());
        assert!(!Error::ConfigError("test".into()).is_recoverable());
    }

    #[test]
    fn test_error_display() {
        let error = Error::NotConnected;
        assert_eq!(error.to_string(), "client is not connected");

        let error = Error::Timeout;
        assert_eq!(error.to_string(), "operation timed out");
    }
}

mod status_tests {
    use quicr::client::Status;
    use quicr::subscribe::SubscribeStatus;

    #[test]
    fn test_client_status_is_ready() {
        assert!(Status::Ready.is_ready());
        assert!(Status::Ok.is_ready());
        assert!(!Status::Connecting.is_ready());
        assert!(!Status::Disconnected.is_ready());
    }

    #[test]
    fn test_client_status_is_disconnected() {
        assert!(Status::Disconnected.is_disconnected());
        assert!(Status::Error.is_disconnected());
        assert!(Status::IdleTimeout.is_disconnected());
        assert!(!Status::Ready.is_disconnected());
    }

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

mod publish_object_status_tests {
    use quicr::publish::PublishObjectStatus;

    #[test]
    fn test_is_ok() {
        assert!(PublishObjectStatus::Ok.is_ok());
        assert!(PublishObjectStatus::ObjectDataComplete.is_ok());
        assert!(!PublishObjectStatus::InternalError.is_ok());
        assert!(!PublishObjectStatus::NoSubscribers.is_ok());
    }

    #[test]
    fn test_can_continue() {
        assert!(PublishObjectStatus::Ok.can_continue());
        assert!(PublishObjectStatus::ObjectDataComplete.can_continue());
        assert!(PublishObjectStatus::ContinuationDataNeeded.can_continue());
        assert!(PublishObjectStatus::NoSubscribers.can_continue());
        assert!(!PublishObjectStatus::InternalError.can_continue());
        assert!(!PublishObjectStatus::NotAuthorized.can_continue());
    }
}
