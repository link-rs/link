// SPDX-FileCopyrightText: Copyright (c) 2024 QuicR Contributors
// SPDX-License-Identifier: BSD-2-Clause

#include "quicr_ffi.h"

#include <quicr/client.h>
#include <quicr/publish_track_handler.h>
#include <quicr/subscribe_track_handler.h>

#include <spdlog/spdlog.h>

#include <atomic>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

// =============================================================================
// Exception handling macros for embedded platforms without exception support
// =============================================================================
#ifdef QUICR_FFI_NO_EXCEPTIONS
    #define FFI_TRY_BEGIN
    #define FFI_TRY_END(error_callback, user_data, context)
#else
    #define FFI_TRY_BEGIN try {
    #define FFI_TRY_END(error_callback, user_data, context) \
        } catch (const std::exception& e) { \
            if (error_callback) { \
                error_callback(user_data, e.what()); \
            } \
        } catch (...) { \
            if (error_callback) { \
                error_callback(user_data, "unknown C++ exception in " context); \
            } \
        }
#endif

// Helper to convert FFI log level to spdlog level
static spdlog::level::level_enum to_spdlog_level(QuicrLogLevel level) {
    switch (level) {
        case QUICR_LOG_LEVEL_TRACE: return spdlog::level::trace;
        case QUICR_LOG_LEVEL_DEBUG: return spdlog::level::debug;
        case QUICR_LOG_LEVEL_INFO: return spdlog::level::info;
        case QUICR_LOG_LEVEL_WARN: return spdlog::level::warn;
        case QUICR_LOG_LEVEL_ERROR: return spdlog::level::err;
        case QUICR_LOG_LEVEL_CRITICAL: return spdlog::level::critical;
        case QUICR_LOG_LEVEL_OFF: return spdlog::level::off;
        default: return spdlog::level::info;
    }
}

// Thread-local error storage
thread_local std::string g_last_error;

static void set_error(const std::string& error) {
    g_last_error = error;
}

static void clear_error() {
    g_last_error.clear();
}

// =============================================================================
// Internal Wrapper Classes
// =============================================================================

class FfiPublishTrackHandler : public quicr::PublishTrackHandler {
public:
    FfiPublishTrackHandler(const quicr::FullTrackName& full_track_name,
                           quicr::TrackMode track_mode,
                           uint8_t default_priority,
                           uint32_t default_ttl,
                           const QuicrPublishTrackCallbacks& callbacks)
        : PublishTrackHandler(full_track_name, track_mode, default_priority, default_ttl)
        , callbacks_(callbacks)
    {}

    void StatusChanged(Status status) noexcept override {
        FFI_TRY_BEGIN
            if (callbacks_.on_status_changed) {
                QuicrPublishStatus ffi_status;
                switch (status) {
                    case Status::kOk:
                        ffi_status = QUICR_PUBLISH_STATUS_OK;
                        break;
                    case Status::kNotConnected:
                        ffi_status = QUICR_PUBLISH_STATUS_NOT_CONNECTED;
                        break;
                    case Status::kNotAnnounced:
                        ffi_status = QUICR_PUBLISH_STATUS_NOT_ANNOUNCED;
                        break;
                    case Status::kPendingAnnounceResponse:
                        ffi_status = QUICR_PUBLISH_STATUS_PENDING_ANNOUNCE;
                        break;
                    case Status::kAnnounceNotAuthorized:
                        ffi_status = QUICR_PUBLISH_STATUS_ANNOUNCE_NOT_AUTHORIZED;
                        break;
                    case Status::kNoSubscribers:
                        ffi_status = QUICR_PUBLISH_STATUS_NO_SUBSCRIBERS;
                        break;
                    case Status::kSendingUnannounce:
                        ffi_status = QUICR_PUBLISH_STATUS_SENDING_UNANNOUNCE;
                        break;
                    case Status::kSubscriptionUpdated:
                        ffi_status = QUICR_PUBLISH_STATUS_SUBSCRIPTION_UPDATED;
                        break;
                    case Status::kNewGroupRequested:
                        ffi_status = QUICR_PUBLISH_STATUS_NEW_GROUP_REQUESTED;
                        break;
                    case Status::kPendingPublishOk:
                        ffi_status = QUICR_PUBLISH_STATUS_PENDING_PUBLISH_OK;
                        break;
                    case Status::kPaused:
                        ffi_status = QUICR_PUBLISH_STATUS_PAUSED;
                        break;
                    default:
                        ffi_status = QUICR_PUBLISH_STATUS_OK;
                }
                callbacks_.on_status_changed(callbacks_.user_data, ffi_status);
            }
        FFI_TRY_END(callbacks_.on_error, callbacks_.user_data, "PublishTrackHandler::StatusChanged")
    }

private:
    QuicrPublishTrackCallbacks callbacks_;
};

