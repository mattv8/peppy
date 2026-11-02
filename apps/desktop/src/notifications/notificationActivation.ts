import type { NotificationActivation } from "@peppy/generated/notification-activation";

export interface ActivationTransport {
  subscribe(listener: () => void): Promise<() => void>;
  peek(): Promise<NotificationActivation | null>;
  ack(request: NotificationActivation): Promise<boolean>;
}

export const nativeNotificationActivation: ActivationTransport = {
  async subscribe(listener) {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return () => {};
    const { getCurrentWebviewWindow } = await import("@tauri-apps/api/webviewWindow");
    return getCurrentWebviewWindow().listen("peppy://notification-activation", listener);
  },
  async peek() {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return null;
    return (await import("@tauri-apps/api/core")).invoke<NotificationActivation | null>("peek_notification_activation");
  },
  async ack({ requestId, sessionGeneration }) {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return false;
    return (await import("@tauri-apps/api/core")).invoke<boolean>("ack_notification_activation", { requestId, sessionGeneration });
  },
};

export interface ActivationContext {
  validate(): Promise<boolean>;
  complete(): Promise<void>;
  fail(error: unknown): void;
}

export function createNotificationActivationDrain(options: {
  transport: ActivationTransport;
  ready(): boolean;
  handle(request: NotificationActivation, context: ActivationContext): Promise<void>;
  invalidate?(): void;
  error?(error: unknown): void;
}) {
  let disposed = false;
  let subscribed = false;
  let subscribing = false;
  let running = false;
  let dirty = false;
  let revision = 0;
  let handled: NotificationActivation | null = null;
  let retryNeeded = false;
  let unsubscribe = () => {};
  const fail = (error: unknown) => {
    if (disposed) return;
    retryNeeded = true;
    options.error?.(error);
  };
  const same = (a: NotificationActivation | null, b: NotificationActivation) =>
    a?.requestId === b.requestId && a.sessionGeneration === b.sessionGeneration;
  const run = async () => {
    if (running || !subscribed || disposed) return;
    running = true;
    try {
      while (dirty && !disposed) {
        dirty = false;
        retryNeeded = false;
        if (!options.ready()) continue;
        const version = revision;
        const request = await options.transport.peek();
        const current = () => !disposed && version === revision && options.ready();
        if (!request || !current()) continue;
        if (same(handled, request)) {
          if (!await options.transport.ack(request)) wake();
          continue;
        }
        options.invalidate?.();
        const validate = async () => {
          if (!current()) return false;
          const pending = await options.transport.peek();
          if (!current()) return false;
          if (!same(pending, request)) { wake(); return false; }
          return true;
        };
        let completed = false;
        await options.handle(request, {
          validate,
          fail(error) { if (current()) fail(error); },
          async complete() {
            if (completed) return;
            completed = true;
            try {
              if (!await validate()) { completed = false; return; }
            } catch (error) { completed = false; fail(error); return; }
            handled = request;
            try {
              if (!await options.transport.ack(request)) wake();
            } catch (error) { fail(error); }
          },
        });
      }
    } catch (error) {
      fail(error);
      // Native pending state survives transient failures; the next readiness/hint retries.
    } finally {
      running = false;
      if (dirty && !disposed) void run();
    }
  };
  const wake = () => {
    if (disposed) return;
    revision += 1;
    dirty = true;
    // Availability hints also accompany ordinary state updates. A completed
    // focus stays visible until peek confirms another request or the UI resets.
    if (!options.ready()) options.invalidate?.();
    if (!subscribed) { subscribe(); return; }
    void run();
  };
  const subscribe = () => {
    if (subscribing || subscribed || disposed) return;
    subscribing = true;
    void options.transport.subscribe(wake).then(stop => {
      if (disposed) { stop(); return; }
      unsubscribe = stop;
      subscribed = true;
      wake();
    }).catch(fail).finally(() => { subscribing = false; });
  };
  subscribe();
  return {
    wake,
    reset() {
      options.invalidate?.();
      wake();
    },
    retry() { if (retryNeeded) wake(); },
    dispose() {
      disposed = true;
      revision += 1;
      unsubscribe();
      options.invalidate?.();
    },
  };
}
