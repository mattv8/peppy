import { throwIfFatalTransferError, transferContactPhotoUploads, transferMedia, type MediaTransferResult, type WorkerMediaBridge } from "./scheduler-media.js";
import { SchedulerProtocol, type VaultRefresh } from "./scheduler-protocol.js";
import { TransportError } from "./transport.js";

const PAGE_LIMIT = 100;
const OUTBOX_LIMIT = 32;
const MAX_OUTBOX_BATCHES = 8;
const MAX_REPLAY_PAGES = 20;
const APPLY_LIMIT = 8;
const MAX_SNAPSHOT_ATTEMPTS = 3;
const PAGE_BYTES = 8 * 1024 * 1024;

export interface SchedulerSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
  hydrate(id: string): Promise<void>;
}
export interface SchedulerTransport {
  json(path: string, request: { method: "GET" | "POST" | "DELETE"; body?: unknown; signal?: AbortSignal; limit?: number }): Promise<unknown>;
  upload(path: string, bytes: Uint8Array, request?: { signal?: AbortSignal }): Promise<void>;
  download(path: string, request: { signal?: AbortSignal; limit?: number }): Promise<Uint8Array>;
  discard?(path: string, request: { method: "POST" | "DELETE"; body?: unknown; signal?: AbortSignal }): Promise<void>;
}

export type SchedulerConnection = "connected" | "offline" | "revoked" | "key-mismatch" | "error";
export interface SchedulerStatus { connection: SchedulerConnection; }
export interface BrowserSchedulerOptions {
  media?: WorkerMediaBridge;
  hasPendingProjectionWork?: () => Promise<boolean>;
  rotateEpoch?: (profile: unknown, header: unknown, passphrase: string) => Promise<void>;
  onEpochUnlocked?: () => void;
}

/** Bounded, single-flight Worker sync round. BrowserSession checkpoints every mutation before it resolves. */
export class BrowserScheduler {
  private running?: Promise<SchedulerStatus>;
  private controller?: AbortController;
  private statusValue: SchedulerStatus = { connection: "offline" };
  private readonly deferredMediaIds = new Set<string>();
  private protocol?: SchedulerProtocol;
  private epochValue?: VaultRefresh;

  public constructor(
    private readonly session: SchedulerSession,
    private readonly transport: SchedulerTransport,
    private readonly options: BrowserSchedulerOptions = {},
  ) {}

  public get status(): SchedulerStatus { return this.statusValue; }
  public get epochStatus(): VaultRefresh | undefined { return this.epochValue; }

  public async unlockNewEpoch(passphrase: string): Promise<void> {
    if (!this.protocol || !this.options.rotateEpoch) throw new TransportError("invalid");
    this.epochValue = await this.protocol.unlockNewEpoch(passphrase, this.options.rotateEpoch, this.controller?.signal);
    this.options.onEpochUnlocked?.();
  }

  public poll(): Promise<SchedulerStatus> {
    if (this.running) return this.running;
    this.controller = new AbortController();
    this.running = this.round(this.controller.signal)
      .then(() => this.setStatus("connected"))
      .catch(error => this.setStatus(statusFor(error)))
      .finally(() => { this.running = undefined; this.controller = undefined; });
    return this.running;
  }

  public stop(): void { this.controller?.abort(); }

  /** Makes a previously deferred attachment eligible for the next bounded poll. */
  public retryAttachment(id: string): void {
    this.deferredMediaIds.delete(requiredString(id));
  }

  private async round(signal: AbortSignal): Promise<void> {
    try {
      throwIfAborted(signal);
      if (this.transport.discard) {
        const protocol = await this.protocolCoordinator();
        const outcome = await protocol.run(signal, {
          beforeSeal: async ids => this.recordMediaTransfer(await transferContactPhotoUploads(this.session, this.transport, this.options.media, ids, signal, this.deferredMediaIds)),
          afterReferences: async () => this.recordMediaTransfer(await transferMedia(this.session, this.transport, this.options.media, signal, this.deferredMediaIds)),
          repairSnapshot: async () => this.snapshot(signal),
        });
        this.epochValue = outcome.vault;
      } else {
        this.recordMediaTransfer(await transferMedia(this.session, this.transport, this.options.media, signal, this.deferredMediaIds));
      }
      await this.sendOutbox(signal);
      await this.replay(signal);
      if (await this.hasPendingProjectionWork()) await this.apply(signal);
      await this.refreshServerContext(signal);
    } catch (error) {
      await this.refreshOfflineContext(error, signal);
      throw error;
    }
  }

