import { TransportError } from "./transport.js";

const MAX_BACKFILL_ROUNDS = 8;
const MAX_SEAL_ROUNDS = 8;
const BACKFILL_LIMIT = 1_000;

export interface ProtocolSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
}

export interface ProtocolTransport {
  json(path: string, request: { method: "GET" | "POST" | "DELETE"; body?: unknown; signal?: AbortSignal; limit?: number }): Promise<unknown>;
  discard(path: string, request: { method: "POST" | "DELETE"; body?: unknown; signal?: AbortSignal }): Promise<void>;
}

export interface VaultBinding {
  vaultId: string;
  deviceId: string;
}

export interface VaultRefresh {
  state: "current" | "needs-unlock" | "epoch-mismatch";
  epoch: string;
  fingerprint: string;
}

export interface ProtocolMediaHooks {
  /** Upload contact-photo IDs with `reference_tracking: true` before sealing their envelopes. */
  beforeSeal?(attachmentIds: readonly string[], signal: AbortSignal): Promise<void>;
  /** Run ordinary media transfer only after photo references have been acknowledged. */
  afterReferences?(signal: AbortSignal): Promise<void>;
  /** Import one server-fenced snapshot when the core has latched contact repair. */
  repairSnapshot?(signal: AbortSignal): Promise<void>;
}

export interface ProtocolRound {
  vault: VaultRefresh;
  compactionNeedsWork: boolean;
  sealNeedsWork: boolean;
  repairNeeded: boolean;
}

export type RotateEpoch = (profile: unknown, header: unknown, passphrase: string) => Promise<void>;

/** Worker-only native protocol coordinator. It never exposes vault headers, keys, or phrases. */
export class SchedulerProtocol {
  private pendingEpoch?: { epoch: string; fingerprint: string; profile: unknown; header: unknown };
  private repairAttempted = false;
  public constructor(
    private readonly session: ProtocolSession,
    private readonly transport: ProtocolTransport,
    private readonly binding: VaultBinding,
  ) {}

  public async run(signal: AbortSignal, media: ProtocolMediaHooks = {}): Promise<ProtocolRound> {
    throwIfAborted(signal);
    const vault = await this.refreshVault(signal);
    const compactionNeedsWork = await nonfatalValue(() => this.declareCompaction(signal), false, signal);
    const transfer = await this.photoTransferState(signal);
    const uploads = await nonfatalValue(async () => stringIds(transfer.uploads, "attachment_id"), [], signal);
    const beforeSeal = media.beforeSeal;
    const uploadsReady = !beforeSeal || await nonfatal(() => beforeSeal(uploads, signal), signal);
    const work = await this.syncWorkStatus(signal);
    const sealNeedsWork = uploadsReady ? await nonfatalValue(() => this.sealPending(work.pendingSeal === true, signal), false, signal) : false;
    const afterSeal = await this.photoTransferState(signal);
    await this.registerReferences(afterSeal.registrations, signal);
    const afterReferences = media.afterReferences;
    if (afterReferences) await nonfatal(() => afterReferences(signal), signal);
    const afterMedia = await this.photoTransferState(signal);
    await this.reclaimPhotos(afterMedia.reclaims, signal);
    const repairNeeded = await this.repairSnapshotIfRequested(signal, media, work);
    return { vault, compactionNeedsWork, sealNeedsWork, repairNeeded };
  }

  private async refreshVault(signal: AbortSignal): Promise<VaultRefresh> {
    const vault = object(await this.transport.json("/v1/vault", { method: "GET", signal }));
    if (requiredString(vault.vault_id) !== this.binding.vaultId || requiredString(vault.device_id) !== this.binding.deviceId) throw new TransportError("invalid");
    const epoch = canonicalEpoch(vault.key_epoch);
    const fingerprint = requiredString(vault.profile_fingerprint);
    const status = object(await this.session.query("key_status", {}));
    const activeEpoch = optionalDecimal(status.activeEpoch);
    const unlocked = stringArray(status.unlockedEpochs);
    if (activeEpoch === epoch) return { state: unlocked.includes(epoch) ? "current" : "needs-unlock", epoch, fingerprint };
    this.pendingEpoch = { epoch, fingerprint, profile: object(vault.public_key_profile), header: decodeVaultHeader(requiredString(vault.encrypted_vault_check_header)) };
    return { state: "epoch-mismatch", epoch, fingerprint };
  }

