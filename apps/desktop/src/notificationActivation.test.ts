import { describe, expect, it, vi } from "vitest";
import type { NotificationActivation } from "@peppy/generated/notification-activation";
import { createNotificationActivationDrain, nativeNotificationActivation, type ActivationContext } from "./notificationActivation";

const request = (requestId: string): NotificationActivation => ({ requestId, sessionGeneration: "1", destination: { kind: "main" } });
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

describe("notification activation drain", () => {
  it("preserves handled focus on duplicate and empty readiness hints but resets for lock or another identity", async () => {
    let pending: NotificationActivation | null = request("a");
    let ready = true;
    const invalidate = vi.fn();
    const handle = vi.fn(async (_: NotificationActivation, context: ActivationContext) => { await context.complete(); });
    const drain = createNotificationActivationDrain({
      transport: { subscribe: async () => () => {}, peek: async () => pending, ack: async () => true },
      ready: () => ready, handle, invalidate,
    });
    await settle();
    invalidate.mockClear();
    drain.wake();
    await settle();
    expect(invalidate).not.toHaveBeenCalled();
    expect(handle).toHaveBeenCalledOnce();
    pending = null;
    drain.wake();
    await settle();
    expect(invalidate).not.toHaveBeenCalled();
    pending = { ...request("a"), sessionGeneration: "new-session" };
    drain.wake();
    await settle();
    expect(invalidate).toHaveBeenCalledOnce();
    expect(handle).toHaveBeenCalledTimes(2);
    invalidate.mockClear();
    ready = false;
    drain.wake();
    expect(invalidate).toHaveBeenCalledOnce();
    invalidate.mockClear();
    ready = true;
    drain.reset();
    expect(invalidate).toHaveBeenCalledOnce();
    drain.dispose();
  });
  it.each(["lock", "identity"])("drops a peek that finishes after %s changes", async change => {
    let ready = true;
    let release!: (value: NotificationActivation | null) => void;
    const peek = vi.fn().mockImplementationOnce(() => new Promise(resolve => { release = resolve; })).mockResolvedValue(null);
    const handle = vi.fn();
    const drain = createNotificationActivationDrain({ transport: { subscribe: async () => () => {}, peek, ack: async () => true }, ready: () => ready, handle });
    await settle();
    if (change === "lock") ready = false;
    drain.wake();
    release(request("old"));
    await settle();
    expect(handle).not.toHaveBeenCalled();
    drain.dispose();
  });
  it("recovers a failed subscription before peeking and reports the failure", async () => {
    const failure = new Error("registration failed");
    const subscribe = vi.fn().mockRejectedValueOnce(failure).mockResolvedValue(() => {});
    const peek = vi.fn(async () => request("a"));
    const error = vi.fn();
    const handle = vi.fn();
    const drain = createNotificationActivationDrain({ transport: { subscribe, peek, ack: async () => true }, ready: () => true, handle, error });
    await settle();
    expect(peek).not.toHaveBeenCalled();
    expect(error).toHaveBeenCalledWith(failure);
    drain.retry();
    await settle();
    expect(subscribe).toHaveBeenCalledTimes(2);
    expect(handle).toHaveBeenCalledOnce();
    drain.dispose();
  });

  it("tracks failures in deferred guard work and makes completion idempotent", async () => {
    let context!: ActivationContext;
    const error = vi.fn();
    const ack = vi.fn(async () => true);
    const handle = vi.fn(async (_: NotificationActivation, next: ActivationContext) => { context = next; });
    const drain = createNotificationActivationDrain({ transport: { subscribe: async () => () => {}, peek: async () => request("a"), ack }, ready: () => true, handle, error });
    await settle();
    context.fail(new Error("draft failed"));
    drain.retry();
    await settle();
    expect(handle).toHaveBeenCalledTimes(2);
    await Promise.all([context.complete(), context.complete()]);
    expect(ack).toHaveBeenCalledOnce();
    drain.dispose();
  });
  it("repeeks after a nonmatching old ACK without dropping the newer slot", async () => {
    let pending: NotificationActivation | null = request("a");
    let release!: (value: boolean) => void;
    const oldAck = new Promise<boolean>(resolve => { release = resolve; });
    const applied: string[] = [];
    const drain = createNotificationActivationDrain({
      transport: {
        subscribe: async () => () => {}, peek: async () => pending,
        ack: async value => { if (value.requestId === "a") return oldAck; pending = null; return true; },
      },
      ready: () => true,
      handle: async (value, context) => { applied.push(value.requestId); await context.complete(); },
    });
    await settle();
    pending = request("b");
    release(false);
    await settle();
    expect(applied).toEqual(["a", "b"]);
    expect(pending).toBeNull();
    drain.dispose();
  });
  it("does not invoke native APIs without a native host", async () => {
    const stop = await nativeNotificationActivation.subscribe(() => {});
    expect(await nativeNotificationActivation.peek()).toBeNull();
    expect(await nativeNotificationActivation.ack(request("a"))).toBe(false);
    stop();
  });

  it("unsubscribes a delayed registration after disposal and resumes on remount", async () => {
    let registered!: (stop: () => void) => void;
    const registration = new Promise<() => void>(resolve => { registered = resolve; });
    const handle = vi.fn();
    const stop = vi.fn();
    const transport = { subscribe: () => registration, peek: async () => request("a"), ack: async () => true };
    const first = createNotificationActivationDrain({ transport, ready: () => true, handle });
    first.dispose();
    registered(stop);
    await settle();
    expect(stop).toHaveBeenCalledOnce();
    expect(handle).not.toHaveBeenCalled();
    const second = createNotificationActivationDrain({ transport, ready: () => true, handle });
    await settle();
    expect(handle).toHaveBeenCalledOnce();
    second.dispose();
  });
  it("retries failed work at data readiness without restarting a pending guard", async () => {
    const handle = vi.fn().mockRejectedValueOnce(new Error("refresh failed")).mockResolvedValue(undefined);
    const drain = createNotificationActivationDrain({ transport: { subscribe: async () => () => {}, peek: async () => request("a"), ack: async () => true }, ready: () => true, handle });
    await settle();
    drain.retry();
    await settle();
    drain.retry();
    await settle();
    expect(handle).toHaveBeenCalledTimes(2);
    drain.dispose();
  });
  it("retries a handled ACK without applying the destination twice", async () => {
    const handle = vi.fn(async (_: NotificationActivation, context: ActivationContext) => { await context.complete(); });
    const ack = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(true);
    const drain = createNotificationActivationDrain({ transport: { subscribe: async () => () => {}, peek: async () => request("a"), ack }, ready: () => true, handle });
    await settle();
    drain.wake();
    await settle();
    expect(ack).toHaveBeenCalledTimes(2);
    expect(handle).toHaveBeenCalledOnce();
    drain.dispose();
  });
  it("subscribes before peeking and retains locked work until readiness", async () => {
    const calls: string[] = [];
    let ready = false;
    const handle = vi.fn();
    const drain = createNotificationActivationDrain({
      transport: { subscribe: async () => { calls.push("subscribe"); return () => {}; }, peek: async () => { calls.push("peek"); return request("a"); }, ack: async () => true },
      ready: () => ready, handle,
    });
    await settle();
    expect(handle).not.toHaveBeenCalled();
    ready = true;
    drain.wake();
    await settle();
    expect(calls.slice(0, 2)).toEqual(["subscribe", "peek"]);
    expect(handle).toHaveBeenCalledOnce();
    drain.dispose();
  });

  it("invalidates A during asynchronous work and drains B without an old ACK", async () => {
    let pending = request("a");
    let hint = () => {};
    let release = () => {};
    const gate = new Promise<void>(resolve => { release = resolve; });
    const applied: string[] = [];
    const ack = vi.fn(async () => true);
    const drain = createNotificationActivationDrain({
      transport: { subscribe: async listener => { hint = listener; return () => {}; }, peek: async () => pending, ack },
      ready: () => true,
      handle: async (value, context) => {
        if (value.requestId === "a") await gate;
        if (!await context.validate()) return;
        applied.push(value.requestId);
        await context.complete();
      },
    });
    await settle();
    pending = request("b");
    hint();
    release();
    await settle();
    expect(applied).toEqual(["b"]);
    expect(ack).toHaveBeenCalledExactlyOnceWith(request("b"));
    drain.dispose();
  });

  it("retries peek errors and invalidates retained guard callbacks on disposal", async () => {
    let context: ActivationContext | undefined;
    const stop = vi.fn();
    const peek = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(request("a"));
    const ack = vi.fn();
    const drain = createNotificationActivationDrain({ transport: { subscribe: async () => stop, peek, ack }, ready: () => true, handle: async (_, value) => { context = value; } });
    await settle();
    drain.wake();
    await settle();
    expect(context).toBeDefined();
    drain.dispose();
    expect(await context!.validate()).toBe(false);
    await context!.complete();
    expect(ack).not.toHaveBeenCalled();
    expect(stop).toHaveBeenCalledOnce();
  });
});