  private async protocolCoordinator(): Promise<SchedulerProtocol> {
    if (this.protocol) return this.protocol;
    const identity = object(await this.session.query("_worker_identity_metadata", {}));
    if (!this.transport.discard) throw new TransportError("invalid");
    this.protocol = new SchedulerProtocol(this.session, this.transport as SchedulerTransport & { discard: NonNullable<SchedulerTransport["discard"]> }, {
      vaultId: requiredString(identity.vaultId),
      deviceId: requiredString(identity.deviceId),
    });
    return this.protocol;
  }

  private async hasPendingProjectionWork(): Promise<boolean> {
    try {
      const work = object(await this.session.query("sync_work_status", {}));
      return work.pendingApply === true || await this.options.hasPendingProjectionWork?.() === true;
    } catch {
      return await this.options.hasPendingProjectionWork?.() === true;
    }
  }

  private async sendOutbox(signal: AbortSignal): Promise<void> {
    for (let batch = 0; batch < MAX_OUTBOX_BATCHES; batch += 1) {
      throwIfAborted(signal);
      const pending = array(object(await this.session.query("pending_outbox", { limit: OUTBOX_LIMIT })).envelopes);
      if (pending.length === 0) return;
      for (const envelope of pending) {
        throwIfAborted(signal);
        try {
          const record = object(envelope);
          const path = eventPath(requiredString(record.purpose));
          await this.transport.json(path, { method: "POST", body: record, signal });
          throwIfAborted(signal);
          await this.session.mutate("ack_outbox", { envelopeId: requiredString(record.envelope_id) });
        } catch (error) {
          throwIfFatalTransferError(error, signal);
          return;
        }
      }
    }
  }

  private async replay(signal: AbortSignal): Promise<void> {
    for (let pageNumber = 0; pageNumber < MAX_REPLAY_PAGES; pageNumber += 1) {
      throwIfAborted(signal);
      const cursor = requiredString(object(await this.session.query("receive_cursor", {})).cursor);
      let page: Record<string, unknown>;
      try {
        page = object(await this.transport.json(`/v1/events?after=${encodeURIComponent(cursor)}&limit=${PAGE_LIMIT}`, { method: "GET", signal, limit: PAGE_BYTES }));
      } catch (error) {
        if (isResync(error)) {
          await this.snapshot(signal);
          continue;
        }
        throw error;
      }
      const events = array(page.events);
      if (events.length === 0) return;
      for (const event of events) {
        throwIfAborted(signal);
        const record = object(event);
        await this.session.mutate("ingest", { cursor: requiredString(record.cursor), envelope: record.envelope });
      }
      await this.apply(signal);
      if (events.length < PAGE_LIMIT || page.next_after === null) return;
    }
  }

  private async snapshot(signal: AbortSignal): Promise<void> {
    for (let attempt = 0; attempt < MAX_SNAPSHOT_ATTEMPTS; attempt += 1) {
      try {
        const progress = await this.startOrResumeSnapshot(signal, attempt > 0);
        await this.importSnapshotCut(progress, signal);
        await this.apply(signal);
        return;
      } catch (error) {
        if (!isResync(error) || attempt + 1 === MAX_SNAPSHOT_ATTEMPTS) throw error;
      }
    }
  }

  private async startOrResumeSnapshot(signal: AbortSignal, fresh: boolean): Promise<Record<string, unknown>> {
    if (!fresh) {
      const existing = object(await this.session.query("snapshot_progress", {})).progress;
      if (existing !== null && existing !== undefined) {
        const progress = object(existing);
        if (decimal(progress.receivedRecords) < decimal(progress.expectedRecords)) return progress;
      }
    }
    const start = object(await this.transport.json("/v1/snapshot", { method: "GET", signal }));
    return object(await this.session.mutate("begin_snapshot", {
      highWater: requiredString(start.high_water_cursor),
      recordCount: requiredString(start.record_count),
      purpose: "bootstrap",
      serverCompactionGeneration: start.compaction_supported === true ? requiredString(start.compaction_generation) : undefined,
    }));
  }

