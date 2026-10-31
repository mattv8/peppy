// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { createSingleFlightTrailingScheduler } from "./single-flight-scheduler";

const deferred = () => {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
};

describe("createSingleFlightTrailingScheduler", () => {
  it("runs one active enrollment request and one trailing request", async () => {
    const first = deferred();
    const second = deferred();
    const run = vi.fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const scheduler = createSingleFlightTrailingScheduler(run);

    scheduler.request();
    scheduler.request();
    scheduler.request();
    expect(run).toHaveBeenCalledOnce();

    first.resolve();
    await vi.waitFor(() => expect(run).toHaveBeenCalledTimes(2));
    second.resolve();
  });

  it("resets after a synchronous throw", () => {
    const run = vi.fn().mockImplementationOnce(() => { throw new Error("sync failure"); }).mockResolvedValueOnce(undefined);
    const scheduler = createSingleFlightTrailingScheduler(run);
    scheduler.request();
    scheduler.request();
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("resets after an asynchronous rejection", async () => {
    let reject!: (error: Error) => void;
    const failed = new Promise<void>((_, fail) => { reject = fail; });
    const run = vi.fn().mockReturnValueOnce(failed).mockResolvedValueOnce(undefined);
    const scheduler = createSingleFlightTrailingScheduler(run);
    scheduler.request();
    reject(new Error("async failure"));
    await Promise.resolve();
    await Promise.resolve();
    scheduler.request();
    expect(run).toHaveBeenCalledTimes(2);
  });
});
