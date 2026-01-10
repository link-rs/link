// SPDX-FileCopyrightText: Copyright (c) 2024 QuicR Contributors
// SPDX-License-Identifier: BSD-2-Clause

#ifndef QUICR_FFI_H
#define QUICR_FFI_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// =============================================================================
// Opaque Handle Types
// =============================================================================

typedef struct QuicrClient QuicrClient;
typedef struct QuicrPublishTrackHandler QuicrPublishTrackHandler;
typedef struct QuicrSubscribeTrackHandler QuicrSubscribeTrackHandler;

// =============================================================================
// Status and Error Types
// =============================================================================

typedef enum {
    QUICR_STATUS_OK = 0,
    QUICR_STATUS_CONNECTING,
    QUICR_STATUS_READY,
    QUICR_STATUS_DISCONNECTING,
    QUICR_STATUS_DISCONNECTED,
    QUICR_STATUS_ERROR,
    QUICR_STATUS_IDLE_TIMEOUT,
    QUICR_STATUS_SHUTDOWN,
} QuicrStatus;

typedef enum {
    QUICR_PUBLISH_STATUS_OK = 0,
    QUICR_PUBLISH_STATUS_NOT_CONNECTED,
    QUICR_PUBLISH_STATUS_NOT_ANNOUNCED,
    QUICR_PUBLISH_STATUS_PENDING_ANNOUNCE,
    QUICR_PUBLISH_STATUS_ANNOUNCE_NOT_AUTHORIZED,
    QUICR_PUBLISH_STATUS_NO_SUBSCRIBERS,
    QUICR_PUBLISH_STATUS_SENDING_UNANNOUNCE,
    QUICR_PUBLISH_STATUS_SUBSCRIPTION_UPDATED,
    QUICR_PUBLISH_STATUS_NEW_GROUP_REQUESTED,
    QUICR_PUBLISH_STATUS_PENDING_PUBLISH_OK,
    QUICR_PUBLISH_STATUS_PAUSED,
} QuicrPublishStatus;

typedef enum {
    QUICR_SUBSCRIBE_STATUS_OK = 0,
    QUICR_SUBSCRIBE_STATUS_NOT_CONNECTED,
    QUICR_SUBSCRIBE_STATUS_ERROR,
    QUICR_SUBSCRIBE_STATUS_NOT_AUTHORIZED,
    QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED,
    QUICR_SUBSCRIBE_STATUS_PENDING_RESPONSE,
    QUICR_SUBSCRIBE_STATUS_SENDING_UNSUBSCRIBE,
    QUICR_SUBSCRIBE_STATUS_PAUSED,
    QUICR_SUBSCRIBE_STATUS_NEW_GROUP_REQUESTED,
    QUICR_SUBSCRIBE_STATUS_CANCELLED,
    QUICR_SUBSCRIBE_STATUS_DONE_BY_FIN,
    QUICR_SUBSCRIBE_STATUS_DONE_BY_RESET,
} QuicrSubscribeStatus;

typedef enum {
    QUICR_PUBLISH_OBJECT_STATUS_OK = 0,
    QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR,
    QUICR_PUBLISH_OBJECT_STATUS_NOT_AUTHORIZED,
    QUICR_PUBLISH_OBJECT_STATUS_NOT_ANNOUNCED,
    QUICR_PUBLISH_OBJECT_STATUS_NO_SUBSCRIBERS,
    QUICR_PUBLISH_OBJECT_STATUS_PAYLOAD_LENGTH_EXCEEDED,
    QUICR_PUBLISH_OBJECT_STATUS_PREVIOUS_OBJECT_TRUNCATED,
    QUICR_PUBLISH_OBJECT_STATUS_NO_PREVIOUS_OBJECT,
    QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_COMPLETE,
    QUICR_PUBLISH_OBJECT_STATUS_CONTINUATION_DATA_NEEDED,
    QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_INCOMPLETE,
    QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_TOO_LARGE,
    QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_GROUP,
    QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_TRACK,
    QUICR_PUBLISH_OBJECT_STATUS_PAUSED,
    QUICR_PUBLISH_OBJECT_STATUS_PENDING_PUBLISH_OK,
} QuicrPublishObjectStatus;

typedef enum {
    QUICR_OBJECT_STATUS_AVAILABLE = 0,
    QUICR_OBJECT_STATUS_DOES_NOT_EXIST = 1,
    QUICR_OBJECT_STATUS_END_OF_GROUP = 3,
    QUICR_OBJECT_STATUS_END_OF_TRACK = 4,
    QUICR_OBJECT_STATUS_END_OF_SUBGROUP = 5,
} QuicrObjectStatus;

