import { describe, expect, it, vi } from "vitest";
import { CoreRejectedError } from "./core.js";
import { TypingController } from "./typing-controller.js";

function fixture() {
  let key: string | null = "first";
  let callbacks!: { onReady: () => void; onClosed: () => void; onFrame: (frame: string) => void };
  const invoke = vi.fn(async (command: string, _args: Record<string, unknown>): Promise<unknown> => {
    if (command === "_worker_typing_context_key") return { key };
    if (command === "_worker_typing_enable") return { generation: "1" };
    if (command === "_worker_typing_seal") return { generation: "1", frameJson: "sealed" };
    if (command === "_worker_typing_open") return {
      generation: "1",
      sessionUpdate: { peerDeviceId: "peer", currentSessionToken: "1", validationRevision: "1" },
      action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "chat", state: "started", deadlineMono: "200" },
    };
    return null;
  });
  const supervisor = { start: vi.fn(), stop: vi.fn(async () => {}), transmit: vi.fn(() => true) };
  const publish = vi.fn();
  const onError = vi.fn();
  const controller = new TypingController({
    core: { invoke }, clock: () => ({ wallMs: 1_000, monoMs: 100 }), publish, onError,
    supervisor: handlers => { callbacks = handlers; return supervisor; },
  });
  const settle = async () => { for (let i = 0; i < 15; i++) await Promise.resolve(); };
  return { controller, invoke, supervisor, publish, onError, callbacks, settle, setKey: (value: string | null) => { key = value; } };
}

describe("TypingController", () => {
  it("enables on hello and preserves the incarnation for an unchanged key", async () => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    await f.controller.refreshContext();
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_enable")).toHaveLength(1);
    expect(f.invoke).toHaveBeenCalledWith("_worker_typing_set_sharing", { sharing: true });
    f.setKey("second");
    await f.controller.refreshContext();
    f.setKey(null);
    await f.controller.refreshContext();
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_enable")).toHaveLength(2);
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_disable")).toHaveLength(2);
  });

  it("only forwards the final tab's stopped intent, including disconnect", async () => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    const a = {};
    const b = {};
    await f.controller.send(a, "chat", "started");
    await f.controller.send(b, "chat", "started");
    await f.controller.send(a, "chat", "stopped");
    expect(f.invoke.mock.calls.filter(([command, args]) => command === "_worker_typing_seal" && args.state === "stopped")).toHaveLength(0);
    await f.controller.disconnect(b);
    expect(f.invoke.mock.calls.filter(([command, args]) => command === "_worker_typing_seal" && args.state === "stopped")).toHaveLength(1);
  });

  it.each(["disconnect", "stop"] as const)("does not start a new conversation after a superseding %s", async action => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    const port = {};
    await f.controller.send(port, "old", "started");
    const original = f.invoke.getMockImplementation()!;
    let release!: (value: unknown) => void;
    f.invoke.mockImplementation(async (command, args) => {
      if (command === "_worker_typing_seal" && args.conversationId === "old" && args.state === "stopped") {
        return new Promise(resolve => { release = resolve; });
      }
      return original(command, args);
    });
    const switching = f.controller.send(port, "new", "started");
    await f.settle();
    expect(release).toBeDefined();
    if (action === "disconnect") await f.controller.disconnect(port);
    else await f.controller.send(port, "new", "stopped");
    release({ generation: "1", frameJson: "old-stop" });
    await switching;
    expect(f.invoke.mock.calls.filter(([command, args]) =>
      command === "_worker_typing_seal" && args.conversationId === "new" && args.state === "started",
    )).toHaveLength(0);
  });

  it("forwards a stop without prior local interest when no tab is typing", async () => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    await f.controller.send({}, "chat", "stopped");
    expect(f.invoke.mock.calls.filter(([command, args]) => command === "_worker_typing_seal" && args.state === "stopped")).toHaveLength(1);
  });

  it("converts display deadlines and suppresses frames after stop", async () => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    f.callbacks.onFrame("frame");
    await f.settle();
    expect(f.publish).toHaveBeenCalledWith({ conversationId: "chat", state: "started", expiresAtMs: "1100" });
    await f.controller.stop();
    f.callbacks.onFrame("frame");
    await f.settle();
    expect(f.publish).toHaveBeenCalledOnce();
  });

  it("discards an open completion that arrives after stop", async () => {
    const f = fixture();
    const original = f.invoke.getMockImplementation()!;
    let finishOpen!: (value: unknown) => void;
    f.invoke.mockImplementation(async (command, args) => {
      if (command === "_worker_typing_open") return new Promise(resolve => { finishOpen = resolve; });
      return original(command, args);
    });
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    f.callbacks.onFrame("frame");
    await f.settle();
    const stop = f.controller.stop();
    finishOpen({
      generation: "1",
      sessionUpdate: { peerDeviceId: "peer", currentSessionToken: "1", validationRevision: "1" },
      action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "chat", state: "started", deadlineMono: "200" },
    });
    await stop;
    await f.settle();
    expect(f.publish).not.toHaveBeenCalled();
    expect(f.supervisor.stop).toHaveBeenCalledOnce();
    await f.controller.stop();
    expect(f.supervisor.stop).toHaveBeenCalledOnce();
  });

  it("handles rejected enable and swallows failed disable on stop", async () => {
    const f = fixture();
    f.invoke.mockImplementation(async command => {
      if (command === "_worker_typing_context_key") return { key: "first" };
      throw new CoreRejectedError("disabled");
    });
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    await expect(f.controller.send({}, "chat", "started")).resolves.toBeUndefined();
    await expect(f.controller.stop()).resolves.toBeUndefined();
    expect(f.onError).not.toHaveBeenCalled();
  });

  it("retires a pending enable synchronously when stopped", async () => {
    const f = fixture();
    let finishEnable!: (value: unknown) => void;
    f.invoke.mockImplementation(async command => {
      if (command === "_worker_typing_context_key") return { key: "first" };
      if (command === "_worker_typing_enable") return new Promise(resolve => { finishEnable = resolve; });
      return null;
    });
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    const stop = f.controller.stop();
    finishEnable({ generation: "1" });
    await stop;
    await f.settle();
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_set_sharing")).toHaveLength(0);
    await f.controller.send({}, "chat", "started");
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_seal")).toHaveLength(0);
  });

  it("disables on close and reapplies remembered sharing on reconnect", async () => {
    const f = fixture();
    await f.controller.setSharing(false);
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    f.callbacks.onClosed();
    await f.settle();
    f.callbacks.onReady();
    await f.settle();
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_disable")).toHaveLength(1);
    expect(f.invoke.mock.calls.filter(([command]) => command === "_worker_typing_set_sharing"))
      .toEqual([["_worker_typing_set_sharing", { sharing: false }], ["_worker_typing_set_sharing", { sharing: false }]]);
  });

  it("propagates fatal failures and handles rejected sends", async () => {
    const f = fixture();
    f.controller.start();
    f.callbacks.onReady();
    await f.settle();
    const original = f.invoke.getMockImplementation()!;
    f.invoke.mockImplementation(async (command, args) => {
      if (command === "_worker_typing_seal") throw new CoreRejectedError("disabled");
      return original(command, args);
    });
    await expect(f.controller.send({}, "chat", "started")).resolves.toBeUndefined();
    f.invoke.mockImplementation(async () => { throw new Error("fatal"); });
    await expect(f.controller.refreshContext()).rejects.toThrow("fatal");
  });
});
