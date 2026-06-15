import type { WsEvent } from '../types';

export type WsState = 'connected' | 'reconnecting';

/**
 * Auto-reconnecting WebSocket client for the map server.
 *
 * Exponential backoff: 100ms → 200ms → … → 5s cap.
 * Fires `onStateChange('reconnecting')` on disconnect and
 * `onStateChange('connected')` on (re)connect so the UI can
 * show a reconnecting indicator.
 */
export class WsConnection {
  private ws: WebSocket | null = null;
  private token: string;
  private onEvent: (event: WsEvent) => void;
  private onStateChange: (state: WsState) => void;
  private reconnectDelay = 100;
  private maxReconnectDelay = 5000;

  constructor(
    token: string,
    onEvent: (event: WsEvent) => void,
    onStateChange: (state: WsState) => void,
  ) {
    this.token = token;
    this.onEvent = onEvent;
    this.onStateChange = onStateChange;
    this.connect();
  }

  private connect(): void {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws?token=${encodeURIComponent(this.token)}`;
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.reconnectDelay = 100;
      this.onStateChange('connected');
    };

    this.ws.onmessage = (e) => {
      try {
        const event: WsEvent = JSON.parse(e.data);
        this.onEvent(event);
      } catch {
        /* ignore malformed messages */
      }
    };

    this.ws.onclose = () => {
      this.onStateChange('reconnecting');
      setTimeout(() => this.connect(), this.reconnectDelay);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }
}
