import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({ getCurrentWebviewWindow: () => ({ listen: vi.fn(async () => () => undefined) }) }));

import { tauriBridge } from "./bridge";

describe("tauriBridge.send_again", () => {
  beforeEach(() => invokeMock.mockReset());

  it("invokes the native command with the request under input, and returns the new message id", async () => {
    invokeMock.mockResolvedValueOnce({ messageId: "message-new" });
    await expect(tauriBridge.send_again({ messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" })).resolves.toEqual({ messageId: "message-new" });
    expect(invokeMock).toHaveBeenCalledWith("send_again", { input: { messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" } });
  });

  it("passes a native rejection through with its code", async () => {
    invokeMock.mockRejectedValueOnce({ code: "already-sent-again", message: "This message was already sent again." });
    await expect(tauriBridge.send_again({ messageId: "message-old", gatewayId: "gateway-pixel", simId: "sim-1" })).rejects.toMatchObject({ code: "already-sent-again" });
  });
});
