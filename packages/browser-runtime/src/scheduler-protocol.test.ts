import { describe, expect, it } from "vitest";
import { SchedulerProtocol } from "./scheduler-protocol.js";

const binding = { vaultId: "vault", deviceId: "device" };

describe("SchedulerProtocol", () => {
  it("declares compaction, drains bounded backfill and seals before acknowledging photo references", async () => {
    const calls: string[] = [];
    let photos = 0;
    const session = {
      query: async (command: string) => {
        if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
        if (command === "compaction_status") return { server_supported: false, server_active: false };
        if (command === "sync_work_status") return { pendingSeal: true, pendingApply: false, pendingSnapshot: false };
        if (command === "contact_photo_transfer_state") return photos++ === 0
          ? { uploads: [{ attachment_id: "photo" }], registrations: [], reclaims: [] }
          : { uploads: [], registrations: [{ attachment_id: "remote", envelope_id: "envelope", producer_device_id: "producer", producer_sequence: "7" }], reclaims: [] };
        if (command === "contact_repair_status") return { repairRequired: false };
        throw new Error(command);
      },
      mutate: async (command: string, args: Record<string, unknown>) => {
        calls.push(command);
        if (command === "set_server_compaction_support") return { backfill_complete: false };
        if (command === "compaction_backfill_step") return { backfill_complete: true, processed: 4 };
        if (command === "seal_pending") return { sealed: calls.filter(call => call === "seal_pending").length === 1 ? 1 : 0 };
        if (command === "acknowledge_contact_photo_reference") expect(args.envelope_id).toBe("envelope");
        return {};
      },
    };
    const transport = {
      json: async (path: string) => {
        if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
        if (path === "/v1/snapshot") return { compaction_supported: true, compaction_active: true, compaction_generation: "4" };
        if (path.endsWith("/references")) { calls.push("reference-http"); return {}; }
        return {};
      },
      discard: async (path: string) => { if (path.endsWith("/references")) calls.push("reference-http"); },
    };
    const protocol = new SchedulerProtocol(session, transport, binding);
    const result = await protocol.run(new AbortController().signal, { beforeSeal: async ids => expect(ids).toEqual(["photo"]) });
    expect(result.compactionNeedsWork).toBe(false);
    expect(calls).toEqual(["set_server_compaction_support", "compaction_backfill_step", "seal_pending", "seal_pending", "reference-http", "acknowledge_contact_photo_reference"]);
  });

  it("returns a manual epoch gate and runs a requested repair through the fenced scheduler hook", async () => {
    let repaired = false;
    const session = {
      query: async (command: string) => {
        if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
        if (command === "compaction_status") return { server_supported: false, server_active: false };
        if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
        if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [] };
        if (command === "contact_repair_status") return { repairRequired: true };
        throw new Error(command);
      },
      mutate: async (command: string) => command === "seal_pending" ? { sealed: 0 } : { backfill_complete: true, processed: 0 },
    };
    const transport = { json: async (path: string) => path === "/v1/vault"
      ? { vault_id: "vault", device_id: "device", key_epoch: 2, profile_fingerprint: "next", public_key_profile: {}, encrypted_vault_check_header: btoa("{}") }
      : { compaction_supported: true, compaction_active: true, compaction_generation: "3" }, discard: async () => undefined };
    const result = await new SchedulerProtocol(session, transport, binding).run(new AbortController().signal, { repairSnapshot: async () => { repaired = true; } });
    expect(result.vault.state).toBe("epoch-mismatch");
    expect(result.repairNeeded).toBe(true);
    expect(repaired).toBe(true);
  });

  it("keeps a new epoch manual until the Worker rotation callback succeeds", async () => {
    const session = { query: async (command: string) => command === "key_status" ? { activeEpoch: "1", unlockedEpochs: ["1"] } : command === "compaction_status" ? { server_supported: false, server_active: false } : command === "sync_work_status" ? { pendingSeal: false } : command === "contact_photo_transfer_state" ? { uploads: [], registrations: [], reclaims: [] } : { repairRequired: false }, mutate: async () => ({ backfill_complete: true }) };
    const transport = { discard: async () => undefined, json: async (path: string) => path === "/v1/vault" ? { vault_id: "vault", device_id: "device", key_epoch: 2, profile_fingerprint: "next", public_key_profile: { key_epoch: 2 }, encrypted_vault_check_header: btoa("{}") } : { compaction_supported: false } };
    const protocol = new SchedulerProtocol(session, transport, binding);
    await protocol.run(new AbortController().signal);
    let rotations = 0;
    await expect(protocol.unlockNewEpoch("wrong", async () => { rotations += 1; throw new Error("wrong"); })).rejects.toThrow("wrong");
    expect(rotations).toBe(1);
    const state = await protocol.unlockNewEpoch("correct", async profile => { expect(profile).toEqual({ key_epoch: 2 }); rotations += 1; });
    expect(state).toMatchObject({ state: "current", epoch: "2", fingerprint: "next" });
    expect(rotations).toBe(2);
  });
});

