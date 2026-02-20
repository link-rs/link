/**
 * Type definitions for moqws.js
 */

export interface ConnectedEvent {
    moqt_version: number;
    server_id: string;
}

export interface DisconnectedEvent {
    reason: string;
}

export interface ErrorEvent {
    code: string;
    message: string;
    id?: number;
}

export interface SubscribedEvent {
    id: number;
}

export interface SubscribeErrorEvent {
    id: number;
    code: string;
    message: string;
}

export interface SubscriptionEndedEvent {
    id: number;
    reason: string;
}

export interface PublishedEvent {
    id: number;
}

export interface PublishErrorEvent {
    id: number;
    code: string;
    message: string;
}

export interface ObjectEvent {
    id: number;
    group_id: number;
    object_id: number;
    payload: ArrayBuffer;
}

export interface CloseEvent {
    code: number;
    reason: string;
}

export interface PublishOptions {
    /** 'datagram' (default, low-latency) or 'stream' (reliable) */
    trackMode?: 'datagram' | 'stream';
    /** Default priority (0-255, default: 0) */
    priority?: number;
    /** Default TTL in milliseconds (default: 1000) */
    ttl?: number;
}

export type MoqwsEventMap = {
    'open': {};
    'close': CloseEvent;
    'error': ErrorEvent;
    'connected': ConnectedEvent;
    'disconnected': DisconnectedEvent;
    'subscribed': SubscribedEvent;
    'subscribe_error': SubscribeErrorEvent;
    'subscription_ended': SubscriptionEndedEvent;
    'published': PublishedEvent;
    'publish_error': PublishErrorEvent;
    'object': ObjectEvent;
};

/**
 * MOQWS WebSocket client (EventTarget-based)
 */
export declare class MoqwsClient extends EventTarget {
    /** WebSocket server URL */
    wsUrl: string;
    /** Whether connected to WebSocket server */
    connected: boolean;
    /** Whether connected to MOQT relay */
    relayConnected: boolean;

    constructor(wsUrl: string);

    /** Connect to the MOQWS WebSocket server */
    connect(): Promise<void>;

    /** Disconnect from the WebSocket server */
    disconnect(): void;

    /** Connect to a MOQT relay server */
    connectToRelay(relayUrl: string, endpointId?: string): Promise<ConnectedEvent>;

    /** Disconnect from the MOQT relay */
    disconnectFromRelay(): void;

    /** Subscribe to a MOQT track */
    subscribe(id: number, namespace: string[], track: string): Promise<void>;

    /** Unsubscribe from a track */
    unsubscribe(id: number): void;

    /** Announce a publish track */
    publishAnnounce(id: number, namespace: string[], track: string, options?: PublishOptions): Promise<void>;

    /** Publish an object to an announced track */
    publish(id: number, groupId: number, objectId: number, payload: ArrayBuffer | Uint8Array): void;

    /** Stop publishing to a track */
    publishEnd(id: number): void;

    /** Generate a unique ID for subscriptions/publish tracks */
    nextId(): number;
}

/**
 * Simple callback-based wrapper for MoqwsClient
 */
export declare class MoqwsSimpleClient {
    /** The underlying MoqwsClient */
    client: MoqwsClient;

    constructor(wsUrl: string);

    /** Register a callback for an event */
    on<K extends keyof MoqwsEventMap>(event: K, callback: (detail: MoqwsEventMap[K]) => void): void;

    /** Remove a callback */
    off<K extends keyof MoqwsEventMap>(event: K, callback: (detail: MoqwsEventMap[K]) => void): void;

    /** Connect to the MOQWS WebSocket server */
    connect(): Promise<void>;

    /** Disconnect from the WebSocket server */
    disconnect(): void;

    /** Connect to a MOQT relay server */
    connectToRelay(relayUrl: string, endpointId?: string): Promise<ConnectedEvent>;

    /** Disconnect from the MOQT relay */
    disconnectFromRelay(): void;

    /** Subscribe to a MOQT track */
    subscribe(id: number, namespace: string[], track: string): Promise<void>;

    /** Unsubscribe from a track */
    unsubscribe(id: number): void;

    /** Announce a publish track */
    publishAnnounce(id: number, namespace: string[], track: string, options?: PublishOptions): Promise<void>;

    /** Publish an object to an announced track */
    publish(id: number, groupId: number, objectId: number, payload: ArrayBuffer | Uint8Array): void;

    /** Stop publishing to a track */
    publishEnd(id: number): void;

    /** Generate a unique ID for subscriptions/publish tracks */
    nextId(): number;
}