typedef enum {
    QUICR_TRACK_MODE_DATAGRAM = 0,
    QUICR_TRACK_MODE_STREAM = 1,
} QuicrTrackMode;

typedef enum {
    QUICR_GROUP_ORDER_ASCENDING = 0,
    QUICR_GROUP_ORDER_DESCENDING = 1,
} QuicrGroupOrder;

typedef enum {
    QUICR_FILTER_TYPE_NEXT_GROUP_START = 1,
    QUICR_FILTER_TYPE_LARGEST_OBJECT = 2,
    QUICR_FILTER_TYPE_ABSOLUTE_START = 3,
    QUICR_FILTER_TYPE_ABSOLUTE_RANGE = 4,
} QuicrFilterType;

typedef enum {
    QUICR_LOG_LEVEL_TRACE = 0,
    QUICR_LOG_LEVEL_DEBUG = 1,
    QUICR_LOG_LEVEL_INFO = 2,
    QUICR_LOG_LEVEL_WARN = 3,
    QUICR_LOG_LEVEL_ERROR = 4,
    QUICR_LOG_LEVEL_CRITICAL = 5,
    QUICR_LOG_LEVEL_OFF = 6,
} QuicrLogLevel;

// =============================================================================
// Configuration Structures
// =============================================================================

typedef struct {
    const char* tls_cert_filename;
    const char* tls_key_filename;
    uint32_t time_queue_init_queue_size;
    uint32_t time_queue_max_duration;
    uint32_t time_queue_bucket_interval;
    uint32_t time_queue_rx_size;
    QuicrLogLevel log_level;
    uint64_t quic_cwin_minimum;
    uint32_t quic_wifi_shadow_rtt_us;
    uint64_t idle_timeout_ms;
    bool use_reset_wait_strategy;
    bool use_bbr;
    const char* quic_qlog_path;
} QuicrTransportConfig;

typedef struct {
    const char* endpoint_id;
    QuicrTransportConfig transport_config;
    uint64_t metrics_sample_ms;
    const char* connect_uri;
} QuicrClientConfig;

// =============================================================================
// Track Name Structures
// =============================================================================

typedef struct {
    const uint8_t* data;
    size_t len;
} QuicrBytes;

typedef struct {
    QuicrBytes* entries;
    size_t num_entries;
} QuicrTrackNamespace;

typedef struct {
    QuicrTrackNamespace name_space;
    QuicrBytes name;
} QuicrFullTrackName;

// =============================================================================
// Object Headers
// =============================================================================

typedef struct {
    uint64_t group_id;
    uint64_t object_id;
    uint64_t subgroup_id;
    uint64_t payload_length;
    QuicrObjectStatus status;
    uint8_t priority;
    bool has_priority;
    uint16_t ttl;
    bool has_ttl;
    QuicrTrackMode track_mode;
    bool has_track_mode;
} QuicrObjectHeaders;

// =============================================================================
// Callback Function Pointer Types
// =============================================================================

// Client callbacks
typedef void (*QuicrStatusChangedCallback)(void* user_data, QuicrStatus status);
typedef void (*QuicrServerSetupCallback)(void* user_data, uint64_t moqt_version, const char* server_id);

// Publish track callbacks
typedef void (*QuicrPublishStatusChangedCallback)(void* user_data, QuicrPublishStatus status);

// Subscribe track callbacks
typedef void (*QuicrSubscribeStatusChangedCallback)(void* user_data, QuicrSubscribeStatus status);
typedef void (*QuicrObjectReceivedCallback)(
    void* user_data,
    const QuicrObjectHeaders* headers,
    const uint8_t* data,
    size_t data_len
);

// Error callback - called when C++ exceptions are caught at FFI boundary
typedef void (*QuicrErrorCallback)(void* user_data, const char* error_msg);

// =============================================================================
// Client Callbacks Structure
// =============================================================================

typedef struct {
    void* user_data;
    QuicrStatusChangedCallback on_status_changed;
    QuicrServerSetupCallback on_server_setup;
    QuicrErrorCallback on_error;
} QuicrClientCallbacks;

typedef struct {
    void* user_data;
    QuicrPublishStatusChangedCallback on_status_changed;
    QuicrErrorCallback on_error;
} QuicrPublishTrackCallbacks;

typedef struct {
    void* user_data;
    QuicrSubscribeStatusChangedCallback on_status_changed;
    QuicrObjectReceivedCallback on_object_received;
    QuicrErrorCallback on_error;
} QuicrSubscribeTrackCallbacks;

