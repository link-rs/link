/**
 * MOQWS - WebSocket client for MOQT bridge
 *
 * Provides access to MOQT (Media over QUIC Transport) relay servers via WebSocket.
 *
 * @example
 * const client = new MoqwsClient('ws://localhost:8765');
 *
 * client.on('connected', ({ moqt_version, server_id }) => {
 *   console.log('Connected to MOQT relay');
 * });
 *
 * client.on('object', ({ id, group_id, object_id, payload }) => {
 *   console.log(`Received object: group=${group_id}, object=${object_id}, size=${payload.byteLength}`);
 * });
 *
 * await client.connect();
 * await client.connectToRelay('moqt://relay.example.com:4433');
 * await client.subscribe(1, ['audio', 'room1'], 'mic');
 */

/**
 * MOQWS WebSocket client
 */
class MoqwsClient extends EventTarget {
  /**
   * @param {string} wsUrl - WebSocket server URL (e.g., 'ws://localhost:8765')
   */
  constructor(wsUrl) {
    super();
    this.wsUrl = wsUrl;
    this.ws = null;
    this.connected = false;
    this.relayConnected = false;
    this._pendingBinary = null;
    this._nextId = 1;
  }

  /**
   * Connect to the MOQWS WebSocket server
   * @returns {Promise<void>}
   */
  connect() {
    return new Promise((resolve, reject) => {
      this.ws = new WebSocket(this.wsUrl);
      this.ws.binaryType = 'arraybuffer';

      this.ws.onopen = () => {
        this.connected = true;
        this._emit('open');
        resolve();
      };

      this.ws.onerror = (err) => {
        this._emit('error', { error: err });
        reject(err);
      };

      this.ws.onclose = (event) => {
        this.connected = false;
        this.relayConnected = false;
        this._emit('close', { code: event.code, reason: event.reason });
      };

      this.ws.onmessage = (event) => {
        this._handleMessage(event.data);
      };
    });
  }

  /**
   * Disconnect from the WebSocket server
   */
  disconnect() {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
      this.connected = false;
      this.relayConnected = false;
    }
  }

  /**
   * Connect to a MOQT relay server
   * @param {string} relayUrl - MOQT relay URL (e.g., 'moqt://relay.example.com:4433')
   * @param {string} [endpointId] - Optional client identifier
   * @returns {Promise<{moqt_version: number, server_id: string}>}
   */
  connectToRelay(relayUrl, endpointId) {
    return new Promise((resolve, reject) => {
      const onConnected = (e) => {
        this.relayConnected = true;
        this.removeEventListener('connected', onConnected);
        this.removeEventListener('error', onError);
        resolve(e.detail);
      };

      const onError = (e) => {
        this.removeEventListener('connected', onConnected);
        this.removeEventListener('error', onError);
        reject(new Error(e.detail.message || 'Connection failed'));
      };

      this.addEventListener('connected', onConnected);
      this.addEventListener('error', onError);

      this._send({
        type: 'connect',
        relay_url: relayUrl,
        endpoint_id: endpointId,
      });
    });
  }

  /**
   * Disconnect from the MOQT relay
   */
  disconnectFromRelay() {
    this._send({ type: 'disconnect' });
    this.relayConnected = false;
  }

  /**
   * Subscribe to a MOQT track
   * @param {number} id - Subscription ID (client-assigned)
   * @param {string[]} namespace - Track namespace segments
   * @param {string} track - Track name
   * @returns {Promise<void>}
   */
  subscribe(id, namespace, track) {
    return new Promise((resolve, reject) => {
      const onSubscribed = (e) => {
        if (e.detail.id === id) {
          this.removeEventListener('subscribed', onSubscribed);
          this.removeEventListener('subscribe_error', onError);
          resolve();
        }
      };

      const onError = (e) => {
        if (e.detail.id === id) {
          this.removeEventListener('subscribed', onSubscribed);
          this.removeEventListener('subscribe_error', onError);
          reject(new Error(e.detail.message || 'Subscribe failed'));
        }
      };

      this.addEventListener('subscribed', onSubscribed);
      this.addEventListener('subscribe_error', onError);

      this._send({
        type: 'subscribe',
        id,
        namespace,
        track,
      });
    });
  }

  /**
   * Unsubscribe from a track
   * @param {number} id - Subscription ID
   */
  unsubscribe(id) {
    this._send({ type: 'unsubscribe', id });
  }

  /**
   * Announce a publish track
   * @param {number} id - Publish track ID (client-assigned)
   * @param {string[]} namespace - Track namespace segments
   * @param {string} track - Track name
   * @param {Object} [options] - Optional settings
   * @param {string} [options.trackMode='datagram'] - 'datagram' or 'stream'
   * @param {number} [options.priority=0] - Default priority (0-255)
   * @param {number} [options.ttl=1000] - Default TTL in milliseconds
   * @returns {Promise<void>}
   */
  publishAnnounce(id, namespace, track, options = {}) {
    return new Promise((resolve, reject) => {
      const onPublished = (e) => {
        if (e.detail.id === id) {
          this.removeEventListener('published', onPublished);
          this.removeEventListener('publish_error', onError);
          resolve();
        }
      };

      const onError = (e) => {
        if (e.detail.id === id) {
          this.removeEventListener('published', onPublished);
          this.removeEventListener('publish_error', onError);
          reject(new Error(e.detail.message || 'Publish announce failed'));
        }
      };

      this.addEventListener('published', onPublished);
      this.addEventListener('publish_error', onError);

      this._send({
        type: 'publish_announce',
        id,
        namespace,
        track,
        track_mode: options.trackMode,
        priority: options.priority,
        ttl: options.ttl,
      });
    });
  }

  /**
   * Publish an object to an announced track
   * @param {number} id - Publish track ID
   * @param {number} groupId - Object group ID
   * @param {number} objectId - Object sequence number
   * @param {ArrayBuffer|Uint8Array} payload - Object payload
   */
  publish(id, groupId, objectId, payload) {
    // Send JSON header
    this._send({
      type: 'publish',
      id,
      group_id: groupId,
      object_id: objectId,
    });

    // Send binary payload
    if (payload instanceof ArrayBuffer) {
      this.ws.send(payload);
    } else if (payload instanceof Uint8Array) {
      this.ws.send(payload.buffer.slice(
        payload.byteOffset,
        payload.byteOffset + payload.byteLength
      ));
    } else {
      throw new Error('Payload must be ArrayBuffer or Uint8Array');
    }
  }

  /**
   * Stop publishing to a track
   * @param {number} id - Publish track ID
   */
  publishEnd(id) {
    this._send({ type: 'publish_end', id });
  }

  /**
   * Generate a unique ID for subscriptions/publish tracks
   * @returns {number}
   */
  nextId() {
    return this._nextId++;
  }

  // ============================================================================
  // Private methods
  // ============================================================================

  _send(obj) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('WebSocket not connected');
    }
    this.ws.send(JSON.stringify(obj));
  }

  _handleMessage(data) {
    if (data instanceof ArrayBuffer) {
      // Binary frame - associate with pending object header
      if (this._pendingBinary) {
        const { id, group_id, object_id } = this._pendingBinary;
        this._pendingBinary = null;
        this._emit('object', {
          id,
          group_id,
          object_id,
          payload: data,
        });
      }
      return;
    }

    // Text frame - JSON message
    let msg;
    try {
      msg = JSON.parse(data);
    } catch (e) {
      console.error('Failed to parse MOQWS message:', e);
      return;
    }

    switch (msg.type) {
      case 'connected':
        this._emit('connected', {
          moqt_version: msg.moqt_version,
          server_id: msg.server_id,
        });
        break;

      case 'disconnected':
        this.relayConnected = false;
        this._emit('disconnected', { reason: msg.reason });
        break;

      case 'error':
        this._emit('error', {
          code: msg.code,
          message: msg.message,
          id: msg.id,
        });
        break;

      case 'subscribed':
        this._emit('subscribed', { id: msg.id });
        break;

      case 'subscribe_error':
        this._emit('subscribe_error', {
          id: msg.id,
          code: msg.code,
          message: msg.message,
        });
        break;

      case 'subscription_ended':
        this._emit('subscription_ended', {
          id: msg.id,
          reason: msg.reason,
        });
        break;

      case 'published':
        this._emit('published', { id: msg.id });
        break;

      case 'publish_error':
        this._emit('publish_error', {
          id: msg.id,
          code: msg.code,
          message: msg.message,
        });
        break;

      case 'object':
        // Store for association with following binary frame
        this._pendingBinary = {
          id: msg.id,
          group_id: msg.group_id,
          object_id: msg.object_id,
        };
        break;

      default:
        console.warn('Unknown MOQWS message type:', msg.type);
    }
  }

  _emit(type, detail = {}) {
    this.dispatchEvent(new CustomEvent(type, { detail }));
  }
}