  /** Applies a server-validated pending epoch only after an explicit Worker-held passphrase. */
  public async unlockNewEpoch(passphrase: string, rotate: RotateEpoch, signal?: AbortSignal): Promise<VaultRefresh> {
    if (!this.pendingEpoch || passphrase.length === 0) throw new TransportError("invalid");
    if (signal?.aborted) throw new TransportError("cancelled");
    await rotate(this.pendingEpoch.profile, this.pendingEpoch.header, passphrase);
    const applied = this.pendingEpoch;
    this.pendingEpoch = undefined;
    return { state: "current", epoch: applied.epoch, fingerprint: applied.fingerprint };
  }

  private async declareCompaction(signal: AbortSignal): Promise<boolean> {
    let snapshot: Record<string, unknown>;
    try {
      await this.transport.discard("/v1/compaction/capability", { method: "POST", body: {}, signal });
      snapshot = object(await this.transport.json("/v1/snapshot", { method: "GET", signal }));
    } catch (error) {
      throwIfFatal(error, signal);
      const current = object(await this.session.query("compaction_status", {}));
      if (current.server_supported === true || current.server_active === true) {
        await this.session.mutate("set_server_compaction_support", { supported: false, active: false });
      }
      return false;
    }
    const supported = snapshot.compaction_supported === true;
    const active = supported && snapshot.compaction_active === true;
    const current = object(await this.session.query("compaction_status", {}));
    const readiness = current.server_supported === supported && current.server_active === active
      ? current
      : object(await this.session.mutate("set_server_compaction_support", { supported, active }));
    let needsWork = readiness.backfill_complete !== true;
    for (let round = 0; supported && needsWork && round < MAX_BACKFILL_ROUNDS; round += 1) {
      throwIfAborted(signal);
      const step = object(await this.session.mutate("compaction_backfill_step", { limit: BACKFILL_LIMIT }));
      needsWork = step.backfill_complete !== true;
      if (nonNegative(step.processed) === 0) break;
    }
    return needsWork;
  }

  private async sealPending(hasPendingSeal: boolean, signal: AbortSignal): Promise<boolean> {
    if (!hasPendingSeal) return false;
    for (let round = 0; round < MAX_SEAL_ROUNDS; round += 1) {
      throwIfAborted(signal);
      const sealed = nonNegative(object(await this.session.mutate("seal_pending", { limit: BACKFILL_LIMIT })).sealed);
      if (sealed === 0) return false;
    }
    return true;
  }

  private async registerReferences(value: unknown, signal: AbortSignal): Promise<void> {
    for (const item of array(value)) {
      throwIfAborted(signal);
      await nonfatal(async () => {
        const registration = object(item);
        const attachmentId = requiredString(registration.attachment_id);
        const envelopeId = requiredString(registration.envelope_id);
        await this.transport.discard(`/v1/attachments/${encodeURIComponent(attachmentId)}/references`, {
          method: "POST", body: { references: [{ producer_device_id: requiredString(registration.producer_device_id), producer_sequence: canonicalDecimal(registration.producer_sequence) }] }, signal,
        });
        await this.session.mutate("acknowledge_contact_photo_reference", { schema_version: 1, envelope_id: envelopeId, attachment_id: attachmentId });
      }, signal);
    }
  }

  private async reclaimPhotos(value: unknown, signal: AbortSignal): Promise<void> {
    const reclaims = array(value);
    if (reclaims.length === 0) return;
    const proof = await nonfatalValue(() => this.releaseProof(signal), undefined, signal);
    if (!proof) return;
    for (const item of reclaims) {
      throwIfAborted(signal);
      await nonfatal(async () => {
        const reclaim = object(item);
        const attachmentId = requiredString(reclaim.attachment_id);
        const remoteObjectId = requiredString(reclaim.remote_object_id);
        const releaseAfter = optionalDecimal(reclaim.release_after_cursor);
        if (!releaseAfter || BigInt(proof.floor) < BigInt(releaseAfter)) return;
        const status = await this.deletePhoto(remoteObjectId, proof, signal);
        await this.session.mutate("acknowledge_contact_photo_reclaim", { schema_version: 1, attachment_id: attachmentId, http_status: status });
      }, signal);
    }
  }