// =============================================================================
// Default Config Initialization
// =============================================================================

void quicr_transport_config_default(QuicrTransportConfig* config);
void quicr_client_config_default(QuicrClientConfig* config);

// =============================================================================
// Client Lifecycle Functions
// =============================================================================

/// Creates a new QuicR client with the given configuration and callbacks.
/// Returns NULL on error.
QuicrClient* quicr_client_create(
    const QuicrClientConfig* config,
    const QuicrClientCallbacks* callbacks
);

/// Destroys a QuicR client and frees all resources.
void quicr_client_destroy(QuicrClient* client);

/// Connects the client to the relay.
/// Returns the resulting status.
QuicrStatus quicr_client_connect(QuicrClient* client);

/// Disconnects the client from the relay.
/// Returns the resulting status.
QuicrStatus quicr_client_disconnect(QuicrClient* client);

/// Gets the current status of the client.
QuicrStatus quicr_client_get_status(const QuicrClient* client);

/// Polls the client for events. Should be called regularly.
/// Returns true if there are more events to process.
bool quicr_client_poll(QuicrClient* client, uint64_t timeout_ms);

// =============================================================================
// Publish Track Functions
// =============================================================================

/// Creates a publish track handler.
/// Returns NULL on error.
QuicrPublishTrackHandler* quicr_publish_track_create(
    const QuicrFullTrackName* track_name,
    QuicrTrackMode track_mode,
    uint8_t default_priority,
    uint32_t default_ttl,
    const QuicrPublishTrackCallbacks* callbacks
);

/// Destroys a publish track handler.
void quicr_publish_track_destroy(QuicrPublishTrackHandler* handler);

/// Registers a publish track with the client.
void quicr_client_publish_track(
    QuicrClient* client,
    QuicrPublishTrackHandler* handler
);

/// Unregisters a publish track from the client.
void quicr_client_unpublish_track(
    QuicrClient* client,
    QuicrPublishTrackHandler* handler
);

/// Publishes an object to the track.
QuicrPublishObjectStatus quicr_publish_track_publish_object(
    QuicrPublishTrackHandler* handler,
    const QuicrObjectHeaders* headers,
    const uint8_t* data,
    size_t data_len
);

/// Gets the current publish status.
QuicrPublishStatus quicr_publish_track_get_status(const QuicrPublishTrackHandler* handler);

/// Checks if publishing is currently allowed.
bool quicr_publish_track_can_publish(const QuicrPublishTrackHandler* handler);

// =============================================================================
// Subscribe Track Functions
// =============================================================================

/// Creates a subscribe track handler.
/// Returns NULL on error.
QuicrSubscribeTrackHandler* quicr_subscribe_track_create(
    const QuicrFullTrackName* track_name,
    uint8_t priority,
    QuicrGroupOrder group_order,
    QuicrFilterType filter_type,
    const QuicrSubscribeTrackCallbacks* callbacks
);

/// Destroys a subscribe track handler.
void quicr_subscribe_track_destroy(QuicrSubscribeTrackHandler* handler);

/// Subscribes to a track.
void quicr_client_subscribe_track(
    QuicrClient* client,
    QuicrSubscribeTrackHandler* handler
);

/// Unsubscribes from a track.
void quicr_client_unsubscribe_track(
    QuicrClient* client,
    QuicrSubscribeTrackHandler* handler
);

/// Gets the current subscribe status.
QuicrSubscribeStatus quicr_subscribe_track_get_status(const QuicrSubscribeTrackHandler* handler);

/// Pauses receiving data.
void quicr_subscribe_track_pause(QuicrSubscribeTrackHandler* handler);

/// Resumes receiving data.
void quicr_subscribe_track_resume(QuicrSubscribeTrackHandler* handler);

// =============================================================================
// Track Namespace Functions
// =============================================================================

/// Publishes (announces) a namespace.
void quicr_client_publish_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace
);

/// Unpublishes (unannounces) a namespace.
void quicr_client_unpublish_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace
);

/// Subscribes to a namespace.
void quicr_client_subscribe_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace
);

/// Unsubscribes from a namespace.
void quicr_client_unsubscribe_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace
);

// =============================================================================
// Utility Functions
// =============================================================================

/// Returns the version string of the library.
const char* quicr_version(void);

/// Returns the last error message, or NULL if no error.
const char* quicr_last_error(void);

/// Clears the last error message.
void quicr_clear_error(void);

#ifdef __cplusplus
}
#endif

#endif // QUICR_FFI_H
