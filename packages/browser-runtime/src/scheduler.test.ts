import { describe, expect, it } from "vitest";
import { BrowserScheduler } from "./scheduler.js";
import { transferMedia } from "./scheduler-media.js";
import { CoreRejectedError } from "./core.js";
import { TransportError } from "./transport.js";

describe("BrowserScheduler", () => {
  it("negotiates before producer work and advertises readiness only after replay", async () => {
    const calls: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        calls.push(`query:${command}`);
        if (command === "pending_uploads" || command === "pending_downloads") return [];
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "sync_work_status") return {};
        return [];
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
    const transport = {
      json: async (path: string) => { calls.push(`http:${path}`); return { events: [] }; },
      upload: async () => undefined, download: async () => new Uint8Array(),
    };
    const retention = {
      prepareRound: async () => { calls.push("retention:negotiate"); return { negotiationResolved: true, protocolEnabled: true, dueExhausted: true }; },
      reconcile: async () => { calls.push("retention:reconcile"); return true; },
    };

    await new BrowserScheduler(session, transport, { retention: retention as never }).poll();

    expect(calls.indexOf("retention:negotiate")).toBeLessThan(calls.indexOf("query:pending_outbox"));
    expect(calls.indexOf("retention:reconcile")).toBeGreaterThan(calls.indexOf("http:/v1/events?after=0&limit=100"));
  });

  it("acknowledges an outbox envelope only after its byte-identical upload succeeds", async () => {
    const calls: string[] = [];
    const envelope = { envelope_id: "envelope-1", purpose: "event", body: { ciphertext: "AQI=" } };
    const session = {
      query: async (command: string) => command === "pending_outbox" ? { envelopes: calls.includes("ack") ? [] : [envelope] } : { cursor: "0" },
      mutate: async (command: string) => { calls.push(command === "ack_outbox" ? "ack" : command); return { applied: 0, quarantined: 0, drained: 0 }; },
      hydrate: async () => undefined,
    };
    const sent: unknown[] = [];
    const transport = {
      json: async (_path: string, request: { body?: unknown }) => {
        if (request.body) sent.push(request.body);
        return { events: [] };
      },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    };
    await new BrowserScheduler(session, transport).poll();
    expect(sent).toEqual([envelope]);
    expect(calls).toContain("ack");
  });

  it("finishes an empty compacted snapshot without requesting a record page", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "snapshot_progress") return { progress: null };
        throw new Error(command);
      },
      mutate: async (command: string) => {
        mutations.push(command);
        if (command === "begin_snapshot") return { generation: "7", highWater: "9", expectedRecords: "0", receivedRecords: "0", lastCursor: "0", serverCompactionGeneration: "3" };
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    const paths: string[] = [];
    const transport = {
      json: async (path: string) => {
        paths.push(path);
        if (path.startsWith("/v1/events")) throw new (await import("./transport.js")).TransportError("status", 409, "resync_required");
        return { high_water_cursor: "9", record_count: "0", compaction_supported: true, compaction_generation: "3" };
      },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    };
    await new BrowserScheduler(session, transport).poll();
    expect(mutations).toContain("finish_snapshot");
    expect(paths.some(path => path.startsWith("/v1/snapshot/records"))).toBe(false);
  });

  it("uploads only ciphertext and checkpoints the committed remote attachment", async () => {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_uploads") return [{ id: "a", ciphertextBytes: 3 }];
        if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "protected", referenceTracking: false };
        if (command === "cipher_file") return { file: "client.db.media/cipher/a.ppss", ciphertextSha256: "abc" };
        if (command === "contact_photo_transfer_state") return { uploads: [] };
        return [];
      },
      mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
    };
    const uploads: Uint8Array[] = [];
    await transferMedia(session, {
      json: async (path: string) => path.endsWith("reserve") ? { attachment_id: "a" } : { attachment_id: "a", duplicate: false },
      upload: async (_path, bytes) => { uploads.push(bytes); },
      download: async () => new Uint8Array(),
    }, {
      readCiphertext: async () => new Uint8Array([1, 2, 3]),
      stageDownload: async () => "unused",
    }, new AbortController().signal);
    expect(uploads).toEqual([new Uint8Array([1, 2, 3])]);
    expect(mutations).toContainEqual(["mark_attachment_uploaded", { id: "a", remoteObjectId: "a" }]);
  });

  it("passes complete server roster and capability responses to the worker-only context command", async () => {
    const contexts: unknown[] = [];
    const devices = { devices: [{ device_id: "1d52c752-817c-4d92-a8d5-8ca7f361ab1a", role: "gateway", revoked: false }] };
    const capabilities = { capabilities: [{ device_id: "1d52c752-817c-4d92-a8d5-8ca7f361ab1a", simulator: false, capabilities: { sims: [{ subscription_id: "sim-1", sms: "available", mms: "available", mms_content_version: 2 }] } }] };
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        throw new Error(command);
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
    const transport = {
      json: async (path: string) => path === "/v1/devices" ? devices : path === "/v1/capabilities" ? capabilities : { events: [] },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    };
    await new BrowserScheduler(session, transport).poll();
    expect(contexts).toEqual([{ devices, capabilities, connection: "connected", outboxRejected: false }]);
  });

  it("reports gateway HTTP failures as offline server context", async () => {
    const contexts: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
      const scheduler = new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path.startsWith("/v1/events")) throw new TransportError("status", 503, "busy");
        return {};
      },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    });

    await expect(scheduler.poll()).resolves.toEqual({ connection: "offline" });
    expect(contexts).toEqual([{ devices: null, capabilities: null, connection: "offline" }]);
  });

  it("preserves the prior connection status when a stopped poll is cancelled", async () => {
    let started!: () => void;
    const requested = new Promise<void>(resolve => { started = resolve; });
    const scheduler = new BrowserScheduler({
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "pending_uploads" || command === "pending_downloads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : { cursor: "0" },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    }, {
      json: async (_path, request) => {
        started();
        await new Promise<void>(resolve => request.signal?.addEventListener("abort", () => resolve(), { once: true }));
        throw new (await import("./transport.js")).TransportError("cancelled");
      },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    });

    const polling = scheduler.poll();
    await requested;
    scheduler.stop();
    const cancelled = expect(polling).rejects.toMatchObject({ kind: "cancelled" });
    await cancelled;
    expect(scheduler.status).toEqual({ connection: "offline" });
  });

  it("stages only exact-size downloaded ciphertext before checkpointed install", async () => {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [] : command === "pending_downloads" ? [{ id: "a", ciphertextBytes: 3, remoteObjectId: "remote-id" }] : command === "contact_photo_transfer_state" ? { uploads: [] } : [],
      mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
    };
    const staged: Uint8Array[] = [];
    await transferMedia(session, {
      json: async () => ({}),
      upload: async () => undefined,
      download: async () => new Uint8Array([1, 2, 3]),
    }, {
      readCiphertext: async () => new Uint8Array(),
      stageDownload: async bytes => { staged.push(bytes); return "download-uuid"; },
    }, new AbortController().signal);
    expect(staged).toEqual([new Uint8Array([1, 2, 3])]);
    expect(mutations).toEqual([["install_downloaded_attachment", { id: "a", source: "download-uuid" }]]);
  });

  it("does not stage a download whose byte count fails the core contract", async () => {
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : [{ id: "a", ciphertextBytes: 3, remoteObjectId: "remote-id" }],
      mutate: async () => ({}),
    };
    let staged = false;
    const result = await transferMedia(session, {
      json: async () => ({}), upload: async () => undefined, download: async () => new Uint8Array([1, 2]),
    }, {
      readCiphertext: async () => new Uint8Array(), stageDownload: async () => { staged = true; return "download-uuid"; },
    }, new AbortController().signal);
    expect(staged).toBe(false);
    expect(result.failedIds).toEqual(["a"]);
  });

  it("continues replay after a rejected outbox envelope without acknowledging it", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] } : command === "receive_cursor" ? { cursor: "0" } : { uploads: [] },
      mutate: async (command: string) => { mutations.push(command); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
    let replayed = false;
    await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new (await import("./transport.js")).TransportError("status", 400, "invalid_event");
        if (path.startsWith("/v1/events?")) return { events: [{ cursor: "1", envelope: {} }], next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();
    replayed = mutations.includes("ingest_batch");
    expect(mutations).not.toContain("ack_outbox");
    expect(replayed).toBe(true);
  });

  it.each([403, 409])("retains a rejected %i outbox envelope while receiving inbound events", async status => {
    const mutations: Array<[string, Record<string, unknown>]> = [];
    const events = [{ cursor: "1", envelope: {} }];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] } : command === "receive_cursor" ? { cursor: "0" } : { uploads: [] },
      mutate: async (command: string, args: Record<string, unknown>) => { mutations.push([command, args]); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
    const result = await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new (await import("./transport.js")).TransportError("status", status, "blocked");
        if (path.startsWith("/v1/events?")) return { events, next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();
    expect(result.connection).toBe("connected");
    expect(mutations.map(([command]) => command)).not.toContain("ack_outbox");
    expect(mutations).toContainEqual(["ingest_batch", { events }]);
  });

  it("treats an outbox 401 as fatal revocation without acknowledging or replaying", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] } : command === "receive_cursor" ? { cursor: "0" } : { uploads: [] },
      mutate: async (command: string) => { mutations.push(command); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
      const scheduler = new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new TransportError("revoked");
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    });

    await expect(scheduler.poll()).resolves.toEqual({ connection: "revoked" });
    expect(mutations).not.toContain("ack_outbox");
    expect(mutations).not.toContain("ingest");
  });

  it.each([403, 502])("does not classify a bodiless %i proxy failure as revoked or acknowledge its envelope", async status => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] } : command === "receive_cursor" ? { cursor: "0" } : { uploads: [] },
      mutate: async (command: string) => { mutations.push(command); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
      const result = await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new TransportError("status", status);
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();

    expect(result.connection).not.toBe("revoked");
    expect(mutations).not.toContain("ack_outbox");
  });

  it.each([403, 409])("reports a coded %i outbox refusal as connected with a sanitized outbox-rejected flag", async status => {
    const mutations: Array<[string, Record<string, unknown>]> = [];
    const contexts: Record<string, unknown>[] = [];
    const events = [{ cursor: "1", envelope: {} }];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async (command: string, args: Record<string, unknown>) => { mutations.push([command, args]); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
      const result = await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new TransportError("status", status, "blocked");
        if (path.startsWith("/v1/events?")) return { events, next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();

    expect(result).toEqual({ connection: "connected" });
    expect(mutations.map(([command]) => command)).not.toContain("ack_outbox");
    expect(mutations).toContainEqual(["ingest_batch", { events }]);
    expect(contexts.at(-1)).toMatchObject({ connection: "connected", outboxRejected: true });
    expect(JSON.stringify(contexts)).not.toContain(String(status));
  });

  it("clears the outbox-rejected flag after the next pass delivers the envelope", async () => {
    let refusing = true;
    const acked: string[] = [];
    const contexts: Record<string, unknown>[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: acked.length > 0 ? [] : [{ envelope_id: "outbox-1", purpose: "event" }] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async (command: string, args: Record<string, unknown>) => {
        if (command === "ack_outbox") acked.push(String(args.envelopeId));
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
      const scheduler = new BrowserScheduler(session, {
      json: async (path: string, request: { method: string }) => {
        if (path === "/v1/events" && request.method === "POST" && refusing) throw new TransportError("status", 403, "blocked");
        if (path === "/v1/events" && request.method === "POST") return {};
        if (path.startsWith("/v1/events?")) return { events: [], next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    });

    await scheduler.poll();
    expect(contexts.at(-1)).toMatchObject({ outboxRejected: true });
    refusing = false;
    await scheduler.poll();
    expect(acked).toEqual(["outbox-1"]);
    expect(contexts.at(-1)).toMatchObject({ connection: "connected", outboxRejected: false });
  });

  it("does not flag a code-less 403 proxy refusal as an outbox rejection", async () => {
    const contexts: Record<string, unknown>[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
      const result = await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new TransportError("status", 403);
        if (path.startsWith("/v1/events?")) return { events: [], next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();

    expect(result.connection).not.toBe("revoked");
    expect(contexts.at(-1)).toMatchObject({ outboxRejected: false });
  });

  it("keeps an outbox 401 fatal without publishing an outbox-rejected context", async () => {
    const contexts: Record<string, unknown>[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [{ envelope_id: "outbox-1", purpose: "event" }] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
      const result = await new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path === "/v1/events") throw new TransportError("revoked");
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }).poll();

    expect(result).toEqual({ connection: "revoked" });
    expect(contexts.some(context => context.outboxRejected === true)).toBe(false);
  });

  it("publishes a clean connected context when the outbox is empty", async () => {
    const contexts: Record<string, unknown>[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
        return { uploads: [] };
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
    const result = await new BrowserScheduler(session, { json: async () => ({ events: [] }), upload: async () => undefined, download: async () => new Uint8Array() }).poll();

    expect(result).toEqual({ connection: "connected" });
    expect(contexts.at(-1)).toMatchObject({ connection: "connected", outboxRejected: false });
  });

  it("continues with later uploads after one attachment fails", async () => {
    const marked: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_uploads") return [{ id: "bad", ciphertextBytes: 1 }, { id: "good", ciphertextBytes: 1 }];
        if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "protected", referenceTracking: false };
        if (command === "contact_photo_transfer_state") return { uploads: [] };
        if (command === "cipher_file") return { file: String(args.id), ciphertextSha256: "hash" };
        return [];
      },
      mutate: async (command: string, args: Record<string, unknown>) => { if (command === "mark_attachment_uploaded") marked.push(String(args.id)); return {}; },
    };
    const result = await transferMedia(session, {
      json: async (path: string, request: { body?: unknown }) => {
        if (path.endsWith("reserve")) {
          const id = (request.body as { attachment_id: string }).attachment_id;
          if (id === "bad") throw new (await import("./transport.js")).TransportError("status", 400, "invalid");
          return { attachment_id: id };
        }
        return { attachment_id: "good", duplicate: false };
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" }, new AbortController().signal);
    expect(result.failedIds).toEqual(["bad"]);
    expect(marked).toEqual(["good"]);
  });

  it("probes finalize after a reservation conflict and restores a lost upload checkpoint", async () => {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [{ id: "a", ciphertextBytes: 1 }] : command === "pending_downloads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : command === "_worker_attachment_retention_upload_classification" ? { retentionClass: "protected", referenceTracking: false } : { file: "a", ciphertextSha256: "hash" },
      mutate: async (command: string, args: Record<string, unknown>) => { mutations.push([command, args]); return {}; },
    };
    const paths: string[] = [];
    await transferMedia(session, {
      json: async (path: string) => {
        paths.push(path);
        if (path.endsWith("reserve")) throw new (await import("./transport.js")).TransportError("status", 409, "attachment_reservation_conflict");
        return { attachment_id: "a", duplicate: true };
      }, upload: async () => { throw new Error("must not re-upload"); }, download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" }, new AbortController().signal);
    expect(paths).toEqual(["/v1/attachments/reserve", "/v1/attachments/a/finalize"]);
    expect(mutations).toContainEqual(["mark_attachment_uploaded", { id: "a", remoteObjectId: "a" }]);
  });

  it("does not acknowledge an upload whose reservation response names another attachment", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [{ id: "a", ciphertextBytes: 1 }] : command === "pending_downloads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : command === "_worker_attachment_retention_upload_classification" ? { retentionClass: "protected", referenceTracking: false } : { file: "a", ciphertextSha256: "hash" },
      mutate: async (command: string) => { mutations.push(command); return {}; },
    };
    const result = await transferMedia(session, {
      json: async () => ({ attachment_id: "other" }), upload: async () => undefined, download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" }, new AbortController().signal);
    expect(result.failedIds).toEqual(["a"]);
    expect(mutations).not.toContain("mark_attachment_uploaded");
  });

  it("cleans a staged download when core installation rejects it", async () => {
    let cleaned: string | undefined;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [] : command === "pending_downloads" ? [{ id: "a", ciphertextBytes: 1, remoteObjectId: "remote" }] : command === "contact_photo_transfer_state" ? { uploads: [] } : [],
      mutate: async () => { throw new Error("core rejected"); },
    };
    const result = await transferMedia(session, { json: async () => ({}), upload: async () => undefined, download: async () => new Uint8Array([1]) }, {
      readCiphertext: async () => new Uint8Array(), stageDownload: async () => "staged", cleanupStagedDownload: async source => { cleaned = source; },
    }, new AbortController().signal);
    expect(result.failedIds).toEqual(["a"]);
    expect(cleaned).toBe("staged");
  });

  it("does not issue media effects after cancellation", async () => {
    const controller = new AbortController();
    controller.abort();
    let queried = false;
    await expect(transferMedia({ query: async () => { queried = true; return []; }, mutate: async () => ({}), hydrate: async () => undefined }, {
      json: async () => ({}), upload: async () => undefined, download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array(), stageDownload: async () => "unused" }, controller.signal)).rejects.toMatchObject({ kind: "cancelled" });
    expect(queried).toBe(false);
  });

  it("retries a deferred attachment only after retryAttachment clears its failure state", async () => {
    let attempts = 0;
    let uploaded = false;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "pending_uploads") return uploaded ? [] : [{ id: "a", ciphertextBytes: 1 }];
        if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "protected", referenceTracking: false };
        if (command === "pending_downloads") return [];
        if (command === "contact_photo_transfer_state") return { uploads: [] };
        if (command === "cipher_file") return { file: "a", ciphertextSha256: "hash" };
        if (command === "receive_cursor") return { cursor: "0" };
        return {};
      },
      mutate: async (command: string) => { if (command === "mark_attachment_uploaded") uploaded = true; return { applied: 0, quarantined: 0, drained: 0 }; },
    };
    const scheduler = new BrowserScheduler(session, {
      json: async (path: string) => {
        if (path.endsWith("reserve")) {
          attempts += 1;
          if (attempts === 1) throw new (await import("./transport.js")).TransportError("status", 503, "busy");
          return { attachment_id: "a" };
        }
        if (path.endsWith("finalize")) return { attachment_id: "a", duplicate: false };
        if (path.startsWith("/v1/events")) return { events: [] };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    }, { media: { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" } });
    await scheduler.poll();
    await scheduler.poll();
    expect(attempts).toBe(1);
    scheduler.retryAttachment("a");
    await scheduler.poll();
    expect(attempts).toBe(2);
    expect(uploaded).toBe(true);
  });

  it("skips idle apply checkpoints until the protocol reports projection work", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "receive_cursor" ? { cursor: "0" } : { uploads: [] },
      mutate: async (command: string) => { mutations.push(command); return { applied: 0, quarantined: 0, drained: 0 }; },
    };
    await new BrowserScheduler(session, { json: async () => ({ events: [] }), upload: async () => undefined, download: async () => new Uint8Array() }).poll();
    expect(mutations).not.toContain("apply_pending");
  });

  it("captures trusted proof context immediately before replay and preserves explicit next_after presence", async () => {
    const calls: string[] = [];
    const attemptId = "018f2c5e-6a3b-7c4d-8e5f-0123456789ab";
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        calls.push(`query:${command}`);
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_history_search_sync_context") return { startedAtMs: 10, snapshotGeneration: 2 };
        if (command === "sync_work_status") return { pendingApply: false };
        return {};
      },
      mutate: async (command: string, args: Record<string, unknown>) => {
        calls.push(`mutate:${command}`);
        if (command === "_worker_history_search_work") return { kind: "wait" };
        if (command === "_worker_observe_history_search_sync") {
          expect(args).toEqual({ attemptId, facts: { proof: {
            context: { startedAtMs: 10, snapshotGeneration: 2 },
            highWaterCursor: "9", nextAfterPresent: true, terminalNextAfter: true,
          } } });
        }
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    const scheduler = new BrowserScheduler(session, {
      json: async path => {
        calls.push(`http:${path}`);
        if (path.startsWith("/v1/events")) return { events: [], high_water_cursor: "9", next_after: null };
        return {};
      }, upload: async () => undefined, download: async () => new Uint8Array(),
    });
    scheduler.trackHistorySearch(attemptId);
    await scheduler.poll();
    expect(calls.indexOf("query:_worker_history_search_sync_context")).toBeLessThan(calls.findIndex(call => call.startsWith("http:/v1/events")));
    expect(calls).toContain("mutate:_worker_observe_history_search_sync");
  });

  it("keeps a server epoch manual, then rotates and wakes only after explicit success", async () => {
    let woke = 0;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
        if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
        if (command === "compaction_status") return { server_supported: false, server_active: false };
        if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
        if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [] };
        if (command === "contact_repair_status") return { repairRequired: false };
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        return {};
      },
      mutate: async () => ({ backfill_complete: true, processed: 0 }),
    };
    const transport = {
      discard: async () => undefined,
      json: async (path: string) => {
        if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 2, profile_fingerprint: "next", public_key_profile: { key_epoch: 2 }, encrypted_vault_check_header: btoa("{}") };
        if (path === "/v1/snapshot") return { compaction_supported: false };
        if (path.startsWith("/v1/events")) return { events: [] };
        return {};
      },
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    };
    const scheduler = new BrowserScheduler(session, transport, {
      rotateEpoch: async profile => expect(profile).toEqual({ key_epoch: 2 }),
      onEpochUnlocked: () => { woke += 1; },
    });
    await scheduler.poll();
    expect(scheduler.epochStatus?.state).toBe("epoch-mismatch");
    await scheduler.unlockNewEpoch("manual-passphrase");
    expect(scheduler.epochStatus?.state).toBe("current");
    expect(woke).toBe(1);
  });

  it("checkpoints one 100-event replay page before applying it", async () => {
    const mutations: [string, Record<string, unknown>][] = [];
    const events = Array.from({ length: 100 }, (_, index) => ({ cursor: String(index + 1), envelope: { index } }));
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string, args: Record<string, unknown>) => {
        mutations.push([command, args]);
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events, next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }).poll();

    const ingest = mutations.filter(([command]) => command === "ingest_batch");
    expect(ingest).toEqual([["ingest_batch", { events }]]);
    expect(mutations.findIndex(([command]) => command === "apply_pending")).toBeGreaterThan(mutations.findIndex(([command]) => command === "ingest_batch"));
  });

  it("publishes exact replay cursors only after durable ingest and apply checkpoints", async () => {
    const mutations: string[] = [];
    const progress = [] as Array<{ phase: string; receivedThrough: string; processedThrough: string; targetHighWater?: string }>;
    let applied = false;
    let applySteps = 0;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "sync_progress") return applied
          ? { receivedThrough: "2", processedThrough: "2", waitingForKeys: "0" }
          : { receivedThrough: "2", processedThrough: "0", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string) => {
        mutations.push(command);
        if (command === "apply_pending") {
          applySteps += 1;
          if (applySteps > 1) return { applied: 0, quarantined: 0, drained: 0 };
          applied = true;
          return { applied: 2, quarantined: 0, drained: 0 };
        }
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }, { cursor: "2", envelope: {} }], high_water_cursor: "2", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value) }).poll();

    expect(mutations.slice(0, 2)).toEqual(["ingest_batch", "apply_pending"]);
    expect(progress).toEqual([
      { phase: "receiving", receivedThrough: "2", processedThrough: "0", targetHighWater: "2" },
      { phase: "current", receivedThrough: "2", processedThrough: "2", targetHighWater: "2" },
    ]);
  });

  it("reports waiting-for-keys without claiming replay is current", async () => {
    const progress: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "sync_progress") return { receivedThrough: "1", processedThrough: "0", waitingForKeys: "1" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string) => command === "apply_pending"
        ? { applied: 0, quarantined: 0, drained: 0 }
        : { applied: 0, quarantined: 0, drained: 0 },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }], high_water_cursor: "1", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value.phase) }).poll();

    expect(progress).toEqual(["waitingForKeys"]);
  });

  it("settles an empty caught-up page to current instead of leaving receiving", async () => {
    const progress: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "5" };
        if (command === "sync_progress") return { receivedThrough: "5", processedThrough: "5", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [], high_water_cursor: "5", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value.phase) }).poll();

    expect(progress).toEqual(["current"]);
  });

  it("settles a duplicates-only page to current after its ingest checkpoint", async () => {
    const mutations: string[] = [];
    const progress: Array<{ phase: string; receivedThrough: string; processedThrough: string }> = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "2" };
        if (command === "sync_progress") return { receivedThrough: "2", processedThrough: "2", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string) => {
        mutations.push(command);
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }, { cursor: "2", envelope: {} }], high_water_cursor: "2", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value) }).poll();

    expect(mutations).toContain("ingest_batch");
    expect(progress.map(value => value.phase)).toEqual(["receiving", "current"]);
    expect(progress.at(-1)).toEqual({ phase: "current", receivedThrough: "2", processedThrough: "2", targetHighWater: "2" });
  });

  it("settles a quarantine-only page to current once apply reports the work done", async () => {
    const progress: string[] = [];
    let applyCalls = 0;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "1" };
        if (command === "sync_progress") return { receivedThrough: "1", processedThrough: "1", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string) => {
        if (command !== "apply_pending") return {};
        applyCalls += 1;
        return applyCalls === 1 ? { applied: 0, quarantined: 1, drained: 0 } : { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }], high_water_cursor: "1", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value.phase) }).poll();

    expect(applyCalls).toBeGreaterThan(0);
    expect(progress.at(-1)).toBe("current");
  });

  it("keeps the page applying while pending apply work remains, even when frontiers match the target", async () => {
    const progress: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "2" };
        if (command === "sync_progress") return { receivedThrough: "2", processedThrough: "2", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: true };
        return [];
      },
      mutate: async () => ({ applied: 0, quarantined: 0, drained: 0 }),
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }, { cursor: "2", envelope: {} }], high_water_cursor: "2", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value.phase) }).poll();

    expect(progress).toEqual(["receiving", "applying"]);
  });

  it("never publishes a post-checkpoint phase when apply fails after ingest", async () => {
    const progress: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "sync_progress") return { receivedThrough: "1", processedThrough: "1", waitingForKeys: "0" };
        if (command === "sync_work_status") return { pendingApply: false };
        return [];
      },
      mutate: async (command: string) => {
        if (command === "apply_pending") throw new Error("apply failed");
        return {};
      },
    };
    await expect(new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }], high_water_cursor: "1", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value.phase) }).poll()).resolves.toEqual({ connection: "error" });

    expect(progress).toEqual(["receiving"]);
  });

  it("splits replay batches by encoded dispatch size without reordering events", async () => {
    const batches: unknown[][] = [];
    const events = Array.from({ length: 3 }, (_, index) => ({ cursor: String(index + 1), envelope: { payload: "x".repeat(3_000_000) } }));
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "receive_cursor" ? { cursor: "0" } : command === "sync_work_status" ? { pendingApply: false } : [],
      mutate: async (command: string, args: Record<string, unknown>) => {
        if (command === "ingest_batch") batches.push(args.events as unknown[]);
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events, next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }).poll();

    expect(batches.map(batch => batch.length)).toEqual([2, 1]);
    expect(batches.flat().map(event => (event as { cursor: string }).cursor)).toEqual(["1", "2", "3"]);
    for (const batch of batches) {
      expect(new TextEncoder().encode(JSON.stringify({ command: "ingest_batch", args: { events: batch } })).byteLength).toBeLessThanOrEqual(8 * 1024 * 1024 - 64 * 1024);
    }
  });

  it.each([
    ["ASCII", (n: number) => "x".repeat(n)],
    ["multi-byte UTF-8", (n: number) => "é".repeat(Math.floor(n / 2)) + "x".repeat(n % 2)],
  ])("packs replay batches to the exact dispatch byte boundary (%s)", async (_label, payloadOf) => {
    const cap = 8 * 1024 * 1024 - 64 * 1024;
    const encoder = new TextEncoder();
    const dispatchBytes = (events: unknown[]) => encoder.encode(JSON.stringify({ command: "ingest_batch", args: { events } })).byteLength;
    const eventWithBytes = (cursor: string, target: number) => {
      const base = encoder.encode(JSON.stringify({ cursor, envelope: { payload: "" } })).byteLength;
      return { cursor, envelope: { payload: payloadOf(target - base) } };
    };
    const emptyBytes = dispatchBytes([]);
    const runWith = async (events: unknown[]) => {
      const batches: unknown[][] = [];
      const session = {
        hydrate: async () => undefined,
        query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "receive_cursor" ? { cursor: "0" } : command === "sync_work_status" ? { pendingApply: false } : [],
        mutate: async (command: string, args: Record<string, unknown>) => {
          if (command === "ingest_batch") batches.push(args.events as unknown[]);
          return { applied: 0, quarantined: 0, drained: 0 };
        },
      };
      await new BrowserScheduler(session, {
        json: async path => path.startsWith("/v1/events") ? { events, next_after: null } : {},
        upload: async () => undefined,
        download: async () => new Uint8Array(),
      }).poll();
      return batches;
    };

    // Two events whose exact dispatch body (wrapper + events + one comma) lands precisely on the cap.
    const total = cap - emptyBytes;
    const first = eventWithBytes("1", Math.floor((total - 1) / 2));
    const second = eventWithBytes("2", total - 1 - Math.floor((total - 1) / 2));
    const fits = [first, second];
    const fitBatches = await runWith(fits);
    expect(fitBatches).toEqual([fits]);
    expect(dispatchBytes(fitBatches[0])).toBe(cap);

    // One byte more must split without reordering.
    const overBy = eventWithBytes("2", total - 1 - Math.floor((total - 1) / 2) + 1);
    const splitBatches = await runWith([first, overBy]);
    expect(splitBatches).toEqual([[first], [overBy]]);
    for (const batch of splitBatches) expect(dispatchBytes(batch)).toBeLessThanOrEqual(cap);
  });

  it("caps replay batches at 100 events even when bytes remain", async () => {
    const events = Array.from({ length: 101 }, (_, index) => ({ cursor: String(index + 1), envelope: { payload: "é" } }));
    const batches: unknown[][] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "receive_cursor" ? { cursor: "0" } : command === "sync_work_status" ? { pendingApply: false } : [],
      mutate: async (command: string, args: Record<string, unknown>) => {
        if (command === "ingest_batch") batches.push(args.events as unknown[]);
        return { applied: 0, quarantined: 0, drained: 0 };
      },
    };
    await new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events, next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }).poll();

    expect(batches.map(batch => batch.length)).toEqual([100, 1]);
    expect(batches.flat()).toEqual(events);
  });

  it("rejects an oversized replay event before a checkpoint or apply", async () => {
    const mutations: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_outbox" ? { envelopes: [] } : command === "receive_cursor" ? { cursor: "0" } : [],
      mutate: async (command: string) => { mutations.push(command); return {}; },
    };
    const scheduler = new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: { payload: "x".repeat(9_000_000) } }], next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    });

    await expect(scheduler.poll()).resolves.toEqual({ connection: "error" });
    expect(mutations).not.toContain("ingest_batch");
    expect(mutations).not.toContain("apply_pending");
  });

  it.each([new CoreRejectedError("core"), new Error("checkpoint failed")])("does not apply or observe history after a failed batch checkpoint", async failure => {
    const mutations: string[] = [];
    const progress: unknown[] = [];
    const attemptId = "018f2c5e-6a3b-7c4d-8e5f-0123456789ab";
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_outbox") return { envelopes: [] };
        if (command === "receive_cursor") return { cursor: "0" };
        if (command === "_worker_history_search_sync_context") return {};
        return [];
      },
      mutate: async (command: string) => {
        mutations.push(command);
        if (command === "ingest_batch") throw failure;
        return {};
      },
    };
    const scheduler = new BrowserScheduler(session, {
      json: async path => path.startsWith("/v1/events") ? { events: [{ cursor: "1", envelope: {} }], high_water_cursor: "1", next_after: null } : {},
      upload: async () => undefined,
      download: async () => new Uint8Array(),
    }, { onProgress: value => progress.push(value) });
    scheduler.trackHistorySearch(attemptId);

    await expect(scheduler.poll()).resolves.toEqual({ connection: "error" });
    expect(mutations).not.toContain("apply_pending");
    expect(mutations).not.toContain("_worker_observe_history_search_sync");
    expect(progress).toEqual([]);
  });
});

