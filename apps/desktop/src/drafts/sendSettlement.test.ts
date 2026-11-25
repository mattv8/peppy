import { afterEach, describe, expect, it, vi } from "vitest";
import { waitForSend } from "./sendSettlement";

describe("waitForSend", () => {
  afterEach(() => vi.useRealTimers());

  it("returns the settlement and clears its timer", async () => {
    vi.useFakeTimers();
    const result = waitForSend(Promise.resolve("sent"), 6000);
    await expect(result).resolves.toBe("sent");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("returns still-sending at the bound", async () => {
    vi.useFakeTimers();
    const result = waitForSend(new Promise(() => {}), 6000);
    await vi.advanceTimersByTimeAsync(6000);
    await expect(result).resolves.toBe("still-sending");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("returns an unsent settlement", async () => {
    vi.useFakeTimers();
    await expect(waitForSend(Promise.resolve("unsent"), 6000)).resolves.toBe("unsent");
    expect(vi.getTimerCount()).toBe(0);
  });
});