class FfiSubscribeTrackHandler : public quicr::SubscribeTrackHandler {
public:
    FfiSubscribeTrackHandler(const quicr::FullTrackName& full_track_name,
                             quicr::messages::SubscriberPriority priority,
                             quicr::messages::GroupOrder group_order,
                             quicr::messages::FilterType filter_type,
                             const QuicrSubscribeTrackCallbacks& callbacks)
        : SubscribeTrackHandler(full_track_name, priority, group_order, filter_type)
        , callbacks_(callbacks)
    {}

    void StatusChanged(Status status) noexcept override {
        FFI_TRY_BEGIN
            if (callbacks_.on_status_changed) {
                QuicrSubscribeStatus ffi_status;
                switch (status) {
                    case Status::kOk:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_OK;
                        break;
                    case Status::kNotConnected:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_NOT_CONNECTED;
                        break;
                    case Status::kError:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_ERROR;
                        break;
                    case Status::kNotAuthorized:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_NOT_AUTHORIZED;
                        break;
                    case Status::kNotSubscribed:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
                        break;
                    case Status::kPendingResponse:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_PENDING_RESPONSE;
                        break;
                    case Status::kSendingUnsubscribe:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_SENDING_UNSUBSCRIBE;
                        break;
                    case Status::kPaused:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_PAUSED;
                        break;
                    case Status::kNewGroupRequested:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_NEW_GROUP_REQUESTED;
                        break;
                    default:
                        ffi_status = QUICR_SUBSCRIBE_STATUS_OK;
                }
                callbacks_.on_status_changed(callbacks_.user_data, ffi_status);
            }
        FFI_TRY_END(callbacks_.on_error, callbacks_.user_data, "SubscribeTrackHandler::StatusChanged")
    }

    void ObjectReceived(const quicr::ObjectHeaders& object_headers,
                        quicr::BytesSpan data) noexcept override {
        FFI_TRY_BEGIN
            if (callbacks_.on_object_received) {
                QuicrObjectHeaders ffi_headers;
                ffi_headers.group_id = object_headers.group_id;
                ffi_headers.object_id = object_headers.object_id;
                ffi_headers.subgroup_id = object_headers.subgroup_id;
                ffi_headers.payload_length = object_headers.payload_length;

                switch (object_headers.status) {
                    case quicr::ObjectStatus::kAvailable:
                        ffi_headers.status = QUICR_OBJECT_STATUS_AVAILABLE;
                        break;
                    case quicr::ObjectStatus::kDoesNotExist:
                        ffi_headers.status = QUICR_OBJECT_STATUS_DOES_NOT_EXIST;
                        break;
                    case quicr::ObjectStatus::kEndOfGroup:
                        ffi_headers.status = QUICR_OBJECT_STATUS_END_OF_GROUP;
                        break;
                    case quicr::ObjectStatus::kEndOfTrack:
                        ffi_headers.status = QUICR_OBJECT_STATUS_END_OF_TRACK;
                        break;
                    case quicr::ObjectStatus::kEndOfSubGroup:
                        ffi_headers.status = QUICR_OBJECT_STATUS_END_OF_SUBGROUP;
                        break;
                }

                ffi_headers.has_priority = object_headers.priority.has_value();
                ffi_headers.priority = object_headers.priority.value_or(0);
                ffi_headers.has_ttl = object_headers.ttl.has_value();
                ffi_headers.ttl = object_headers.ttl.value_or(0);
                ffi_headers.has_track_mode = object_headers.track_mode.has_value();
                if (object_headers.track_mode.has_value()) {
                    ffi_headers.track_mode = object_headers.track_mode.value() == quicr::TrackMode::kDatagram
                        ? QUICR_TRACK_MODE_DATAGRAM
                        : QUICR_TRACK_MODE_STREAM;
                }

                callbacks_.on_object_received(
                    callbacks_.user_data,
                    &ffi_headers,
                    data.data(),
                    data.size()
                );
            }
        FFI_TRY_END(callbacks_.on_error, callbacks_.user_data, "SubscribeTrackHandler::ObjectReceived")
    }

private:
    QuicrSubscribeTrackCallbacks callbacks_;
};