it("replays and applies server events after a photo reference request fails", async () => {
  const mutations: string[] = [];
  let photoStateCalls = 0;
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_photo_transfer_state") return photoStateCalls++ === 1
        ? { uploads: [], registrations: [{ attachment_id: "photo", envelope_id: "envelope", producer_device_id: "producer", producer_sequence: "1" }], reclaims: [] }
        : { uploads: [], registrations: [], reclaims: [] };
      if (command === "contact_repair_status") return { repairRequired: false };
      if (command === "pending_outbox") return { envelopes: [] };
      if (command === "_worker_draft_bootstrap_required") return { required: false };
      if (command === "_worker_draft_sync_cursor") return { cursor: "0" };
      if (command === "_worker_draft_pending_uploads") return { uploads: [] };
      if (command === "_worker_draft_pending_pushes") return { pushes: [] };
      if (command === "_worker_draft_pending_discards") return { draftIds: [] };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => {
      mutations.push(command);
      if (command === "_worker_draft_apply_changes") return { more: false, nextAfter: "0" };
      return command === "apply_pending" ? { applied: mutations.filter(value => value === "apply_pending").length === 1 ? 1 : 0, quarantined: 0, drained: 0 } : {};
    },
  };
  const transport = {
    discard: async (path: string) => {
      if (path.endsWith("/references")) throw new (await import("./transport.js")).TransportError("status", 503, "busy");
    },
    json: async (path: string) => {
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path === "/v1/snapshot") return { compaction_supported: true, compaction_active: true, compaction_generation: "1" };
      if (path.startsWith("/v1/events")) return { events: [{ cursor: "1", envelope: {} }], next_after: null };
      if (path === "/v1/devices") return { devices: [] };
      if (path.startsWith("/v1/drafts")) return { changes: [], next_after: "0", more: false };
      return { capabilities: [] };
    },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };

  await expect(new BrowserScheduler(session, transport).poll()).resolves.toEqual({ connection: "connected" });
  expect(mutations).toContain("ingest_batch");
  expect(mutations).toContain("apply_pending");
});