  private async releaseProof(signal: AbortSignal): Promise<{ generation: string; floor: string } | undefined> {
    const snapshot = object(await this.transport.json("/v1/snapshot", { method: "GET", signal }));
    if (snapshot.compaction_supported !== true) return undefined;
    const generation = canonicalDecimal(snapshot.compaction_generation);
    const cursor = requiredString(object(await this.session.query("receive_cursor", {})).cursor);
    const marks = object(await this.transport.json(`/v1/events?after=${encodeURIComponent(cursor)}&limit=1`, { method: "GET", signal }));
    return { generation, floor: canonicalDecimal(marks.replay_floor_cursor) };
  }

  private async deletePhoto(remoteObjectId: string, proof: { generation: string; floor: string }, signal: AbortSignal): Promise<number> {
    try {
      await this.transport.discard(`/v1/attachments/${encodeURIComponent(remoteObjectId)}`, {
        method: "DELETE", body: { compaction_generation: proof.generation, release_before_cursor: proof.floor }, signal,
      });
      return 204;
    } catch (error) {
      if (error instanceof TransportError && error.kind === "status" && (error.status === 404 || error.status === 409)) return error.status;
      throw error;
    }
  }

  private async repairSnapshotIfRequested(signal: AbortSignal, media: ProtocolMediaHooks, work: Record<string, unknown>): Promise<boolean> {
    if (this.repairAttempted || work.pendingSnapshot === true || work.pendingApply === true) return false;
    const repair = await nonfatalValue(() => this.session.query("contact_repair_status", {}).then(object), undefined, signal);
    if (repair?.repairRequired !== true) return false;
    return await nonfatalValue(async () => {
      const snapshot = object(await this.transport.json("/v1/snapshot", { method: "GET", signal }));
      if (snapshot.compaction_supported !== true) return false;
      canonicalDecimal(snapshot.compaction_generation);
      if (!media.repairSnapshot) return false;
      this.repairAttempted = true;
      await media.repairSnapshot(signal);
      return true;
    }, false, signal);
  }

  private photoTransferState(signal: AbortSignal): Promise<Record<string, unknown>> {
    return nonfatalValue(() => this.session.query("contact_photo_transfer_state", {}).then(object), { uploads: [], registrations: [], reclaims: [] }, signal);
  }

  private syncWorkStatus(signal: AbortSignal): Promise<Record<string, unknown>> {
    return nonfatalValue(() => this.session.query("sync_work_status", {}).then(object), {}, signal);
  }
}

async function nonfatal(operation: () => Promise<unknown>, signal: AbortSignal): Promise<boolean> {
  try {
    await operation();
    return true;
  } catch (error) {
    throwIfFatal(error, signal);
    return false;
  }
}

async function nonfatalValue<T>(operation: () => Promise<T>, fallback: T, signal: AbortSignal): Promise<T> {
  try {
    return await operation();
  } catch (error) {
    throwIfFatal(error, signal);
    return fallback;
  }
}

function throwIfFatal(error: unknown, signal: AbortSignal): void {
  if (signal.aborted) throw new TransportError("cancelled");
  if (error instanceof TransportError && (error.kind === "cancelled" || error.kind === "revoked")) throw error;
}

function object(value: unknown): Record<string, unknown> { if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid"); return value as Record<string, unknown>; }
function array(value: unknown): unknown[] { if (!Array.isArray(value)) return []; return value; }
function requiredString(value: unknown): string { if (typeof value !== "string" || value.length === 0) throw new TransportError("invalid"); return value; }
function optionalDecimal(value: unknown): string | undefined { return typeof value === "string" && /^(0|[1-9][0-9]*)$/.test(value) ? value : undefined; }
function canonicalEpoch(value: unknown): string {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1 || value > 0xffff_ffff) throw new TransportError("invalid");
  return String(value);
}
function decodeVaultHeader(value: string): unknown {
  try {
    const binary = atob(value);
    const bytes = Uint8Array.from(binary, character => character.charCodeAt(0));
    return object(JSON.parse(new TextDecoder().decode(bytes)));
  } catch {
    throw new TransportError("invalid");
  }
}
function canonicalDecimal(value: unknown): string { const decimal = optionalDecimal(value); if (!decimal) throw new TransportError("invalid"); return decimal; }
function stringArray(value: unknown): string[] { if (!Array.isArray(value)) throw new TransportError("invalid"); return value.map(canonicalDecimal); }
function stringIds(value: unknown, key: string): string[] { return array(value).map(item => requiredString(object(item)[key])); }
function nonNegative(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0; }
function throwIfAborted(signal: AbortSignal): void { if (signal.aborted) throw new TransportError("cancelled"); }