/**
 * Simple callback-based wrapper for MoqwsClient
 */
class MoqwsSimpleClient {
  /**
   * @param {string} wsUrl - WebSocket server URL
   */
  constructor(wsUrl) {
    this.client = new MoqwsClient(wsUrl);
    this._callbacks = {};
  }

  /**
   * Register a callback for an event
   * @param {string} event - Event name
   * @param {Function} callback - Callback function
   */
  on(event, callback) {
    if (!this._callbacks[event]) {
      this._callbacks[event] = [];
      this.client.addEventListener(event, (e) => {
        this._callbacks[event].forEach(cb => cb(e.detail));
      });
    }
    this._callbacks[event].push(callback);
  }

  /**
   * Remove a callback
   * @param {string} event - Event name
   * @param {Function} callback - Callback function
   */
  off(event, callback) {
    if (this._callbacks[event]) {
      this._callbacks[event] = this._callbacks[event].filter(cb => cb !== callback);
    }
  }

  // Delegate all other methods to the underlying client
  connect() { return this.client.connect(); }
  disconnect() { return this.client.disconnect(); }
  connectToRelay(url, endpointId) { return this.client.connectToRelay(url, endpointId); }
  disconnectFromRelay() { return this.client.disconnectFromRelay(); }
  subscribe(id, namespace, track) { return this.client.subscribe(id, namespace, track); }
  unsubscribe(id) { return this.client.unsubscribe(id); }
  publishAnnounce(id, namespace, track, options) { return this.client.publishAnnounce(id, namespace, track, options); }
  publish(id, groupId, objectId, payload) { return this.client.publish(id, groupId, objectId, payload); }
  publishEnd(id) { return this.client.publishEnd(id); }
  nextId() { return this.client.nextId(); }
}

// Export for different module systems
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { MoqwsClient, MoqwsSimpleClient };
} else if (typeof window !== 'undefined') {
  window.MoqwsClient = MoqwsClient;
  window.MoqwsSimpleClient = MoqwsSimpleClient;
}