it("replays and applies server events when a photo release proof requests resync", async () => {
  const mutations: string[] = [];
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [{ attachment_id: "photo", remote_object_id: "remote", release_after_cursor: "1" }] };
      if (command === "contact_repair_status") return { repairRequired: false };
      if (command === "pending_outbox") return { envelopes: [] };
      if (command === "_worker_draft_bootstrap_required") return { required: false };
      if (command === "_worker_draft_sync_cursor") return { cursor: "0" };
      if (command === "_worker_draft_pending_uploads") return { uploads: [] };
      if (command === "_worker_draft_pending_pushes") return { pushes: [] };
      if (command === "_worker_draft_pending_discards") return { draftIds: [] };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => { mutations.push(command); if (command === "_worker_draft_apply_changes") return { more: false, nextAfter: "0" }; return command === "apply_pending" ? { applied: 0, quarantined: 0, drained: 0 } : {}; },
  };
  let eventRequests = 0;
  const transport = {
    discard: async () => undefined,
    json: async (path: string) => {
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path === "/v1/snapshot") return { compaction_supported: true, compaction_active: true, compaction_generation: "1" };
      if (path.startsWith("/v1/events")) {
        eventRequests += 1;
        if (eventRequests === 1) throw new (await import("./transport.js")).TransportError("status", 409, "resync_required");
        return { events: [{ cursor: "1", envelope: {} }], next_after: null };
      }
      if (path === "/v1/devices") return { devices: [] };
      if (path.startsWith("/v1/drafts")) return { changes: [], next_after: "0", more: false };
      return { capabilities: [] };
    },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };

  await expect(new BrowserScheduler(session, transport).poll()).resolves.toEqual({ connection: "connected" });
  expect(mutations).toContain("ingest_batch");
  expect(mutations).toContain("apply_pending");
});