class FfiClient : public quicr::Client {
public:
    FfiClient(const quicr::ClientConfig& cfg, const QuicrClientCallbacks& callbacks)
        : quicr::Client(cfg)
        , callbacks_(callbacks)
    {}

    static std::shared_ptr<FfiClient> Create(const quicr::ClientConfig& cfg,
                                              const QuicrClientCallbacks& callbacks) {
        return std::shared_ptr<FfiClient>(new FfiClient(cfg, callbacks));
    }

    void StatusChanged(quicr::Transport::Status status) noexcept override {
        FFI_TRY_BEGIN
            if (callbacks_.on_status_changed) {
                QuicrStatus ffi_status;
                switch (status) {
                    case quicr::Transport::Status::kReady:
                        ffi_status = QUICR_STATUS_READY;
                        break;
                    case quicr::Transport::Status::kConnecting:
                    case quicr::Transport::Status::kPendingServerSetup:
                        // kPendingServerSetup means we've sent CLIENT_SETUP and are waiting
                        // for SERVER_SETUP - still in the connecting phase
                        ffi_status = QUICR_STATUS_CONNECTING;
                        break;
                    case quicr::Transport::Status::kDisconnecting:
                        ffi_status = QUICR_STATUS_DISCONNECTING;
                        break;
                    case quicr::Transport::Status::kNotConnected:
                        ffi_status = QUICR_STATUS_DISCONNECTED;
                        break;
                    default:
                        ffi_status = QUICR_STATUS_ERROR;
                }
                callbacks_.on_status_changed(callbacks_.user_data, ffi_status);
            }
        FFI_TRY_END(callbacks_.on_error, callbacks_.user_data, "Client::StatusChanged")
    }

    void ServerSetupReceived(const quicr::ServerSetupAttributes& attrs) noexcept override {
        FFI_TRY_BEGIN
            if (callbacks_.on_server_setup) {
                callbacks_.on_server_setup(
                    callbacks_.user_data,
                    attrs.moqt_version,
                    attrs.server_id.c_str()
                );
            }
        FFI_TRY_END(callbacks_.on_error, callbacks_.user_data, "Client::ServerSetupReceived")
    }

private:
    QuicrClientCallbacks callbacks_;
};

// =============================================================================
// FFI Wrapper Structures
// =============================================================================

struct QuicrClient {
    std::shared_ptr<FfiClient> client;
    std::mutex mutex;
};

struct QuicrPublishTrackHandler {
    std::shared_ptr<FfiPublishTrackHandler> handler;
};

struct QuicrSubscribeTrackHandler {
    std::shared_ptr<FfiSubscribeTrackHandler> handler;
};

// =============================================================================
// Helper Functions
// =============================================================================

static quicr::TrackNamespace convert_track_namespace(const QuicrTrackNamespace* ns) {
    std::vector<std::vector<uint8_t>> entries;
    for (size_t i = 0; i < ns->num_entries; ++i) {
        entries.emplace_back(ns->entries[i].data, ns->entries[i].data + ns->entries[i].len);
    }
    return quicr::TrackNamespace(entries);
}

static quicr::FullTrackName convert_full_track_name(const QuicrFullTrackName* ftn) {
    quicr::FullTrackName result;
    result.name_space = convert_track_namespace(&ftn->name_space);
    result.name = std::vector<uint8_t>(ftn->name.data, ftn->name.data + ftn->name.len);
    return result;
}

