import { describe, expect, it, vi } from "vitest";
import { BrowserBridge } from "./browser-bridge";

class TestPort {
  public onmessage: ((event: MessageEvent) => void) | null = null;
  public onmessageerror: (() => void) | null = null;
  public readonly sent: unknown[] = [];
  public start = vi.fn();
  public close = vi.fn();
  public postMessage(message: unknown): void {
    this.sent.push(message);
  }
  public reply(message: unknown): void {
    this.onmessage?.({ data: message } as MessageEvent);
  }
}

describe("BrowserBridge.send_again", () => {
  it("sends the request as flat arguments to the worker and resolves with the worker's new message id", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const sent = bridge.send_again({ messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" });
    expect(port.sent).toEqual([{ id: 1, command: "send_again", args: { messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" } }]);
    port.reply({ id: 1, ok: true, value: { messageId: "message-new" } });
    await expect(sent).resolves.toEqual({ messageId: "message-new" });
    bridge.dispose();
  });

  it("rejects with the worker's error code, so the popover can map it", async () => {
    const port = new TestPort();
    const bridge = new BrowserBridge(port as unknown as MessagePort);
    const sent = bridge.send_again({ messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" });
    port.reply({ id: 1, ok: false, error: { code: "route-unavailable", message: "That phone can't send right now." } });
    await expect(sent).rejects.toMatchObject({ code: "route-unavailable" });
    bridge.dispose();
  });
});