it("starts one contact repair and drains its remaining apply work across later polls", async () => {
  let remainingApply = 0;
  let snapshotStarts = 0;
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: remainingApply > 0, pendingSnapshot: false };
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [] };
      if (command === "contact_repair_status") return { repairRequired: true };
      if (command === "snapshot_progress") return { progress: null };
      if (command === "pending_outbox") return { envelopes: [] };
      if (command === "_worker_draft_bootstrap_required") return { required: false };
      if (command === "_worker_draft_sync_cursor") return { cursor: "0" };
      if (command === "_worker_draft_pending_uploads") return { uploads: [] };
      if (command === "_worker_draft_pending_pushes") return { pushes: [] };
      if (command === "_worker_draft_pending_discards") return { draftIds: [] };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => {
      if (command === "_worker_draft_apply_changes") return { more: false, nextAfter: "0" };
      if (command === "begin_snapshot") { snapshotStarts += 1; return { generation: "1", highWater: "1", expectedRecords: "0", receivedRecords: "0", lastCursor: "0" }; }
      if (command === "finish_snapshot") { remainingApply = 20; return {}; }
      if (command === "apply_pending") { const applied = remainingApply > 0 ? 1 : 0; remainingApply -= applied; return { applied, quarantined: 0, drained: 0 }; }
      return {};
    },
  };
  const transport = {
    discard: async () => undefined,
    json: async (path: string) => {
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path === "/v1/snapshot") return { compaction_supported: true, compaction_active: true, compaction_generation: "1", high_water_cursor: "1", record_count: "0" };
      if (path.startsWith("/v1/events")) return { events: [] };
      if (path === "/v1/devices") return { devices: [] };
      if (path.startsWith("/v1/drafts")) return { changes: [], next_after: "0", more: false };
      return { capabilities: [] };
    },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };
  const scheduler = new BrowserScheduler(session, transport);

  await scheduler.poll();
  await scheduler.poll();
  await scheduler.poll();

  expect(snapshotStarts).toBe(1);
  expect(remainingApply).toBe(0);
});