static QuicrPublishObjectStatus convert_publish_object_status(
    quicr::PublishTrackHandler::PublishObjectStatus status) {
    switch (status) {
        case quicr::PublishTrackHandler::PublishObjectStatus::kOk:
            return QUICR_PUBLISH_OBJECT_STATUS_OK;
        case quicr::PublishTrackHandler::PublishObjectStatus::kInternalError:
            return QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR;
        case quicr::PublishTrackHandler::PublishObjectStatus::kNotAuthorized:
            return QUICR_PUBLISH_OBJECT_STATUS_NOT_AUTHORIZED;
        case quicr::PublishTrackHandler::PublishObjectStatus::kNotAnnounced:
            return QUICR_PUBLISH_OBJECT_STATUS_NOT_ANNOUNCED;
        case quicr::PublishTrackHandler::PublishObjectStatus::kNoSubscribers:
            return QUICR_PUBLISH_OBJECT_STATUS_NO_SUBSCRIBERS;
        case quicr::PublishTrackHandler::PublishObjectStatus::kObjectPayloadLengthExceeded:
            return QUICR_PUBLISH_OBJECT_STATUS_PAYLOAD_LENGTH_EXCEEDED;
        case quicr::PublishTrackHandler::PublishObjectStatus::kPreviousObjectTruncated:
            return QUICR_PUBLISH_OBJECT_STATUS_PREVIOUS_OBJECT_TRUNCATED;
        case quicr::PublishTrackHandler::PublishObjectStatus::kNoPreviousObject:
            return QUICR_PUBLISH_OBJECT_STATUS_NO_PREVIOUS_OBJECT;
        case quicr::PublishTrackHandler::PublishObjectStatus::kObjectDataComplete:
            return QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_COMPLETE;
        case quicr::PublishTrackHandler::PublishObjectStatus::kObjectContinuationDataNeeded:
            return QUICR_PUBLISH_OBJECT_STATUS_CONTINUATION_DATA_NEEDED;
        case quicr::PublishTrackHandler::PublishObjectStatus::kObjectDataIncomplete:
            return QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_INCOMPLETE;
        case quicr::PublishTrackHandler::PublishObjectStatus::kObjectDataTooLarge:
            return QUICR_PUBLISH_OBJECT_STATUS_OBJECT_DATA_TOO_LARGE;
        case quicr::PublishTrackHandler::PublishObjectStatus::kPreviousObjectNotCompleteMustStartNewGroup:
            return QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_GROUP;
        case quicr::PublishTrackHandler::PublishObjectStatus::kPreviousObjectNotCompleteMustStartNewTrack:
            return QUICR_PUBLISH_OBJECT_STATUS_MUST_START_NEW_TRACK;
        case quicr::PublishTrackHandler::PublishObjectStatus::kPaused:
            return QUICR_PUBLISH_OBJECT_STATUS_PAUSED;
        default:
            return QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR;
    }
}

// =============================================================================
// Default Config Functions
// =============================================================================

extern "C" void quicr_transport_config_default(QuicrTransportConfig* config) {
    if (!config) return;
    config->tls_cert_filename = nullptr;
    config->tls_key_filename = nullptr;
    config->time_queue_init_queue_size = 1000;
    config->time_queue_max_duration = 2000;
    config->time_queue_bucket_interval = 1;
    config->time_queue_rx_size = 1000;
    config->log_level = QUICR_LOG_LEVEL_OFF;
    config->quic_cwin_minimum = 131072;
    config->quic_wifi_shadow_rtt_us = 20000;
    config->idle_timeout_ms = 30000;
    config->use_reset_wait_strategy = false;
    config->use_bbr = true;
    config->quic_qlog_path = nullptr;
}

extern "C" void quicr_client_config_default(QuicrClientConfig* config) {
    if (!config) return;
    config->endpoint_id = nullptr;
    quicr_transport_config_default(&config->transport_config);
    config->metrics_sample_ms = 5000;
    config->connect_uri = nullptr;
    config->tick_service_sleep_delay_us = 333; // Match libquicr default
}

// =============================================================================
// Client Lifecycle Functions
// =============================================================================

extern "C" QuicrClient* quicr_client_create(
    const QuicrClientConfig* config,
    const QuicrClientCallbacks* callbacks)
{
    clear_error();

    if (!config || !callbacks) {
        set_error("Invalid arguments: config and callbacks must not be null");
        return nullptr;
    }

    try {
        quicr::ClientConfig cfg;

        if (config->endpoint_id) {
            cfg.endpoint_id = config->endpoint_id;
        }

        if (config->connect_uri) {
            cfg.connect_uri = config->connect_uri;
        }

        cfg.metrics_sample_ms = config->metrics_sample_ms;
        cfg.tick_service_sleep_delay_us = config->tick_service_sleep_delay_us;

        // Transport config
        if (config->transport_config.tls_cert_filename) {
            cfg.transport_config.tls_cert_filename = config->transport_config.tls_cert_filename;
        }
        if (config->transport_config.tls_key_filename) {
            cfg.transport_config.tls_key_filename = config->transport_config.tls_key_filename;
        }
        cfg.transport_config.time_queue_init_queue_size = config->transport_config.time_queue_init_queue_size;
        cfg.transport_config.time_queue_max_duration = config->transport_config.time_queue_max_duration;
        cfg.transport_config.time_queue_bucket_interval = config->transport_config.time_queue_bucket_interval;
        cfg.transport_config.time_queue_rx_size = config->transport_config.time_queue_rx_size;
        // Set debug flag for transport based on log level (debug if TRACE or DEBUG)
        cfg.transport_config.debug = (config->transport_config.log_level <= QUICR_LOG_LEVEL_DEBUG);
        cfg.transport_config.quic_cwin_minimum = config->transport_config.quic_cwin_minimum;
        cfg.transport_config.quic_wifi_shadow_rtt_us = config->transport_config.quic_wifi_shadow_rtt_us;
        cfg.transport_config.idle_timeout_ms = config->transport_config.idle_timeout_ms;
        cfg.transport_config.use_reset_wait_strategy = config->transport_config.use_reset_wait_strategy;
        cfg.transport_config.use_bbr = config->transport_config.use_bbr;
        if (config->transport_config.quic_qlog_path) {
            cfg.transport_config.quic_qlog_path = config->transport_config.quic_qlog_path;
        }

        // Set spdlog level based on config
        spdlog::set_level(to_spdlog_level(config->transport_config.log_level));

        auto wrapper = new QuicrClient();
        wrapper->client = FfiClient::Create(cfg, *callbacks);
        return wrapper;
    } catch (const std::exception& e) {
        set_error(e.what());
        return nullptr;
    }
}

