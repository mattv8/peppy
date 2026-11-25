import { afterEach, describe, expect, it, vi } from "vitest";
import { closeWaitOutcome, createInFlightSend, waitForSend } from "./sendSettlement";

describe("createInFlightSend", () => {
  it("reports submission and preserves the first settlement", async () => {
    const send = createInFlightSend();
    send.submit();
    send.settle("sent");
    send.settle("unsent");
    await expect(send.submitted).resolves.toBe(true);
    await expect(send.settled).resolves.toBe("sent");
    await expect(waitForSend(send.submitted, 6000)).resolves.toBe(true);
  });

  it("reports no submission when settled first", async () => {
    const send = createInFlightSend();
    send.settle("unsent");
    send.submit();
    await expect(send.submitted).resolves.toBe(false);
    await expect(send.settled).resolves.toBe("unsent");
  });
});

describe("closeWaitOutcome", () => {
  afterEach(() => vi.useRealTimers());

  it.each([
    ["sent", ["sent"], undefined],
    ["unsent", ["unsent"], "send-unsent"],
    ["sent", ["sent", "unsent"], "reaction-unsent"],
  ] as const)("settles send and reactions with precedence", async (send, reactions, expected) => {
    await expect(closeWaitOutcome(Promise.resolve(send), reactions.map((result) => Promise.resolve(result)), 6000)).resolves.toBe(expected);
  });

  it.each([
    [true, "still-sending"],
    [false, "reaction-still-sending"],
  ] as const)("uses one timeout and prioritizes pending send: %s", async (pendingSend, expected) => {
    vi.useFakeTimers();
    const pending = new Promise<"sent" | "unsent">(() => {});
    const outcome = closeWaitOutcome(pendingSend ? pending : Promise.resolve("unsent"), [pending], 6000);
    await vi.advanceTimersByTimeAsync(6000);
    await expect(outcome).resolves.toBe(expected);
    expect(vi.getTimerCount()).toBe(0);
  });
});

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