it("uses core upload classification in reservation instead of a host default", async () => {
    let reservedBody: unknown;
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_uploads") return [{ id: "a", ciphertextBytes: 3 }];
        if (command === "cipher_file") return { file: "cipher", ciphertextSha256: "hash" };
        if (command === "contact_photo_transfer_state") return { uploads: [] };
        if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "message_media", referenceTracking: true };
        if (command === "pending_downloads") return [];
        throw new Error(command);
      },
      mutate: async () => ({}),
    };
    await transferMedia(session, {
      json: async (path: string, request: { body?: unknown }) => { if (path.endsWith("reserve")) { reservedBody = request.body; return { attachment_id: "a" }; } return { attachment_id: "a" }; },
      upload: async () => undefined, download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array([1, 2, 3]), stageDownload: async () => "unused" }, new AbortController().signal);

    expect(reservedBody).toMatchObject({ retention_class: "message_media", reference_tracking: true });
  });

it("checkpoints exact download removal while preserving local storage and ignores generic 410", async () => {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_uploads") return [];
        if (command === "pending_downloads") return [{ id: "a", ciphertextBytes: 3, remoteObjectId: "remote" }];
        if (command === "contact_photo_transfer_state") return { uploads: [] };
        throw new Error(command);
      },
      mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
    };
    const policyRemoved = new (await import("./transport.js")).TransportError("status", 410, "attachment_removed_by_storage_policy");
    await transferMedia(session, {
      json: async () => ({}), upload: async () => undefined, download: async () => { throw policyRemoved; },
    }, { readCiphertext: async () => new Uint8Array(), stageDownload: async () => "must-not-stage" }, new AbortController().signal);

    expect(mutations).toContainEqual(["_worker_attachment_retention_report_removed", { attachmentId: "a" }]);
    expect(mutations.some(value => Array.isArray(value) && value[0] === "install_downloaded_attachment")).toBe(false);
  });

it("checkpoints exact policy removal from reserve upload and finalize while generic 404 and 410 remain retryable", async () => {
  const run = async (stage: "reserve" | "upload" | "finalize", error: TransportError) => {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => {
        if (command === "pending_uploads") return [{ id: "a", ciphertextBytes: 1 }];
        if (command === "pending_downloads") return [];
        if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "message_media", referenceTracking: false };
        if (command === "cipher_file") return { file: "cipher", ciphertextSha256: "hash" };
        throw new Error(command);
      },
      mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
    };
    const result = await transferMedia(session, {
      json: async (path: string) => {
        if (stage === "reserve" && path.endsWith("reserve") || stage === "finalize" && path.endsWith("finalize")) throw error;
        return { attachment_id: "a" };
      },
      upload: async () => { if (stage === "upload") throw error; },
      download: async () => new Uint8Array(),
    }, { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" }, new AbortController().signal);
    return { mutations, result };
  };

  for (const stage of ["reserve", "upload", "finalize"] as const) {
    const exact = await run(stage, new TransportError("status", 410, "attachment_removed_by_storage_policy"));
    expect(exact.mutations).toContainEqual(["_worker_attachment_retention_report_removed", { attachmentId: "a" }]);
    expect(exact.result.failedIds).toEqual([]);
  }
  for (const error of [new TransportError("status", 404), new TransportError("status", 410, "gone")]) {
    const generic = await run("reserve", error);
    expect(generic.mutations).not.toContainEqual(["_worker_attachment_retention_report_removed", { attachmentId: "a" }]);
    expect(generic.result.failedIds).toEqual(["a"]);
  }
});

it("defers producers on transient negotiation then registers before reserve after the next successful poll", async () => {
  const calls: string[] = [];
  let attempt = 0;
  let uploaded = false;
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      calls.push(`query:${command}`);
      if (command === "pending_uploads") return uploaded ? [] : [{ id: "a", ciphertextBytes: 1 }];
      if (command === "pending_downloads") return [];
      if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "message_media", referenceTracking: false };
      if (command === "cipher_file") return { file: "cipher", ciphertextSha256: "hash" };
      if (command === "pending_outbox") return { envelopes: [] };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "sync_work_status") return {};
      return [];
    },
    mutate: async (command: string) => { if (command === "mark_attachment_uploaded") uploaded = true; if (command === "ingest_batch") calls.push("incoming-media-use"); return { applied: 0, quarantined: 0, drained: 0 }; },
  };
  const transport = {
    json: async (path: string) => { calls.push(`http:${path}`); if (path.endsWith("reserve") || path.endsWith("finalize")) return { attachment_id: "a" }; return path.startsWith("/v1/events") && attempt > 1 ? { events: [{ cursor: "1", envelope: {} }], next_after: null } : { events: [] }; },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };
  const retention = {
    prepareRound: async () => { attempt += 1; if (attempt === 1) return { negotiationResolved: false, protocolEnabled: false, dueExhausted: false }; calls.push("retention:registered"); return { negotiationResolved: true, protocolEnabled: true, dueExhausted: true }; },
    reconcile: async () => { calls.push("retention:post-replay-use-drained"); return true; },
  };
  const scheduler = new BrowserScheduler(session, transport, {
    retention: retention as never,
    media: { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "unused" },
  });

  await scheduler.poll();
  expect(calls).not.toContain("query:pending_uploads");
  expect(calls).not.toContain("http:/v1/events?after=0&limit=100");
  await scheduler.poll();
  expect(calls.indexOf("retention:registered")).toBeLessThan(calls.indexOf("http:/v1/attachments/reserve"));
  expect(calls).toContain("incoming-media-use");
  expect(calls.indexOf("incoming-media-use")).toBeLessThan(calls.indexOf("retention:post-replay-use-drained"));
});