extern "C" void quicr_client_destroy(QuicrClient* client) {
    if (client) {
        try {
            std::lock_guard<std::mutex> lock(client->mutex);
            if (client->client) {
                client->client->Disconnect();
            }
        } catch (const std::exception& e) {
            set_error(e.what());
        } catch (...) {
            set_error("unknown C++ exception in quicr_client_destroy");
        }
        try {
            delete client;
        } catch (const std::exception& e) {
            set_error(e.what());
        } catch (...) {
            set_error("unknown C++ exception in quicr_client_destroy destructor");
        }
    }
}

extern "C" QuicrStatus quicr_client_connect(QuicrClient* client) {
    clear_error();

    if (!client || !client->client) {
        set_error("Invalid client handle");
        return QUICR_STATUS_ERROR;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        auto status = client->client->Connect();

        switch (status) {
            case quicr::Transport::Status::kReady:
                return QUICR_STATUS_READY;
            case quicr::Transport::Status::kConnecting:
                return QUICR_STATUS_CONNECTING;
            case quicr::Transport::Status::kNotConnected:
                return QUICR_STATUS_DISCONNECTED;
            default:
                return QUICR_STATUS_ERROR;
        }
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_STATUS_ERROR;
    }
}

extern "C" QuicrStatus quicr_client_disconnect(QuicrClient* client) {
    clear_error();

    if (!client || !client->client) {
        set_error("Invalid client handle");
        return QUICR_STATUS_ERROR;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        auto status = client->client->Disconnect();
        return QUICR_STATUS_DISCONNECTING;
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_STATUS_ERROR;
    }
}

extern "C" QuicrStatus quicr_client_get_status(const QuicrClient* client) {
    if (!client || !client->client) {
        return QUICR_STATUS_ERROR;
    }

    try {
        auto status = client->client->GetStatus();
        switch (status) {
            case quicr::Transport::Status::kReady:
                return QUICR_STATUS_READY;
            case quicr::Transport::Status::kConnecting:
                return QUICR_STATUS_CONNECTING;
            case quicr::Transport::Status::kDisconnecting:
                return QUICR_STATUS_DISCONNECTING;
            case quicr::Transport::Status::kNotConnected:
                return QUICR_STATUS_DISCONNECTED;
            default:
                return QUICR_STATUS_ERROR;
        }
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_STATUS_ERROR;
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_get_status");
        return QUICR_STATUS_ERROR;
    }
}

extern "C" bool quicr_client_poll(QuicrClient* client, uint64_t timeout_ms) {
    if (!client || !client->client) {
        return false;
    }

    try {
        // The libquicr client runs its own event loop internally,
        // so we just sleep for a short time to yield
        std::this_thread::sleep_for(std::chrono::milliseconds(timeout_ms > 0 ? 1 : 0));
        return client->client->GetStatus() == quicr::Transport::Status::kReady;
    } catch (const std::exception& e) {
        set_error(e.what());
        return false;
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_poll");
        return false;
    }
}

// =============================================================================
// Publish Track Functions
// =============================================================================

