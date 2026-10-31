import { describe, expect, it, vi } from "vitest";
import type { AttachmentRetentionRequestDto } from "@peppy/generated/attachment-retention";
import { AttachmentRetentionDriver } from "./attachment-retention.js";
import { TransportError } from "./transport.js";

const capability = (cleanupReady: boolean): AttachmentRetentionRequestDto => ({
  method: "PUT", relativePath: "/v1/devices/self/retention-capability",
  jsonBody: JSON.stringify({ protocol_version: 1, cleanup_ready: cleanupReady }),
  protocolHeader: null, expectedSuccessStatus: 204,
});

function retentionSession(calls: string[], preparedBatches: Record<string, unknown>[][] = [[]]) {
  return {
    async query(command: string, args: Record<string, unknown>) {
      calls.push(`query:${command}`);
      if (command === "_worker_attachment_retention_capability_request") return capability(args.cleanupReady === true);
      if (command === "_worker_attachment_retention_operation_request") return { method: "POST", relativePath: "/v1/attachments/a/uses", jsonBody: "{}", protocolHeader: { name: "Peppy-Attachment-Retention-Protocol", value: "1" }, expectedSuccessStatus: 200 };
      if (command === "_worker_attachment_retention_parse_operation_response") return { useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history", state: "registered" };
      if (command === "_worker_attachment_retention_preparation_pending") return { pending: (preparedBatches[0]?.length ?? 0) > 0 };
      if (command === "_worker_attachment_retention_ready") return { ready: true };
      throw new Error(command);
    },
    async mutate(command: string) {
      calls.push(`mutate:${command}`);
      if (command === "_worker_attachment_retention_prepare") return { operations: preparedBatches.shift() ?? [] };
      return {};
    },
  };
}

describe("AttachmentRetentionDriver", () => {
  it("uses canonical capability once and leaves old-server sync enabled", async () => {
    const calls: string[] = [];
    const send = vi.fn(async (request: AttachmentRetentionRequestDto) => {
      if (request.relativePath.includes("retention-capability")) throw new TransportError("status", 404);
      return "";
    });
    const driver = new AttachmentRetentionDriver(retentionSession(calls), { retention: send });

    await expect(driver.negotiate(new AbortController().signal)).resolves.toBe(false);
    await expect(driver.negotiate(new AbortController().signal)).resolves.toBe(false);

    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(capability(false), expect.any(AbortSignal));
    expect(calls).not.toContain("mutate:_worker_attachment_retention_configure_protocol");
  });

  it("checkpoints preparation before HTTP, parses read-only, and advertises only after reconciliation", async () => {
    const calls: string[] = [];
    const operation = { operation: "register", useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history" };
    const driver = new AttachmentRetentionDriver(retentionSession(calls, [[operation], []]), { retention: async request => { calls.push(`http:${request.relativePath}:${request.jsonBody}`); return "{\"state\":\"registered\"}"; } });

    expect(await driver.negotiate(new AbortController().signal)).toBe(true);
    await driver.reconcile(new AbortController().signal);

    expect(calls).toEqual([
      "query:_worker_attachment_retention_capability_request", `http:/v1/devices/self/retention-capability:${capability(false).jsonBody}`,
      "mutate:_worker_attachment_retention_configure_protocol", "query:_worker_attachment_retention_preparation_pending", "mutate:_worker_attachment_retention_prepare",
      "query:_worker_attachment_retention_operation_request", "http:/v1/attachments/a/uses:{}",
      "query:_worker_attachment_retention_parse_operation_response", "mutate:_worker_attachment_retention_apply_response",
      "mutate:_worker_attachment_retention_prepare", "query:_worker_attachment_retention_preparation_pending",
      "query:_worker_attachment_retention_ready", "query:_worker_attachment_retention_capability_request",
      `http:/v1/devices/self/retention-capability:${capability(true).jsonBody}`,
    ]);
  });

  it("persists an exact policy-removal tombstone without applying a fabricated response", async () => {
    const calls: string[] = [];
    const operation = { operation: "register", useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history" };
    const driver = new AttachmentRetentionDriver(retentionSession(calls, [[operation], []]), { retention: async request => {
      if (request.relativePath.includes("retention-capability")) return "";
      throw new TransportError("status", 410, "attachment_removed_by_storage_policy");
    } });

    await driver.negotiate(new AbortController().signal);
    await driver.reconcile(new AbortController().signal);

    expect(calls).toContain("mutate:_worker_attachment_retention_report_removed");
    expect(calls).not.toContain("mutate:_worker_attachment_retention_apply_response");
  });

  it("fences a delayed response after cancellation before applying it", async () => {
    const calls: string[] = [];
    const operation = { operation: "register", useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history" };
    const controller = new AbortController();
    const driver = new AttachmentRetentionDriver(retentionSession(calls, [[operation]]), { retention: async request => {
      if (request.relativePath.includes("retention-capability")) return "";
      controller.abort(); return "{}";
    } });

    await driver.negotiate(controller.signal);
    await expect(driver.reconcile(controller.signal)).rejects.toMatchObject({ kind: "cancelled" });
    expect(calls).not.toContain("query:_worker_attachment_retention_parse_operation_response");
  });

  it("never performs HTTP when the preparation checkpoint is poisoned", async () => {
    const calls: string[] = [];
    const session = retentionSession(calls);
    session.mutate = async command => { calls.push(`mutate:${command}`); if (command === "_worker_attachment_retention_prepare") throw new Error("checkpoint failed"); return {}; };
    const baseQuery = session.query;
    session.query = async (command, args) => command === "_worker_attachment_retention_preparation_pending" ? { pending: true } : baseQuery(command, args);
    const retention = vi.fn(async () => "");
    const driver = new AttachmentRetentionDriver(session, { retention });
    await driver.negotiate(new AbortController().signal);

    await expect(driver.reconcile(new AbortController().signal)).rejects.toThrow("checkpoint failed");
    expect(retention).toHaveBeenCalledTimes(1);
  });

  it("stops empty due-progress work at the finite pass budget without advertising readiness", async () => {
    let prepares = 0;
    const session = {
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "_worker_attachment_retention_capability_request") return capability(args.cleanupReady === true);
        if (command === "_worker_attachment_retention_preparation_pending") return { pending: true };
        throw new Error(command);
      },
      mutate: async (command: string) => { if (command === "_worker_attachment_retention_prepare") { prepares += 1; return { operations: [] }; } return {}; },
    };
    const retention = vi.fn(async () => "");
    const driver = new AttachmentRetentionDriver(session, { retention });

    await driver.negotiate(new AbortController().signal);
    await expect(driver.reconcile(new AbortController().signal)).resolves.toBe(false);
    expect(prepares).toBe(8);
    expect(retention).toHaveBeenCalledTimes(1);
  });


  it("drains operations created after readiness and withdraws readiness after core invalidation", async () => {
    const calls: string[] = [];
    const operation = { operation: "release", useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history" };
    const batches: Record<string, unknown>[][] = [[]];
    let ready = true;
    const session = retentionSession(calls, batches);
    const originalQuery = session.query;
    session.query = async (command, args) => command === "_worker_attachment_retention_ready" ? { ready } : originalQuery(command, args);
    const send = vi.fn(async (_request: AttachmentRetentionRequestDto) => "{}");
    const driver = new AttachmentRetentionDriver(session, { retention: send });
    const signal = new AbortController().signal;

    await driver.negotiate(signal);
    await driver.reconcile(signal);
    batches[0] = [operation];
    batches[1] = [];
    ready = false;
    await driver.reconcile(signal);

    expect(calls).toContain("query:_worker_attachment_retention_operation_request");
    expect(send.mock.calls.map(([request]) => JSON.parse(request.jsonBody ?? "{}").cleanup_ready).filter(value => value !== undefined)).toEqual([false, true, false]);
  });

  it("retries transient negotiation and shares one in-flight attempt", async () => {
    const calls: string[] = [];
    let release!: () => void;
    let attempts = 0;
    const send = vi.fn(async () => {
      attempts += 1;
      if (attempts === 1) throw new TransportError("status", 503, "attachment_retention_unavailable");
      if (attempts === 2) await new Promise<void>(resolve => { release = resolve; });
      return "";
    });
    const driver = new AttachmentRetentionDriver(retentionSession(calls), { retention: send });
    const signal = new AbortController().signal;

    await expect(driver.negotiate(signal)).resolves.toBe(false);
    const first = driver.negotiate(signal);
    const second = driver.negotiate(signal);
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    release();
    await expect(Promise.all([first, second])).resolves.toEqual([true, true]);
    expect(send).toHaveBeenCalledTimes(2);
  });

});

describe("AttachmentRetentionNoticeFacade", () => {
  it("rejects missing, stale, and unknown-policy acknowledgements before HTTP", async () => {
    const { AttachmentRetentionNoticeFacade } = await import("./attachment-retention.js");
    let generation = 1;
    const session = {
      query: vi.fn(async (command: string) => {
        if (command === "_worker_identity_metadata") return { vaultId: "v", deviceId: "d" };
        if (command === "_worker_attachment_retention_status_request") return { method: "GET", relativePath: "/v1/devices/self/attachment-storage", jsonBody: null, protocolHeader: null, expectedSuccessStatus: 200 };
        if (command === "_worker_attachment_retention_parse_status") return { usedBytes: 0 };
        if (command === "_worker_attachment_retention_notice_acknowledgement_request") return { method: "POST", relativePath: "/v1/devices/self/attachment-retention-notice", jsonBody: "{}", protocolHeader: null, expectedSuccessStatus: 204 };
        throw new Error(command);
      }),
      mutate: vi.fn(),
    };
    const retention = vi.fn(async () => "{}");
    const facade = new AttachmentRetentionNoticeFacade(session, { retention }, () => generation, () => new AbortController().signal);

    await expect(facade.acknowledge({ scopeId: "", policyVersion: "hosted_attachment_retention_v1" })).resolves.toBe("stale_scope");
    const available = await facade.status();
    if (available.kind !== "available") throw new Error("expected available");
    await expect(facade.acknowledge({ scopeId: available.scopeId, policyVersion: "future" })).resolves.toBe("unsupported_policy_version");
    generation = 2;
    await expect(facade.acknowledge({ scopeId: available.scopeId, policyVersion: "hosted_attachment_retention_v1" })).resolves.toBe("stale_scope");
    expect(retention).toHaveBeenCalledTimes(1);
  });
});

describe("AttachmentRetentionDriver readiness withdrawal", () => {
  it("withdraws server readiness before a newly backlogged retention operation fails", async () => {
    const calls: string[] = [];
    const operation = { operation: "register", useId: "u", attachmentId: "a", deviceId: "d", kind: "message_history" };
    const batches: Record<string, unknown>[][] = [[]];
    let ready = true;
    const session = retentionSession(calls, batches);
    const baseQuery = session.query;
    session.query = async (command, args) => command === "_worker_attachment_retention_ready" ? { ready } : baseQuery(command, args);
    const transport = vi.fn(async (request: AttachmentRetentionRequestDto) => {
      calls.push(`transport:${request.relativePath}:${request.jsonBody}`);
      if (request.relativePath.includes("/uses")) throw new TransportError("status", 409, "conflict");
      return "{}";
    });
    const driver = new AttachmentRetentionDriver(session, { retention: transport });
    const signal = new AbortController().signal;

    await driver.negotiate(signal);
    await driver.reconcile(signal);
    batches[0] = [operation];
    ready = false;
    await expect(driver.prepareRound(signal)).resolves.toMatchObject({ negotiationResolved: true, protocolEnabled: true, dueExhausted: false });

    const withdrawn = calls.indexOf(`transport:/v1/devices/self/retention-capability:${capability(false).jsonBody}`, 2);
    const failedOperation = calls.indexOf("transport:/v1/attachments/a/uses:{}");
    expect(withdrawn).toBeGreaterThan(0);
    expect(withdrawn).toBeLessThan(failedOperation);
  });
});