it("seals and sends text through the real protocol while retention backlog defers uploads", async () => {
  const calls: string[] = [];
  const sent: unknown[] = [];
  const envelope = { envelope_id: "text-envelope", purpose: "event", body: { ciphertext: "text-only" } };
  let sealed = false;
  let acknowledged = false;
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      calls.push(`query:${command}`);
      if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: false, server_active: false, backfill_complete: true };
      if (command === "contact_photo_transfer_state") return { uploads: [{ attachment_id: "photo" }], registrations: [], reclaims: [] };
      if (command === "sync_work_status") return { pendingSeal: !sealed, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_repair_status") return { repairRequired: false };
      if (command === "pending_uploads") return [{ id: "photo", ciphertextBytes: 1 }];
      if (command === "pending_downloads") return [];
      if (command === "pending_outbox") return { envelopes: sealed && !acknowledged ? [envelope] : [] };
      if (command === "_worker_draft_bootstrap_required") return { required: false };
      if (command === "_worker_draft_sync_cursor") return { cursor: "0" };
      if (command === "_worker_draft_pending_uploads") return { uploads: [] };
      if (command === "_worker_draft_pending_pushes") return { pushes: [] };
      if (command === "_worker_draft_pending_discards") return { draftIds: [] };
      if (command === "receive_cursor") return { cursor: "0" };
      return {};
    },
    mutate: async (command: string) => {
      calls.push(`mutate:${command}`);
      if (command === "_worker_draft_apply_changes") return { more: false, nextAfter: "0" };
      if (command === "seal_pending") {
        if (sealed) return { sealed: 0 };
        sealed = true;
        return { sealed: 1 };
      }
      if (command === "ack_outbox") acknowledged = true;
      return { applied: 0, quarantined: 0, drained: 0 };
    },
  };
  const transport = {
    discard: async () => undefined,
    json: async (path: string, request: { method: string; body?: unknown }) => {
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path === "/v1/snapshot") return { compaction_supported: false };
      if (path.startsWith("/v1/events") && request.method === "GET") return { events: [] };
      if (path.startsWith("/v1/drafts")) return { changes: [], next_after: "0", more: false };
      if (request.method === "POST" && request.body) sent.push(request.body);
      return {};
    },
    upload: async () => { calls.push("upload"); },
    download: async () => new Uint8Array(),
  };
  const retention = {
    prepareRound: async () => ({ negotiationResolved: true, protocolEnabled: true, dueExhausted: false }),
    reconcile: async () => false,
  };

  await new BrowserScheduler(session, transport, { retention: retention as never }).poll();

  expect(calls).toContain("mutate:seal_pending");
  expect(sent).toContainEqual(envelope);
  expect(calls).not.toContain("upload");
});

it("retention backlog blocks quota upload only while unrelated outbox and GET downloads continue", async () => {
  const calls: string[] = [];
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => {
      if (command === "pending_uploads") return [{ id: "upload", ciphertextBytes: 1 }];
      if (command === "pending_downloads") return [{ id: "download", ciphertextBytes: 1, remoteObjectId: "remote" }];
      if (command === "pending_outbox") return calls.includes("outbox") ? { envelopes: [] } : { envelopes: [{ envelope_id: "e", purpose: "event" }] };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "sync_work_status") return {};
      return [];
    },
    mutate: async (command: string) => { calls.push(command === "ack_outbox" ? "outbox" : command); return { applied: 0, quarantined: 0, drained: 0 }; },
  };
  const transport = {
    json: async (path: string) => { calls.push(path); return path.startsWith("/v1/events") ? { events: [] } : {}; },
    upload: async () => { calls.push("upload"); },
    download: async () => { calls.push("download"); return new Uint8Array([1]); },
  };
  const retention = {
    prepareRound: async () => ({ negotiationResolved: true, protocolEnabled: true, dueExhausted: false }),
    reconcile: async () => false,
  };

  await new BrowserScheduler(session, transport, {
    retention: retention as never,
    media: { readCiphertext: async () => new Uint8Array([1]), stageDownload: async () => "stage" },
  }).poll();

  expect(calls.some(call => call.startsWith("/v1/events"))).toBe(true);
  expect(calls).toContain("download");
  expect(calls).toContain("outbox");
  expect(calls).not.toContain("upload");
});

it("generic download 404 and non-policy 410 remain retryable without tombstones", async () => {
  for (const error of [new TransportError("status", 404), new TransportError("status", 410, "gone")]) {
    const mutations: unknown[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string) => command === "pending_uploads" ? [] : command === "pending_downloads" ? [{ id: "a", ciphertextBytes: 1, remoteObjectId: "remote" }] : [],
      mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
    };
    const result = await transferMedia(session, {
      json: async () => ({}), upload: async () => undefined, download: async () => { throw error; },
    }, { readCiphertext: async () => new Uint8Array(), stageDownload: async () => "unused" }, new AbortController().signal);
    expect(result.failedIds).toEqual(["a"]);
    expect(mutations).not.toContainEqual(["_worker_attachment_retention_report_removed", { attachmentId: "a" }]);
  }
});

const DRAFT_ID = "00000000-0000-0000-0000-000000000001";
const REVISION_ID = "00000000-0000-0000-0000-000000000002";

function draftSyncFixture(overrides: {
  draftJson: (path: string, request: { method: string; body?: unknown; signal?: AbortSignal; limit?: number }) => Promise<unknown>;
  pendingUploads?: unknown[];
  pendingPushes?: unknown[];
  pendingDiscards?: unknown[];
  pendingOutbox?: unknown[];
  media?: { readCiphertext(file: string, expectedBytes: number, signal: AbortSignal): Promise<Uint8Array>; stageDownload(bytes: Uint8Array, signal: AbortSignal): Promise<string> };
}) {
  const calls: string[] = [];
  const draftLimits: Array<number | undefined> = [];
  const mutations: Array<[string, Record<string, unknown>]> = [];
  const contexts: Record<string, unknown>[] = [];
  let pendingOutbox = overrides.pendingOutbox ?? [];
  const session = {
    hydrate: async (id: string) => { calls.push(`hydrate:${id}`); },
    query: async (command: string, args: Record<string, unknown>) => {
      calls.push(`query:${command}`);
      if (command === "_worker_identity_metadata") return { vaultId: "vault", deviceId: "device" };
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: false, server_active: false, backfill_complete: true };
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [] };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_repair_status") return { repairRequired: false };
      if (command === "pending_outbox") return { envelopes: pendingOutbox };
      if (command === "_worker_draft_bootstrap_required") return { required: true };
      if (command === "_worker_draft_pending_uploads") return { uploads: overrides.pendingUploads ?? [] };
      if (command === "_worker_draft_pending_pushes") return { pushes: overrides.pendingPushes ?? [] };
      if (command === "_worker_draft_pending_discards") return { draftIds: overrides.pendingDiscards ?? [] };
      if (command === "_worker_attachment_retention_upload_classification") return { retentionClass: "protected", referenceTracking: false };
      if (command === "cipher_file") return { file: "draft-cipher", ciphertextSha256: "hash" };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") { contexts.push(args); return {}; }
      throw new Error(`${command}:${JSON.stringify(args)}`);
    },
    mutate: async (command: string, args: Record<string, unknown>) => {
      calls.push(`mutate:${command}`);
      mutations.push([command, args]);
      if (command === "_worker_draft_apply_changes") {
        const changes = args as { more: boolean; next_after: string };
        return { more: changes.more, nextAfter: changes.next_after };
      }
      if (command === "_worker_draft_mark_push_attempt") return { attempted: true };
      if (command === "ack_outbox") pendingOutbox = [];
      if (command === "apply_pending") return { applied: 0, quarantined: 0, drained: 0 };
      return {};
    },
  };
  const transport = {
    request: "bound-transport",
    json: async function (this: { request: string }, path: string, request: { method: string; body?: unknown; signal?: AbortSignal; limit?: number }) {
      calls.push(`http:${path}`);
      if (path.startsWith("/v1/drafts")) {
        draftLimits.push(request.limit);
        return overrides.draftJson(path, request);
      }
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path === "/v1/snapshot") return { compaction_supported: false };
      if (path.startsWith("/v1/events")) return { events: [] };
      if (path === "/v1/devices") return { devices: [] };
      if (path.endsWith("/reserve") || path.endsWith("/finalize")) return { attachment_id: "attachment" };
      return { capabilities: [] };
    },
    upload: async (path: string) => { calls.push(`upload:${path}`); },
    download: async () => new Uint8Array(),
    discard: async function (this: { request: string }, path: string) {
      calls.push(`discard:${this.request}:${path}`);
    },
  };
  return { calls, contexts, draftLimits, mutations, session, transport, scheduler: new BrowserScheduler(session, transport, { media: overrides.media }) };
}