  private async importSnapshotCut(progress: Record<string, unknown>, signal: AbortSignal): Promise<void> {
    let current = progress;
    while (decimal(current.receivedRecords) < decimal(current.expectedRecords)) {
      throwIfAborted(signal);
      const highWater = requiredString(current.highWater);
      const after = requiredString(current.lastCursor);
      const generation = requiredString(current.generation);
      const fence = optionalString(current.serverCompactionGeneration);
      const query = new URLSearchParams({ high_water: highWater, after, limit: String(PAGE_LIMIT) });
      if (fence) query.set("compaction_generation", fence);
      const page = object(await this.transport.json(`/v1/snapshot/records?${query}`, { method: "GET", signal, limit: PAGE_BYTES }));
      const records = array(page.records);
      if (records.length === 0) throw new TransportError("status", 409, "resync_required");
      current = object(await this.session.mutate("append_snapshot_page", { generation, records }));
    }
    await this.session.mutate("finish_snapshot", { generation: requiredString(current.generation) });
  }

  private async apply(signal: AbortSignal): Promise<void> {
    for (let step = 0; step < APPLY_LIMIT; step += 1) {
      throwIfAborted(signal);
      const report = object(await this.session.mutate("apply_pending", { limit: 1000 }));
      if (nonNegative(report.applied) + nonNegative(report.quarantined) + nonNegative(report.drained) === 0) return;
    }
  }

  private recordMediaTransfer(result: MediaTransferResult): void {
    for (const id of result.completedIds) this.deferredMediaIds.delete(id);
    for (const id of result.failedIds) this.deferredMediaIds.add(id);
  }

  private async refreshServerContext(signal: AbortSignal): Promise<void> {
    throwIfAborted(signal);
    const devices = await this.transport.json("/v1/devices", { method: "GET", signal });
    throwIfAborted(signal);
    const capabilities = await this.transport.json("/v1/capabilities", { method: "GET", signal });
    await this.session.query("_worker_apply_server_context", { devices, capabilities, connection: "connected" });
  }

  private async refreshOfflineContext(error: unknown, signal: AbortSignal): Promise<void> {
    if (signal.aborted) return;
    const connection = error instanceof TransportError && error.kind === "offline" ? "offline" : "error";
    try {
      await this.session.query("_worker_apply_server_context", { devices: null, capabilities: null, connection });
    } catch {
      // Context is advisory; retain the original sync error and prior Rust-validated gateways.
    }
  }

  private setStatus(connection: SchedulerConnection): SchedulerStatus {
    this.statusValue = { connection };
    return this.statusValue;
  }
}

function eventPath(purpose: string): "/v1/commands" | "/v1/events" {
  if (purpose === "Command" || purpose === "command") return "/v1/commands";
  if (purpose === "Event" || purpose === "event") return "/v1/events";
  throw new TransportError("invalid");
}

function statusFor(error: unknown): SchedulerConnection {
  if (error instanceof TransportError) return error.kind === "revoked" ? "revoked" : error.kind === "offline" || error.kind === "cancelled" ? "offline" : "error";
  return typeof error === "object" && error !== null && "code" in error && (error as { code?: unknown }).code === "key-mismatch" ? "key-mismatch" : "error";
}

function isResync(error: unknown): boolean {
  return error instanceof TransportError && error.kind === "status" && (error.status === 409 || error.status === 400) && error.code === "resync_required";
}

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid");
  return value as Record<string, unknown>;
}
function array(value: unknown): unknown[] { if (!Array.isArray(value)) throw new TransportError("invalid"); return value; }
function requiredString(value: unknown): string { if (typeof value !== "string" || value.length === 0) throw new TransportError("invalid"); return value; }
function optionalString(value: unknown): string | undefined { return typeof value === "string" && value.length > 0 ? value : undefined; }
function nonNegative(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0; }
function decimal(value: unknown): bigint { const text = requiredString(value); if (!/^(?:0|[1-9][0-9]*)$/.test(text)) throw new TransportError("invalid"); return BigInt(text); }
function throwIfAborted(signal: AbortSignal): void { if (signal.aborted) throw new TransportError("cancelled"); }