extern "C" QuicrPublishTrackHandler* quicr_publish_track_create(
    const QuicrFullTrackName* track_name,
    QuicrTrackMode track_mode,
    uint8_t default_priority,
    uint32_t default_ttl,
    const QuicrPublishTrackCallbacks* callbacks)
{
    clear_error();

    if (!track_name || !callbacks) {
        set_error("Invalid arguments");
        return nullptr;
    }

    try {
        auto ftn = convert_full_track_name(track_name);
        auto mode = track_mode == QUICR_TRACK_MODE_DATAGRAM
            ? quicr::TrackMode::kDatagram
            : quicr::TrackMode::kStream;

        auto wrapper = new QuicrPublishTrackHandler();
        wrapper->handler = std::make_shared<FfiPublishTrackHandler>(
            ftn, mode, default_priority, default_ttl, *callbacks
        );
        return wrapper;
    } catch (const std::exception& e) {
        set_error(e.what());
        return nullptr;
    }
}

extern "C" void quicr_publish_track_destroy(QuicrPublishTrackHandler* handler) {
    try {
        delete handler;
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_publish_track_destroy");
    }
}

extern "C" void quicr_client_publish_track(
    QuicrClient* client,
    QuicrPublishTrackHandler* handler)
{
    if (!client || !client->client || !handler || !handler->handler) {
        return;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->PublishTrack(handler->handler);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_publish_track");
    }
}

extern "C" void quicr_client_unpublish_track(
    QuicrClient* client,
    QuicrPublishTrackHandler* handler)
{
    if (!client || !client->client || !handler || !handler->handler) {
        return;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->UnpublishTrack(handler->handler);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_unpublish_track");
    }
}

extern "C" QuicrPublishObjectStatus quicr_publish_track_publish_object(
    QuicrPublishTrackHandler* handler,
    const QuicrObjectHeaders* headers,
    const uint8_t* data,
    size_t data_len)
{
    clear_error();

    if (!handler || !handler->handler || !headers) {
        set_error("Invalid arguments");
        return QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR;
    }

    try {
        quicr::ObjectHeaders obj_headers;
        obj_headers.group_id = headers->group_id;
        obj_headers.object_id = headers->object_id;
        obj_headers.subgroup_id = headers->subgroup_id;
        obj_headers.payload_length = data_len;

        switch (headers->status) {
            case QUICR_OBJECT_STATUS_AVAILABLE:
                obj_headers.status = quicr::ObjectStatus::kAvailable;
                break;
            case QUICR_OBJECT_STATUS_DOES_NOT_EXIST:
                obj_headers.status = quicr::ObjectStatus::kDoesNotExist;
                break;
            case QUICR_OBJECT_STATUS_END_OF_GROUP:
                obj_headers.status = quicr::ObjectStatus::kEndOfGroup;
                break;
            case QUICR_OBJECT_STATUS_END_OF_TRACK:
                obj_headers.status = quicr::ObjectStatus::kEndOfTrack;
                break;
            case QUICR_OBJECT_STATUS_END_OF_SUBGROUP:
                obj_headers.status = quicr::ObjectStatus::kEndOfSubGroup;
                break;
        }

        if (headers->has_priority) {
            obj_headers.priority = headers->priority;
        }
        if (headers->has_ttl) {
            obj_headers.ttl = headers->ttl;
        }
        if (headers->has_track_mode) {
            obj_headers.track_mode = headers->track_mode == QUICR_TRACK_MODE_DATAGRAM
                ? quicr::TrackMode::kDatagram
                : quicr::TrackMode::kStream;
        }

        auto status = handler->handler->PublishObject(
            obj_headers,
            quicr::BytesSpan(data, data_len)
        );

        return convert_publish_object_status(status);
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_PUBLISH_OBJECT_STATUS_INTERNAL_ERROR;
    }
}