const pendingDraftPush = {
  draftId: DRAFT_ID,
  revisionId: REVISION_ID,
  request: { base_revision_id: null, sealed_record: "record", attachment_ids: [], authored: true },
};

const acceptedDraftPush = async (_path: string, request: { method: string }) =>
  request.method === "GET"
    ? { changes: [], next_after: "0", more: false }
    : { branch_id: DRAFT_ID, expires_at: "2026-12-01T00:00:00Z", conflict: false };

it("checkpoints a draft push attempt before its PUT", async () => {
  const fixture = draftSyncFixture({ draftJson: acceptedDraftPush, pendingPushes: [pendingDraftPush] });

  await fixture.scheduler.poll();

  const mark = fixture.calls.indexOf("mutate:_worker_draft_mark_push_attempt");
  const put = fixture.calls.findIndex(call => call === `http:/v1/drafts/${DRAFT_ID}/revisions/${REVISION_ID}`);
  expect(mark).toBeGreaterThan(-1);
  expect(mark).toBeLessThan(put);
});

it("skips a stale draft push attempt without a PUT", async () => {
  const fixture = draftSyncFixture({ draftJson: acceptedDraftPush, pendingPushes: [pendingDraftPush] });
  const mutate = fixture.session.mutate;
  fixture.session.mutate = async (command, args) => {
    if (command === "_worker_draft_mark_push_attempt") {
      fixture.mutations.push([command, args]);
      return { attempted: false };
    }
    return mutate(command, args);
  };

  await fixture.scheduler.poll();

  expect(fixture.mutations).toContainEqual(["_worker_draft_mark_push_attempt", { draftId: DRAFT_ID, revisionId: REVISION_ID }]);
  expect(fixture.calls).not.toContain(`http:/v1/drafts/${DRAFT_ID}/revisions/${REVISION_ID}`);
});

it("fails closed when the draft push checkpoint cannot commit", async () => {
  const fixture = draftSyncFixture({ draftJson: acceptedDraftPush, pendingPushes: [pendingDraftPush] });
  const mutate = fixture.session.mutate;
  fixture.session.mutate = async (command, args) => {
    if (command === "_worker_draft_mark_push_attempt") {
      fixture.mutations.push([command, args]);
      throw new Error("checkpoint failed");
    }
    return mutate(command, args);
  };

  expect(await fixture.scheduler.poll()).toEqual({ connection: "error" });
  expect(fixture.mutations).toContainEqual(["_worker_draft_mark_push_attempt", { draftId: DRAFT_ID, revisionId: REVISION_ID }]);
  expect(fixture.calls).not.toContain(`http:/v1/drafts/${DRAFT_ID}/revisions/${REVISION_ID}`);
});

it.each(["put", "record"])("surfaces a draft %s failure while continuing replay and clears it after reconciliation", async failure => {
  let shouldFail = true;
  const fixture = draftSyncFixture({
    draftJson: async (_path, request) => {
      if (request.method === "PUT" && shouldFail && failure === "put") throw new TransportError("status", 400, "invalid_draft");
      return acceptedDraftPush(_path, request);
    },
    pendingPushes: [pendingDraftPush],
  });
  const mutate = fixture.session.mutate;
  fixture.session.mutate = async (command, args) => {
    if (command === "_worker_draft_record_push" && shouldFail && failure === "record") throw new CoreRejectedError("core");
    return mutate(command, args);
  };

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  expect(fixture.calls).toContain("http:/v1/events?after=0&limit=100");

  shouldFail = false;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
});

it("retains a permanent draft failure through a transient PUT retry and clears it after reconciliation", async () => {
  let phase = 0;
  const fixture = draftSyncFixture({
    draftJson: async (path, request) => {
      if (request.method === "PUT" && phase === 0) throw new TransportError("status", 400, "invalid_draft");
      if (request.method === "PUT" && phase === 1) throw new TransportError("status", 503, "busy");
      return acceptedDraftPush(path, request);
    },
    pendingPushes: [pendingDraftPush],
  });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  expect(fixture.contexts.at(-1)).toMatchObject({ devices: { devices: [] }, capabilities: { capabilities: [] }, connection: "error" });
  expect(fixture.calls).toContain("http:/v1/events?after=0&limit=100");

  phase = 1;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  expect(fixture.contexts.at(-1)).toMatchObject({ connection: "error" });

  phase = 2;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
  expect(fixture.contexts.at(-1)).toMatchObject({ connection: "connected" });
});

it("retains a handled record failure through a transient PUT retry until the outcome records", async () => {
  let phase = 0;
  const fixture = draftSyncFixture({
    draftJson: async (path, request) => {
      if (request.method === "PUT" && phase === 1) throw new TransportError("status", 503, "busy");
      return acceptedDraftPush(path, request);
    },
    pendingPushes: [pendingDraftPush],
  });
  const mutate = fixture.session.mutate;
  fixture.session.mutate = async (command, args) => {
    if (command === "_worker_draft_record_push" && phase === 0) throw new CoreRejectedError("core");
    return mutate(command, args);
  };

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  phase = 1;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  phase = 2;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
  expect(fixture.contexts.at(-1)).toMatchObject({ connection: "connected" });
});

it("does not let a transient discard hide an actionable draft push failure", async () => {
  let recovered = false;
  const fixture = draftSyncFixture({
    draftJson: acceptedDraftPush,
    pendingPushes: [pendingDraftPush],
    pendingDiscards: [DRAFT_ID],
  });
  const mutate = fixture.session.mutate;
  fixture.session.mutate = async (command, args) => {
    if (command === "_worker_draft_record_push" && !recovered) throw new CoreRejectedError("core");
    return mutate(command, args);
  };
  fixture.transport.discard = async () => {
    if (!recovered) throw new TransportError("offline");
  };

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  expect(fixture.contexts.at(-1)).toMatchObject({ connection: "error" });
  expect(fixture.calls).toContain("http:/v1/events?after=0&limit=100");

  recovered = true;
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
  expect(fixture.contexts.at(-1)).toMatchObject({ connection: "connected" });
});

it("bootstraps every draft page before finishing and records a discard with its bound transport", async () => {
  const fixture = draftSyncFixture({
    draftJson: async path => path.includes("after=0")
      ? { changes: [{ draft_id: DRAFT_ID }], next_after: "1", more: true }
      : { changes: [{ draft_id: REVISION_ID }], next_after: "2", more: false },
    pendingDiscards: [DRAFT_ID],
  });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });

  expect(fixture.calls.filter(call => call.startsWith("http:/v1/drafts?"))).toEqual([
    "http:/v1/drafts?after=0&limit=2",
    "http:/v1/drafts?after=1&limit=2",
  ]);
  expect(fixture.mutations.filter(([command]) => command === "_worker_draft_apply_changes").map(([, args]) => args.next_after)).toEqual(["1", "2"]);
  const finish = fixture.mutations.find(([command]) => command === "_worker_draft_finish_bootstrap");
  expect(finish).toEqual(["_worker_draft_finish_bootstrap", { draftIds: [DRAFT_ID, REVISION_ID] }]);
  expect(fixture.calls.indexOf("mutate:_worker_draft_finish_bootstrap")).toBeGreaterThan(fixture.calls.indexOf("http:/v1/drafts?after=1&limit=2"));
  expect(fixture.calls).toContain(`discard:bound-transport:/v1/drafts/${DRAFT_ID}/discard`);
  expect(fixture.mutations).toContainEqual(["_worker_draft_record_discard", { draftId: DRAFT_ID }]);
});

it("pages two large valid draft records through the bounded transport", async () => {
  const sealedRecord = "a".repeat(350 * 1024);
  const fixture = draftSyncFixture({
    draftJson: async path => path.includes("after=0")
      ? { changes: [{ draft_id: DRAFT_ID, sealed_record: sealedRecord }, { draft_id: REVISION_ID, sealed_record: sealedRecord }], next_after: "2", more: true }
      : { changes: [], next_after: "2", more: false },
  });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });

  expect(fixture.mutations.filter(([command]) => command === "_worker_draft_apply_changes").map(([, args]) => args.next_after)).toEqual(["2"]);
  expect(fixture.calls.filter(call => call.startsWith("http:/v1/drafts?"))).toEqual([
    "http:/v1/drafts?after=0&limit=2",
    "http:/v1/drafts?after=2&limit=2",
  ]);
  expect(fixture.draftLimits).toEqual([1024 * 1024, 1024 * 1024]);
});