it("continues reference registration after a nonfatal item failure", async () => {
  const acknowledged: string[] = [];
  const session = {
    query: async (command: string) => {
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [
        { attachment_id: "failed", envelope_id: "envelope-1", producer_device_id: "producer", producer_sequence: "1" },
        { attachment_id: "accepted", envelope_id: "envelope-2", producer_device_id: "producer", producer_sequence: "2" },
      ], reclaims: [] };
      if (command === "contact_repair_status") return { repairRequired: false };
      throw new Error(command);
    },
    mutate: async (_command: string, args: Record<string, unknown>) => { acknowledged.push(String(args.attachment_id)); return {}; },
  };
  const transport = {
    json: async (path: string) => path === "/v1/vault"
      ? { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" }
      : { compaction_supported: true, compaction_active: true },
    discard: async (path: string) => {
      if (path.includes("failed")) throw new (await import("./transport.js")).TransportError("status", 503, "busy");
    },
  };

  await expect(new SchedulerProtocol(session, transport, binding).run(new AbortController().signal)).resolves.toBeDefined();
  expect(acknowledged).toEqual(["accepted"]);
});

it("skips photo reclamation when release proof fails nonfatally", async () => {
  let deleted = 0;
  const session = {
    query: async (command: string) => {
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "receive_cursor") return { cursor: "0" };
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [{ attachment_id: "photo", remote_object_id: "remote", release_after_cursor: "1" }] };
      if (command === "contact_repair_status") return { repairRequired: false };
      throw new Error(command);
    },
    mutate: async () => ({}),
  };
  const transport = {
    json: async (path: string) => {
      if (path === "/v1/vault") return { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" };
      if (path.startsWith("/v1/events")) throw new (await import("./transport.js")).TransportError("status", 409, "resync_required");
      return { compaction_supported: true, compaction_active: true, compaction_generation: "3" };
    },
    discard: async (path: string) => { if (path.includes("remote")) deleted += 1; },
  };

  await expect(new SchedulerProtocol(session, transport, binding).run(new AbortController().signal)).resolves.toBeDefined();
  expect(deleted).toBe(0);
});

it("starts contact repair once and waits while snapshot or apply work is pending", async () => {
  let work = { pendingSeal: false, pendingApply: true, pendingSnapshot: false };
  let repaired = 0;
  const session = {
    query: async (command: string) => {
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return work;
      if (command === "contact_photo_transfer_state") return { uploads: [], registrations: [], reclaims: [] };
      if (command === "contact_repair_status") return { repairRequired: true };
      throw new Error(command);
    },
    mutate: async () => ({}),
  };
  const transport = {
    json: async (path: string) => path === "/v1/vault"
      ? { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" }
      : { compaction_supported: true, compaction_active: true, compaction_generation: "3" },
    discard: async () => undefined,
  };
  const protocol = new SchedulerProtocol(session, transport, binding);
  const media = { repairSnapshot: async () => { repaired += 1; throw new Error("nonfatal repair failure"); } };

  await protocol.run(new AbortController().signal, media);
  expect(repaired).toBe(0);
  work = { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
  await protocol.run(new AbortController().signal, media);
  await protocol.run(new AbortController().signal, media);
  expect(repaired).toBe(1);
});

it("preserves revocation from photo reference registration", async () => {
  let photoStateCalls = 0;
  const session = {
    query: async (command: string) => {
      if (command === "key_status") return { activeEpoch: "1", unlockedEpochs: ["1"] };
      if (command === "compaction_status") return { server_supported: true, server_active: true, backfill_complete: true };
      if (command === "sync_work_status") return { pendingSeal: false, pendingApply: false, pendingSnapshot: false };
      if (command === "contact_photo_transfer_state") return photoStateCalls++ === 1
        ? { uploads: [], registrations: [{ attachment_id: "photo", envelope_id: "envelope", producer_device_id: "producer", producer_sequence: "1" }], reclaims: [] }
        : { uploads: [], registrations: [], reclaims: [] };
      throw new Error(command);
    },
    mutate: async () => ({}),
  };
  const transport = {
    json: async (path: string) => path === "/v1/vault"
      ? { vault_id: "vault", device_id: "device", key_epoch: 1, profile_fingerprint: "fingerprint" }
      : { compaction_supported: true, compaction_active: true },
    discard: async (path: string) => {
      if (path.endsWith("/references")) throw new (await import("./transport.js")).TransportError("revoked");
    },
  };

  await expect(new SchedulerProtocol(session, transport, binding).run(new AbortController().signal)).rejects.toMatchObject({ kind: "revoked" });
});