extern "C" QuicrPublishStatus quicr_publish_track_get_status(const QuicrPublishTrackHandler* handler) {
    if (!handler || !handler->handler) {
        return QUICR_PUBLISH_STATUS_NOT_CONNECTED;
    }

    try {
        auto status = handler->handler->GetStatus();
        switch (status) {
            case quicr::PublishTrackHandler::Status::kOk:
                return QUICR_PUBLISH_STATUS_OK;
            case quicr::PublishTrackHandler::Status::kNotConnected:
                return QUICR_PUBLISH_STATUS_NOT_CONNECTED;
            case quicr::PublishTrackHandler::Status::kNotAnnounced:
                return QUICR_PUBLISH_STATUS_NOT_ANNOUNCED;
            case quicr::PublishTrackHandler::Status::kPendingAnnounceResponse:
                return QUICR_PUBLISH_STATUS_PENDING_ANNOUNCE;
            case quicr::PublishTrackHandler::Status::kAnnounceNotAuthorized:
                return QUICR_PUBLISH_STATUS_ANNOUNCE_NOT_AUTHORIZED;
            case quicr::PublishTrackHandler::Status::kNoSubscribers:
                return QUICR_PUBLISH_STATUS_NO_SUBSCRIBERS;
            case quicr::PublishTrackHandler::Status::kSendingUnannounce:
                return QUICR_PUBLISH_STATUS_SENDING_UNANNOUNCE;
            case quicr::PublishTrackHandler::Status::kSubscriptionUpdated:
                return QUICR_PUBLISH_STATUS_SUBSCRIPTION_UPDATED;
            case quicr::PublishTrackHandler::Status::kNewGroupRequested:
                return QUICR_PUBLISH_STATUS_NEW_GROUP_REQUESTED;
            case quicr::PublishTrackHandler::Status::kPendingPublishOk:
                return QUICR_PUBLISH_STATUS_PENDING_PUBLISH_OK;
            case quicr::PublishTrackHandler::Status::kPaused:
                return QUICR_PUBLISH_STATUS_PAUSED;
            default:
                return QUICR_PUBLISH_STATUS_NOT_CONNECTED;
        }
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_PUBLISH_STATUS_NOT_CONNECTED;
    } catch (...) {
        set_error("unknown C++ exception in quicr_publish_track_get_status");
        return QUICR_PUBLISH_STATUS_NOT_CONNECTED;
    }
}

extern "C" bool quicr_publish_track_can_publish(const QuicrPublishTrackHandler* handler) {
    if (!handler || !handler->handler) {
        return false;
    }
    try {
        return handler->handler->CanPublish();
    } catch (const std::exception& e) {
        set_error(e.what());
        return false;
    } catch (...) {
        set_error("unknown C++ exception in quicr_publish_track_can_publish");
        return false;
    }
}

extern "C" bool quicr_publish_track_get_track_alias(const QuicrPublishTrackHandler* handler, uint64_t* out_track_alias) {
    if (!handler || !handler->handler || !out_track_alias) {
        return false;
    }
    try {
        auto alias = handler->handler->GetTrackAlias();
        if (alias.has_value()) {
            *out_track_alias = alias.value();
            return true;
        }
        return false;
    } catch (const std::exception& e) {
        set_error(e.what());
        return false;
    } catch (...) {
        set_error("unknown C++ exception in quicr_publish_track_get_track_alias");
        return false;
    }
}

// =============================================================================
// Subscribe Track Functions
// =============================================================================

extern "C" QuicrSubscribeTrackHandler* quicr_subscribe_track_create(
    const QuicrFullTrackName* track_name,
    uint8_t priority,
    QuicrGroupOrder group_order,
    QuicrFilterType filter_type,
    const QuicrSubscribeTrackCallbacks* callbacks)
{
    clear_error();

    if (!track_name || !callbacks) {
        set_error("Invalid arguments");
        return nullptr;
    }

    try {
        auto ftn = convert_full_track_name(track_name);

        quicr::messages::GroupOrder order = group_order == QUICR_GROUP_ORDER_ASCENDING
            ? quicr::messages::GroupOrder::kAscending
            : quicr::messages::GroupOrder::kDescending;

        quicr::messages::FilterType filter;
        switch (filter_type) {
            case QUICR_FILTER_TYPE_NEXT_GROUP_START:
                filter = quicr::messages::FilterType::kNextGroupStart;
                break;
            case QUICR_FILTER_TYPE_LARGEST_OBJECT:
                filter = quicr::messages::FilterType::kLargestObject;
                break;
            case QUICR_FILTER_TYPE_ABSOLUTE_START:
                filter = quicr::messages::FilterType::kAbsoluteStart;
                break;
            case QUICR_FILTER_TYPE_ABSOLUTE_RANGE:
                filter = quicr::messages::FilterType::kAbsoluteRange;
                break;
            default:
                filter = quicr::messages::FilterType::kLargestObject;
                break;
        }

        auto wrapper = new QuicrSubscribeTrackHandler();
        wrapper->handler = std::make_shared<FfiSubscribeTrackHandler>(
            ftn, priority, order, filter, *callbacks
        );
        return wrapper;
    } catch (const std::exception& e) {
        set_error(e.what());
        return nullptr;
    }
}

extern "C" void quicr_subscribe_track_destroy(QuicrSubscribeTrackHandler* handler) {
    try {
        delete handler;
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_subscribe_track_destroy");
    }
}

extern "C" void quicr_client_subscribe_track(
    QuicrClient* client,
    QuicrSubscribeTrackHandler* handler)
{
    if (!client || !client->client || !handler || !handler->handler) {
        return;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->SubscribeTrack(handler->handler);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_subscribe_track");
    }
}

