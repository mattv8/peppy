import { afterEach, describe, expect, it, vi } from "vitest";
import { createHostedSessionRenewal, installHostedSessionRenewal } from "./hostedSessionRenewal";

describe("installHostedSessionRenewal", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renews once for trusted visible interaction and coalesces concurrent input", async () => {
    const renew = vi.fn(() => Promise.resolve());
    const addEventListener = vi.spyOn(document, "addEventListener");
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    const stop = installHostedSessionRenewal(renew);
    const listener = addEventListener.mock.calls.find(([event]) => event === "pointerdown")?.[1] as EventListener;
    listener({ isTrusted: true } as Event);
    listener({ isTrusted: true } as Event);
    await Promise.resolve();

    expect(renew).toHaveBeenCalledOnce();
    stop();
  });

  it("does not renew while hidden", async () => {
    const renew = vi.fn(() => Promise.resolve());
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    const stop = installHostedSessionRenewal(renew);
    const scheduler = createHostedSessionRenewal(renew);
    scheduler.handleInteraction({ isTrusted: true });
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    scheduler.handleInteraction({ isTrusted: false });
    await Promise.resolve();

    expect(renew).not.toHaveBeenCalled();
    stop();
  });

  it("throttles successful and failed attempts", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(100_000);
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    const renew = vi.fn(() => Promise.reject(new Error("offline")));
    const scheduler = createHostedSessionRenewal(renew);
    scheduler.handleInteraction({ isTrusted: true });
    await Promise.resolve();
    await Promise.resolve();
    scheduler.handleInteraction({ isTrusted: true });
    expect(renew).toHaveBeenCalledOnce();
    vi.advanceTimersByTime(30_000);
    scheduler.handleInteraction({ isTrusted: true });
    expect(renew).toHaveBeenCalledTimes(2);
    vi.useRealTimers();
  });
});