it("restarts a failed draft bootstrap from zero without finishing the partial checkpoint", async () => {
  let requests = 0;
  const fixture = draftSyncFixture({
    draftJson: async path => {
      requests += 1;
      if (requests === 2) throw new TransportError("offline");
      return path.includes("after=0")
        ? { changes: [{ draft_id: DRAFT_ID }], next_after: "1", more: requests === 1 }
        : { changes: [], next_after: "1", more: false };
    },
  });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
  expect(fixture.mutations.some(([command]) => command === "_worker_draft_finish_bootstrap")).toBe(false);
  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });

  expect(fixture.calls.filter(call => call === "http:/v1/drafts?after=0&limit=2")).toHaveLength(2);
  expect(fixture.mutations.filter(([command]) => command === "_worker_draft_finish_bootstrap")).toHaveLength(1);
});

it.each([
  [new TransportError("status", 503), "connected"],
  [new TransportError("status", 403, "forbidden", "json"), "error"],
] as const)("keeps ordinary outbox and replay moving when draft sync fails (%s)", async (error, connection) => {
  const fixture = draftSyncFixture({
    draftJson: async () => { throw error; },
    pendingOutbox: [{ purpose: "event", envelope_id: "outbox-1" }],
  });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection });

  expect(fixture.mutations).toContainEqual(["ack_outbox", { envelopeId: "outbox-1" }]);
  expect(fixture.calls).toContain("http:/v1/events");
  expect(fixture.calls).toContain("http:/v1/events?after=0&limit=100");
});

it("recognizes only documented draft-route fallbacks after authenticated evidence", async () => {
  const unsupported = async (error: TransportError, probeFails = false) => {
    const fixture = draftSyncFixture({ draftJson: async () => { throw error; } });
    if (probeFails) {
      const json = fixture.transport.json;
      let vaultRequests = 0;
      fixture.transport.json = async function (this: { request: string }, path, request) {
        if (path === "/v1/vault" && ++vaultRequests > 1) throw new TransportError("offline");
        return json.call(this, path, request);
      };
    }
    return fixture;
  };

  for (const error of [
    new TransportError("status", 404, "not_found", "json"),
    new TransportError("status", 405, "method_not_allowed", "json"),
    new TransportError("status", 404, undefined, "empty"),
  ]) {
    const fixture = await unsupported(error);
    await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
    expect(fixture.mutations).toContainEqual(["_worker_draft_sync_supported", { supported: false }]);
    expect(fixture.scheduler.isDraftSyncEnabled).toBe(false);
  }

  for (const [error, probeFails] of [
    [new TransportError("status", 405, undefined, "other"), false],
    [new TransportError("status", 404, undefined, "other"), false],
    [new TransportError("status", 404, undefined, "empty"), true],
  ] as const) {
    const fixture = await unsupported(error, probeFails);
    await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });
    expect(fixture.mutations).not.toContainEqual(["_worker_draft_sync_supported", { supported: false }]);
  }
});

it("skips unchanged empty draft pages after the initial support checkpoint", async () => {
  const fixture = draftSyncFixture({
    draftJson: async () => ({ changes: [], next_after: "0", more: false }),
  });

  await fixture.scheduler.poll();
  fixture.mutations.length = 0;
  fixture.calls.length = 0;
  await fixture.scheduler.poll();

  expect(fixture.mutations).not.toContainEqual(["_worker_draft_sync_supported", { supported: true }]);
  expect(fixture.mutations.some(([command]) => command === "_worker_draft_apply_changes")).toBe(false);
  expect(fixture.calls.some(call => call.startsWith("hydrate:"))).toBe(false);
});

it("reprobes drafts after an unsupported route instead of permanently latching local-only mode", async () => {
  let calls = 0;
  const fixture = draftSyncFixture({
    draftJson: async () => {
      calls += 1;
      if (calls === 1) throw new TransportError("status", 404, "not_found", "json");
      return { changes: [], next_after: "0", more: false };
    },
  });

  await fixture.scheduler.poll();
  await fixture.scheduler.poll();

  expect(calls).toBe(2);
  expect(fixture.scheduler.isDraftSyncEnabled).toBe(true);
  expect(fixture.mutations).toContainEqual(["_worker_draft_sync_supported", { supported: true }]);
});

it("records draft download failures durably while leaving ordinary media retries unchanged", async () => {
  const mutations: unknown[] = [];
  const session = {
    hydrate: async () => undefined,
    query: async (command: string) => command === "pending_uploads" ? [] : command === "pending_downloads"
      ? [{ id: "draft", draftId: DRAFT_ID, ciphertextBytes: 1, remoteObjectId: "remote" }, { id: "ordinary", ciphertextBytes: 1, remoteObjectId: "ordinary" }]
      : [],
    mutate: async (command: string, args: unknown) => { mutations.push([command, args]); return {}; },
  };

  await transferMedia(session, {
    json: async () => ({}), upload: async () => undefined, download: async () => { throw new TransportError("status", 503); },
  }, { readCiphertext: async () => new Uint8Array(), stageDownload: async () => "unused" }, new AbortController().signal);

  expect(mutations).toContainEqual(["_worker_draft_attachment_transfer_failure", { attachmentId: "draft", direction: "download" }]);
  expect(mutations).not.toContainEqual(["_worker_draft_attachment_transfer_failure", { attachmentId: "ordinary", direction: "download" }]);
});

it("does not finish a draft bootstrap when cancellation interrupts its next page", async () => {
  let secondPageRequested!: () => void;
  const secondPage = new Promise<void>(resolve => { secondPageRequested = resolve; });
  const fixture = draftSyncFixture({
    draftJson: async (path, request) => {
      if (path.includes("after=0")) return { changes: [{ draft_id: DRAFT_ID }], next_after: "1", more: true };
      secondPageRequested();
      return new Promise((_, reject) => request.signal?.addEventListener("abort", () => reject(new TransportError("cancelled")), { once: true }));
    },
  });

  const poll = fixture.scheduler.poll();
  await secondPage;
  fixture.scheduler.stop();
  await expect(poll).rejects.toMatchObject({ kind: "cancelled" });

  expect(fixture.mutations.some(([command]) => command === "_worker_draft_finish_bootstrap")).toBe(false);
});

it("propagates a revoked draft credential", async () => {
  const fixture = draftSyncFixture({ draftJson: async () => { throw new TransportError("revoked"); } });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "revoked" });
  expect(fixture.calls).not.toContain("query:pending_outbox");
});

it("does not continue after a 401 draft credential rejection", async () => {
  const fixture = draftSyncFixture({ draftJson: async () => { throw new TransportError("status", 401, "device_revoked", "json"); } });

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "error" });
  expect(fixture.calls).not.toContain("query:pending_outbox");
});

it("uploads and checkpoints draft attachments before pushing their draft revision", async () => {
  const events: string[] = [];
  const fixture = draftSyncFixture({
    draftJson: async path => path.includes("after=0")
      ? { changes: [], next_after: "0", more: false }
      : { branch_id: REVISION_ID, expires_at: "2025-01-01T00:00:00.000Z", conflict: false },
    pendingUploads: [{ attachmentId: "attachment", ciphertextBytes: 3, draftId: DRAFT_ID }],
    pendingPushes: [{ draftId: DRAFT_ID, revisionId: REVISION_ID, request: {} }],
    media: {
      readCiphertext: async () => { events.push("read"); return new Uint8Array([1, 2, 3]); },
      stageDownload: async () => "unused",
    },
  });
  const originalJson = fixture.transport.json;
  fixture.transport.json = async function (this: { request: string }, path, request) {
    if (path.endsWith("/reserve")) events.push(`reserve:${JSON.stringify(request.body)}`);
    if (path.includes("/revisions/")) events.push("push");
    return originalJson.call(this, path, request);
  };
  fixture.transport.upload = async path => { events.push(`upload:${path}`); };

  await expect(fixture.scheduler.poll()).resolves.toEqual({ connection: "connected" });

  expect(events).toEqual(expect.arrayContaining([
    `reserve:${JSON.stringify({ attachment_id: "attachment", declared_ciphertext_bytes: 3, declared_ciphertext_sha256: "hash", retention_class: "protected", reference_tracking: false, draft_id: DRAFT_ID })}`,
    "upload:/v1/attachments/attachment/upload",
    "push",
  ]));
  expect(fixture.mutations).toContainEqual(["_worker_draft_attachment_transfer_clear", { attachmentId: "attachment" }]);
  expect(events.indexOf("push")).toBeGreaterThan(events.findIndex(event => event.startsWith("upload:")));
});