extern "C" void quicr_client_unsubscribe_track(
    QuicrClient* client,
    QuicrSubscribeTrackHandler* handler)
{
    if (!client || !client->client || !handler || !handler->handler) {
        return;
    }

    try {
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->UnsubscribeTrack(handler->handler);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_unsubscribe_track");
    }
}

extern "C" QuicrSubscribeStatus quicr_subscribe_track_get_status(const QuicrSubscribeTrackHandler* handler) {
    if (!handler || !handler->handler) {
        return QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
    }

    try {
        auto status = handler->handler->GetStatus();
        switch (status) {
            case quicr::SubscribeTrackHandler::Status::kOk:
                return QUICR_SUBSCRIBE_STATUS_OK;
            case quicr::SubscribeTrackHandler::Status::kNotConnected:
                return QUICR_SUBSCRIBE_STATUS_NOT_CONNECTED;
            case quicr::SubscribeTrackHandler::Status::kError:
                return QUICR_SUBSCRIBE_STATUS_ERROR;
            case quicr::SubscribeTrackHandler::Status::kNotAuthorized:
                return QUICR_SUBSCRIBE_STATUS_NOT_AUTHORIZED;
            case quicr::SubscribeTrackHandler::Status::kNotSubscribed:
                return QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
            case quicr::SubscribeTrackHandler::Status::kPendingResponse:
                return QUICR_SUBSCRIBE_STATUS_PENDING_RESPONSE;
            case quicr::SubscribeTrackHandler::Status::kSendingUnsubscribe:
                return QUICR_SUBSCRIBE_STATUS_SENDING_UNSUBSCRIBE;
            case quicr::SubscribeTrackHandler::Status::kPaused:
                return QUICR_SUBSCRIBE_STATUS_PAUSED;
            case quicr::SubscribeTrackHandler::Status::kNewGroupRequested:
                return QUICR_SUBSCRIBE_STATUS_NEW_GROUP_REQUESTED;
            default:
                return QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
        }
    } catch (const std::exception& e) {
        set_error(e.what());
        return QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
    } catch (...) {
        set_error("unknown C++ exception in quicr_subscribe_track_get_status");
        return QUICR_SUBSCRIBE_STATUS_NOT_SUBSCRIBED;
    }
}

extern "C" void quicr_subscribe_track_pause(QuicrSubscribeTrackHandler* handler) {
    try {
        if (handler && handler->handler) {
            handler->handler->Pause();
        }
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_subscribe_track_pause");
    }
}

extern "C" void quicr_subscribe_track_resume(QuicrSubscribeTrackHandler* handler) {
    try {
        if (handler && handler->handler) {
            handler->handler->Resume();
        }
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_subscribe_track_resume");
    }
}

// =============================================================================
// Namespace Functions
// =============================================================================

extern "C" void quicr_client_publish_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace)
{
    if (!client || !client->client || !track_namespace) {
        return;
    }

    try {
        auto ns = convert_track_namespace(track_namespace);
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->PublishNamespace(ns);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_publish_namespace");
    }
}

extern "C" void quicr_client_unpublish_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace)
{
    if (!client || !client->client || !track_namespace) {
        return;
    }

    try {
        auto ns = convert_track_namespace(track_namespace);
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->PublishNamespaceDone(ns);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_unpublish_namespace");
    }
}

extern "C" void quicr_client_subscribe_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace)
{
    if (!client || !client->client || !track_namespace) {
        return;
    }

    try {
        auto ns = convert_track_namespace(track_namespace);
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->SubscribeNamespace(ns);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_subscribe_namespace");
    }
}

extern "C" void quicr_client_unsubscribe_namespace(
    QuicrClient* client,
    const QuicrTrackNamespace* track_namespace)
{
    if (!client || !client->client || !track_namespace) {
        return;
    }

    try {
        auto ns = convert_track_namespace(track_namespace);
        std::lock_guard<std::mutex> lock(client->mutex);
        client->client->UnsubscribeNamespace(ns);
    } catch (const std::exception& e) {
        set_error(e.what());
    } catch (...) {
        set_error("unknown C++ exception in quicr_client_unsubscribe_namespace");
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

extern "C" const char* quicr_version(void) {
    return "0.1.0";
}

extern "C" const char* quicr_last_error(void) {
    if (g_last_error.empty()) {
        return nullptr;
    }
    return g_last_error.c_str();
}

extern "C" void quicr_clear_error(void) {
    clear_error();
}
