import { describe, expect, it } from "vitest";
import { BrowserScheduler } from "./scheduler.js";
import { transferMedia } from "./scheduler-media.js";

describe("BrowserScheduler", () => {
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
    expect(contexts).toEqual([{ devices, capabilities, connection: "connected" }]);
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
    replayed = mutations.includes("ingest");
    expect(mutations).not.toContain("ack_outbox");
    expect(replayed).toBe(true);
  });

  it("continues with later uploads after one attachment fails", async () => {
    const marked: string[] = [];
    const session = {
      hydrate: async () => undefined,
      query: async (command: string, args: Record<string, unknown>) => {
        if (command === "pending_uploads") return [{ id: "bad", ciphertextBytes: 1 }, { id: "good", ciphertextBytes: 1 }];
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
      query: async (command: string) => command === "pending_uploads" ? [{ id: "a", ciphertextBytes: 1 }] : command === "pending_downloads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : { file: "a", ciphertextSha256: "hash" },
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
      query: async (command: string) => command === "pending_uploads" ? [{ id: "a", ciphertextBytes: 1 }] : command === "pending_downloads" ? [] : command === "contact_photo_transfer_state" ? { uploads: [] } : { file: "a", ciphertextSha256: "hash" },
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
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => {
      mutations.push(command);
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
      return { capabilities: [] };
    },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };

  await expect(new BrowserScheduler(session, transport).poll()).resolves.toEqual({ connection: "connected" });
  expect(mutations).toContain("ingest");
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
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => { mutations.push(command); return command === "apply_pending" ? { applied: 0, quarantined: 0, drained: 0 } : {}; },
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
      return { capabilities: [] };
    },
    upload: async () => undefined,
    download: async () => new Uint8Array(),
  };

  await expect(new BrowserScheduler(session, transport).poll()).resolves.toEqual({ connection: "connected" });
  expect(mutations).toContain("ingest");
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
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "_worker_apply_server_context") return {};
      throw new Error(command);
    },
    mutate: async (command: string) => {
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
