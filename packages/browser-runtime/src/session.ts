import { type CheckpointInput, CheckpointError, IndexedDbCheckpointStore, SerializedRuntime } from "./checkpoint.js";
import { BrowserCoreError, CoreRejectedError, type CoreResult } from "./core.js";
import { captureFilesystemCheckpoint, hydrateCipher, restoreFilesystemCheckpoint, type EmscriptenFilesystem } from "./filesystem.js";

const ROOT = "/peppy";
const MAX_PLAINTEXT_MEDIA_BYTES = 32 * 1024 * 1024;
const MAX_CIPHERTEXT_MEDIA_BYTES = 33 * 1024 * 1024;
const MAX_PREVIEW_URL_BYTES = 2 * 1024 * 1024;
const CIPHER_FILE = /^client\.db\.media\/cipher\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.ppss$/;
const REMEMBERED_UNLOCK_BOOT_TIMEOUT_MS = 3_000;

export type BrowserSessionPhase = "unenrolled" | "locked" | "ready" | "closed";
export type BrowserPublicIdentity = { role: "owner" | "device" | "gateway" };

export class BrowserSessionError extends Error {
  public constructor(public readonly code: "busy" | "cancelled" | "not-ready" | "closed") {
    super(code);
  }
}

export interface BrowserCoreFacade {
  invoke(request: { command: string; args: Record<string, unknown> }): Promise<CoreResult>;
}

export interface RememberedUnlockLifecycle {
  remember(wrappedIdentity: string, passphrase: string): Promise<void>;
  recover(wrappedIdentity: string): Promise<string | undefined>;
  clear(): Promise<void>;
  validate(): Promise<void>;
}

export interface BrowserSessionOptions {
  origin: string;
  core: BrowserCoreFacade;
  filesystem: EmscriptenFilesystem;
  checkpoint: IndexedDbCheckpointStore;
  onFatal: (error: Error) => void;
  rememberedUnlock?: RememberedUnlockLifecycle;
}

function isReadyAttachment(value: CoreResult): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) && (value as Record<string, unknown>).state === "ready";
}

function attachmentBytes(value: Record<string, unknown>): number {
  const size = value.plaintextBytes ?? value.byteSize;
  if (typeof size !== "number" || !Number.isSafeInteger(size) || size < 0 || size > MAX_PLAINTEXT_MEDIA_BYTES) throw new BrowserCoreError("core-error");
  return size;
}

function exportedAttachment(value: CoreResult, outputId: string, path: string, filesystem: EmscriptenFilesystem, maxBytes: number): Record<string, unknown> & { bytes: Uint8Array } {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
  const output = value as Record<string, unknown>;
  const length = output.length;
  if (output.file !== `worker-output/${outputId}` || typeof length !== "number" || !Number.isSafeInteger(length) || length < 0 || length > maxBytes || typeof output.displayName !== "string" || typeof output.type !== "string") throw new BrowserCoreError("core-error");
  const bytes = new Uint8Array(filesystem.readFile(path));
  if (bytes.byteLength !== length) throw new BrowserCoreError("core-error");
  return { bytes, displayName: output.displayName, type: output.type };
}

export class BrowserSession {
  private phaseValue: BrowserSessionPhase = "closed";
  private generationValue = 0;
  private files: readonly string[] = [];
  private wrappedIdentity?: string;
  private transportToken?: string;
  private publicIdentityValue?: BrowserPublicIdentity;
  private lifecycleBusy = false;
  private rememberedUnlockGeneration = 0;
  private fatalReported = false;
  private readonly previews = new Map<string, string>();
  private readonly previewedIds = new Set<string>();
  private readonly lifetime = new AbortController();
  private runtime!: SerializedRuntime;

  private constructor(private readonly options: BrowserSessionOptions) {}

  public static async boot(options: BrowserSessionOptions): Promise<BrowserSession> {
    const session = new BrowserSession(options);
    try {
      await options.core.invoke({ command: "_worker_initialize", args: { origin: options.origin } });
      session.assertFreshFilesystem();
      const loaded = await options.checkpoint.load();
      if (loaded.kind === "absent") {
        session.createRuntime();
        session.phaseValue = "unenrolled";
        return session;
      }
      restoreFilesystemCheckpoint(loaded.checkpoint, options.filesystem);
      session.generationValue = loaded.checkpoint.manifest.generation;
      session.files = [...loaded.checkpoint.manifest.files];
      session.wrappedIdentity = decodeWrappedIdentity(loaded.checkpoint.manifest.wrappedCredentials);
      session.createRuntime();
      session.phaseValue = "locked";
      await session.recoverRememberedUnlock();
      return session;
    } catch (error: unknown) {
      const failure = error instanceof Error ? error : new CheckpointError("storage-unavailable");
      session.shutdown();
      throw failure;
    }
  }

  public get phase(): BrowserSessionPhase { return this.phaseValue; }
  public get generation(): number { return this.generationValue; }
  public get signal(): AbortSignal { return this.lifetime.signal; }
  public publicIdentity(): BrowserPublicIdentity | undefined { return this.publicIdentityValue ? { ...this.publicIdentityValue } : undefined; }

  public async enroll(metadata: unknown, deviceToken: string, passphrase: string, signal?: AbortSignal): Promise<void> {
    return this.lifecycle("unenrolled", signal, async () => {
      const identity = await this.options.core.invoke({ command: "_worker_enroll_identity", args: { metadata, deviceToken, passphrase } });
      this.publicIdentityValue = publicIdentity(identity);
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
    });
  }

  /** Completes a Rust-verified enrollment handoff without routing it through generic identity enrollment. */
  public async finishEnrollment(command: "_worker_enrollment_finish_owner" | "_worker_enrollment_finish_manual" | "_worker_enrollment_finish_hosted_owner", responseJson: string, passphrase: string, signal?: AbortSignal): Promise<void> {
    return this.lifecycle("unenrolled", signal, async () => {
      const identity = await this.options.core.invoke({ command, args: { responseJson, passphrase } });
      this.publicIdentityValue = publicIdentity(identity);
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
    });
  }

  public async finishComputerEnrollment(responseJson: string, signal?: AbortSignal): Promise<void> {
    return this.lifecycle("unenrolled", signal, async () => {
      const identity = await this.options.core.invoke({ command: "_worker_enrollment_finish_computer", args: { completeJson: responseJson } });
      this.publicIdentityValue = publicIdentity(identity);
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
    });
  }

  public async unlock(passphrase: string, signal?: AbortSignal, options?: { remember?: boolean }): Promise<void> {
    return this.unlockWithPassphrase(passphrase, signal, options?.remember === true, this.rememberedUnlockGeneration);
  }

  public async clearRememberedUnlock(): Promise<void> {
    this.rememberedUnlockGeneration += 1;
    await this.options.rememberedUnlock?.clear();
  }

  public async validateRememberedUnlock(): Promise<void> {
    await this.options.rememberedUnlock?.validate().catch(() => undefined);
  }

  private async unlockWithPassphrase(passphrase: string, signal: AbortSignal | undefined, remember: boolean, rememberedUnlockGeneration: number): Promise<void> {
    return this.lifecycle("locked", signal, async () => {
      const wrappedIdentity = this.wrappedIdentity;
      if (!wrappedIdentity) throw new CheckpointError("invalid-checkpoint");
      const identity = await this.options.core.invoke({ command: "_worker_unlock_identity", args: { wrappedIdentity, passphrase } });
      this.publicIdentityValue = publicIdentity(identity);
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
      if (remember) this.rememberUnlock(passphrase, rememberedUnlockGeneration);
    });
  }

  public async rotate(profile: unknown, header: unknown, passphrase: string): Promise<void> {
    this.requireReady();
    const rememberedUnlockGeneration = this.rememberedUnlockGeneration;
    const previousWrappedIdentity = this.wrappedIdentity;
    let checkpoint: CheckpointInput | undefined;
    try {
      const result = await this.runtime.mutate({
        invoke: () => this.options.core.invoke({ command: "_worker_rotate_identity", args: { profile, header, passphrase } }),
        capture: async generation => checkpoint = await this.capture(generation),
      });
      this.applyCheckpoint(checkpoint, result.generation);
      if (previousWrappedIdentity) this.rekeyRememberedUnlock(previousWrappedIdentity, passphrase, rememberedUnlockGeneration);
    } catch (error: unknown) {
      this.applyHandledCheckpoint(checkpoint, error);
      throw error;
    }
  }

  /** Trusted Worker-internal operation; the SharedWorker entry must never route UI messages here. */
  public async query(command: string, args: Record<string, unknown>): Promise<CoreResult> {
    this.requireReady();
    return this.runtime.read(async () => {
      this.requireReady();
      try {
        return await this.options.core.invoke({ command, args });
      } catch (error: unknown) {
        if (!(error instanceof CoreRejectedError)) this.fatal(asError(error));
        throw error;
      }
    });
  }

  /** Trusted Worker-internal operation; the SharedWorker entry must never route UI messages here. */
  public async mutate(command: string, args: Record<string, unknown>): Promise<CoreResult> {
    this.requireReady();
    let checkpoint: CheckpointInput | undefined;
    try {
      const result = await this.runtime.mutate({
        invoke: () => { this.requireReady(); return this.options.core.invoke({ command, args }); },
        capture: async generation => checkpoint = await this.capture(generation),
      });
      this.applyCheckpoint(checkpoint, result.generation);
      return result.value;
    } catch (error: unknown) {
      this.applyHandledCheckpoint(checkpoint, error);
      throw error;
    }
  }

  public async hydrate(id: string): Promise<void> {
    this.requireReady();
    return this.runtime.read(async () => {
      this.requireReady();
      try {
        await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
      } catch (error: unknown) {
        this.fatal(asError(error));
        throw error;
      }
    });
  }

  /** Worker-private media bridge; renderer RPC never accepts filesystem paths. */
  public async readCiphertext(file: string, expectedBytes: number, signal: AbortSignal): Promise<Uint8Array> {
    this.requireReady();
    if (!CIPHER_FILE.test(file) || !Number.isSafeInteger(expectedBytes) || expectedBytes <= 0 || expectedBytes > MAX_CIPHERTEXT_MEDIA_BYTES || signal.aborted) throw new BrowserSessionError("cancelled");
    return this.runtime.read(async () => {
      this.requireReady();
      if (signal.aborted) throw new BrowserSessionError("cancelled");
      const bytes = new Uint8Array(this.options.filesystem.readFile(`${ROOT}/${file}`));
      if (bytes.byteLength !== expectedBytes) throw new CheckpointError("invalid-checkpoint");
      return bytes;
    });
  }

  public async stageDownload(bytes: Uint8Array, signal: AbortSignal): Promise<string> {
    this.requireReady();
    if (bytes.byteLength === 0 || bytes.byteLength > MAX_CIPHERTEXT_MEDIA_BYTES || signal.aborted) throw new BrowserSessionError("cancelled");
    return this.runtime.read(async () => {
      this.requireReady();
      if (signal.aborted) throw new BrowserSessionError("cancelled");
      const directory = `${ROOT}/worker-input`;
      if (!this.options.filesystem.readdir(ROOT).includes("worker-input")) this.options.filesystem.mkdir(directory);
      const name = `${crypto.randomUUID()}.ppss`;
      this.options.filesystem.writeFile(`${directory}/${name}`, new Uint8Array(bytes));
      return name;
    });
  }

  public async stageInput(bytes: Uint8Array): Promise<string> {
    return this.stageDownload(bytes, new AbortController().signal);
  }

  /** Removes a Worker-only downloaded ciphertext staging file; callers cannot supply a path. */
  public async cleanupStagedDownload(source: string): Promise<void> {
    if (!/^[0-9a-f-]{36}\.ppss$/.test(source)) return;
    await this.runtime.read(async () => {
      try { this.options.filesystem.unlink(`${ROOT}/worker-input/${source}`); } catch {}
    });
  }

  /** Stages browser plaintext, invokes the core, checkpoints, and removes the staging file as one serialized operation. */
  public async prepareAttachment(bytes: Uint8Array, displayName: string, mediaType: string): Promise<CoreResult> {
    this.requireReady();
    if (bytes.byteLength === 0 || bytes.byteLength > MAX_PLAINTEXT_MEDIA_BYTES) throw new BrowserSessionError("cancelled");
    let checkpoint: CheckpointInput | undefined;
    const name = `${crypto.randomUUID()}.ppss`;
    const path = `${ROOT}/worker-input/${name}`;
    try {
      const result = await this.runtime.mutate({
        invoke: async () => {
          this.requireReady();
          const directory = `${ROOT}/worker-input`;
          if (!this.options.filesystem.readdir(ROOT).includes("worker-input")) this.options.filesystem.mkdir(directory);
          this.options.filesystem.writeFile(path, new Uint8Array(bytes));
          return this.options.core.invoke({ command: "prepare_attachment", args: { temporaryInputFilename: name, displayName, mediaType } });
        },
        capture: async generation => checkpoint = await this.capture(generation),
      });
      this.applyCheckpoint(checkpoint, result.generation);
      return await this.withPreview(result.value);
    } catch (error: unknown) {
      this.applyHandledCheckpoint(checkpoint, error);
      throw error;
    } finally {
      try { this.options.filesystem.unlink(path); } catch {}
    }
  }

  /** Returns a sanitized PNG data URL for one attachment without exposing its plaintext path. */
  public async previewAttachment(id: string): Promise<string | undefined> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id)) return undefined;
    const cached = this.previews.get(id);
    if (cached) return cached;
    if (this.previewedIds.has(id)) return undefined;
    let previewUrl: unknown;
    try {
      previewUrl = await this.runtime.read(async () => {
        this.requireReady();
        const info = await this.options.core.invoke({ command: "attachment_info", args: { id } });
        if (!isReadyAttachment(info)) throw new CoreRejectedError("media-pending");
        attachmentBytes(info);
        await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
        const value = await this.options.core.invoke({ command: "preview_attachment", args: { id } });
        return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>).previewUrl : undefined;
      });
    } catch {
      return undefined;
    }
    if (typeof previewUrl !== "string" || previewUrl.length > MAX_PREVIEW_URL_BYTES || !/^data:image\/png;base64,[A-Za-z0-9+/]*={0,2}$/.test(previewUrl)) {
      this.previewedIds.add(id);
      return undefined;
    }
    if (this.previews.size >= 32) this.previews.delete(this.previews.keys().next().value!);
    this.previews.set(id, previewUrl);
    return previewUrl;
  }

  public async readWorkerOutput(id: string, expectedBytes: number): Promise<Uint8Array> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id) || !Number.isSafeInteger(expectedBytes) || expectedBytes < 0 || expectedBytes > MAX_PLAINTEXT_MEDIA_BYTES) throw new BrowserSessionError("cancelled");
    return this.runtime.read(async () => {
      this.requireReady();
      const path = `${ROOT}/worker-output/${id}`;
      const bytes = new Uint8Array(this.options.filesystem.readFile(path));
      if (bytes.byteLength !== expectedBytes) throw new CheckpointError("invalid-checkpoint");
      return bytes;
    });
  }

  /** Exports media and deletes the Worker-only plaintext output after its serialized mutation settles. */
  public async exportAttachment(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array }> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id)) throw new BrowserSessionError("cancelled");
    const outputId = crypto.randomUUID();
    const path = `${ROOT}/worker-output/${outputId}`;
    try {
      return await this.accessMedia(id, MAX_PLAINTEXT_MEDIA_BYTES, false, async () => {
        this.requireReady();
        const value = await this.options.core.invoke({ command: "export_attachment", args: { id, outputId } });
        if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
        const output = value as Record<string, unknown>;
        const length = output.length;
        if (output.file !== `worker-output/${outputId}` || typeof length !== "number" || !Number.isSafeInteger(length) || length < 0 || length > MAX_PLAINTEXT_MEDIA_BYTES || typeof output.displayName !== "string" || typeof output.type !== "string") throw new BrowserCoreError("core-error");
        const bytes = new Uint8Array(this.options.filesystem.readFile(path));
        if (bytes.byteLength !== length) throw new BrowserCoreError("core-error");
        return { bytes, displayName: output.displayName, type: output.type };
      });
    } finally { try { this.options.filesystem.unlink(path); } catch {} }
  }

  /** Checks availability, hydrates, and exports under one checkpointed mutation. */
  public async openMediaAttachment(id: string, maxBytes: number): Promise<Record<string, unknown> & { bytes: Uint8Array }> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id) || !Number.isSafeInteger(maxBytes) || maxBytes <= 0 || maxBytes > MAX_PLAINTEXT_MEDIA_BYTES) throw new BrowserSessionError("cancelled");
    const outputId = crypto.randomUUID();
    const path = `${ROOT}/worker-output/${outputId}`;
    try {
      return await this.accessMedia(id, maxBytes, true, async () => {
        const value = await this.options.core.invoke({ command: "export_attachment", args: { id, outputId } });
        return exportedAttachment(value, outputId, path, this.options.filesystem, maxBytes);
      });
    } finally { try { this.options.filesystem.unlink(path); } catch {} }
  }

  /** Produces and consumes a private metadata-free derivative without exposing a Worker path. */
  public async publicImage(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array; name: string }> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id)) throw new BrowserSessionError("cancelled");
    const outputId = crypto.randomUUID();
    const path = `${ROOT}/worker-output/${outputId}`;
    try {
      return await this.accessMedia(id, MAX_PLAINTEXT_MEDIA_BYTES, false, async () => {
        this.requireReady();
        const value = await this.options.core.invoke({ command: "public_image", args: { id, outputId } });
        if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
        const output = value as Record<string, unknown>;
        const byteSize = output.byteSize;
        const name = output.name;
        if (output.file !== `worker-output/${outputId}` || typeof byteSize !== "number" || !Number.isSafeInteger(byteSize) || byteSize <= 0 || byteSize > MAX_PLAINTEXT_MEDIA_BYTES || typeof name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(name)) throw new BrowserCoreError("core-error");
        const bytes = new Uint8Array(this.options.filesystem.readFile(path));
        if (bytes.byteLength !== byteSize) throw new BrowserCoreError("core-error");
        return { ...output, name, bytes };
      });
    } finally { try { this.options.filesystem.unlink(path); } catch {} }
  }

  public tokenForTransport(): string {
    this.requireReady();
    if (!this.transportToken) throw new BrowserSessionError("closed");
    return this.transportToken;
  }

  public shutdown(): void {
    if (this.phaseValue === "closed") return;
    this.phaseValue = "closed";
    this.transportToken = undefined;
    this.publicIdentityValue = undefined;
    this.previews.clear();
    this.previewedIds.clear();
    this.lifetime.abort();
  }

  private async withPreview(value: CoreResult): Promise<CoreResult> {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return value;
    const attachment = value as Record<string, unknown>;
    if (typeof attachment.id !== "string") return value;
    const previewUrl = await this.previewAttachment(attachment.id);
    return previewUrl ? { ...attachment, previewUrl } : attachment;
  }

  private async recoverRememberedUnlock(): Promise<void> {
    const wrappedIdentity = this.wrappedIdentity;
    if (!wrappedIdentity || !this.options.rememberedUnlock) return;
    const rememberedUnlockGeneration = this.rememberedUnlockGeneration;
    const passphrase = await this.recoverRememberedPassphraseWithinBootDeadline(wrappedIdentity);
    if (!passphrase || rememberedUnlockGeneration !== this.rememberedUnlockGeneration || this.phaseValue !== "locked") return;
    try {
      await this.unlockWithPassphrase(passphrase, undefined, false, rememberedUnlockGeneration);
    } catch (error: unknown) {
      if (error instanceof CoreRejectedError) {
        await this.discardRememberedUnlock();
        return;
      }
      throw error;
    }
  }

  private async recoverRememberedPassphraseWithinBootDeadline(wrappedIdentity: string): Promise<string | undefined> {
    const rememberedUnlock = this.options.rememberedUnlock;
    if (!rememberedUnlock) return undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const timedOut = Symbol("remembered-unlock-timeout");
    try {
      const passphrase = await Promise.race([
        rememberedUnlock.recover(wrappedIdentity).catch(() => undefined),
        new Promise<typeof timedOut>(resolve => {
          timeout = setTimeout(() => {
            resolve(timedOut);
          }, REMEMBERED_UNLOCK_BOOT_TIMEOUT_MS);
        }),
      ]);
      if (passphrase === timedOut) {
        this.rememberedUnlockGeneration += 1;
        return undefined;
      }
      return passphrase;
    } finally {
      if (timeout !== undefined) clearTimeout(timeout);
    }
  }

  private rememberUnlock(passphrase: string, generation: number): void {
    const wrappedIdentity = this.wrappedIdentity;
    if (!wrappedIdentity || generation !== this.rememberedUnlockGeneration) return;
    void this.options.rememberedUnlock?.remember(wrappedIdentity, passphrase).catch(() => undefined);
  }

  private rekeyRememberedUnlock(previousWrappedIdentity: string, passphrase: string, generation: number): void {
    const rememberedUnlock = this.options.rememberedUnlock;
    if (!rememberedUnlock) return;
    void rememberedUnlock.recover(previousWrappedIdentity)
      .then(recoveredPassphrase => {
        if (recoveredPassphrase) this.rememberUnlock(passphrase, generation);
      })
      .catch(() => undefined);
  }

  private async discardRememberedUnlock(): Promise<void> {
    this.rememberedUnlockGeneration += 1;
    await this.options.rememberedUnlock?.clear().catch(() => undefined);
  }

  /** Ensures work accepted before shutdown finishes before the owner lock is released. */
  public async drain(): Promise<void> { await this.runtime.drain(); }

  private async lifecycle(expected: BrowserSessionPhase, signal: AbortSignal | undefined, operation: () => Promise<void>): Promise<void> {
    if (this.lifecycleBusy) throw new BrowserSessionError("busy");
    this.requirePhase(expected);
    this.throwIfAborted(signal);
    this.lifecycleBusy = true;
    try {
      await operation();
    } catch (error: unknown) {
      if (error instanceof BrowserSessionError && error.code === "cancelled") {
        this.shutdown();
        throw error;
      }
      if (error instanceof CoreRejectedError) throw error;
      this.fatal(asError(error));
      if (error instanceof BrowserCoreError) throw new CoreRejectedError("core");
      throw error;
    } finally {
      this.lifecycleBusy = false;
    }
  }

  private async commitCheckpoint(): Promise<void> {
    const checkpoint = await this.capture(this.generationValue);
    const generation = await this.options.checkpoint.commit(checkpoint);
    this.applyCheckpoint(checkpoint, generation);
    this.createRuntime();
  }

  private async accessMedia<T>(id: string, maxBytes: number, retainLease: boolean, operation: () => Promise<T>): Promise<T> {
    const preflight = await this.query("attachment_info", { id });
    if (!isReadyAttachment(preflight)) throw new CoreRejectedError("media-pending");
    if (attachmentBytes(preflight) > maxBytes) throw new CoreRejectedError("unsupported");
    let checkpoint: CheckpointInput | undefined;
    try {
      const result = await this.runtime.mutate({
        invoke: async () => {
          this.requireReady();
          const info = await this.options.core.invoke({ command: "attachment_info", args: { id } });
          if (!isReadyAttachment(info)) throw new CoreRejectedError("media-pending");
          if (attachmentBytes(info) > maxBytes) throw new CoreRejectedError("unsupported");
          await this.options.core.invoke({ command: "retain_attachment_ciphertext", args: { id } });
          try {
            await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
            const value = await operation();
            await this.options.core.invoke({ command: "touch_attachment_ciphertext", args: { id } });
            if (!retainLease) await this.options.core.invoke({ command: "release_attachment_ciphertext", args: { id } });
            return value;
          } catch (error: unknown) {
            try { await this.options.core.invoke({ command: "release_attachment_ciphertext", args: { id } }); } catch {}
            throw error;
          }
        },
        capture: async generation => checkpoint = await this.capture(generation),
      });
      this.applyCheckpoint(checkpoint, result.generation);
      return result.value;
    } catch (error: unknown) {
      this.applyHandledCheckpoint(checkpoint, error);
      throw error;
    }
  }

  private async capture(expectedGeneration: number): Promise<CheckpointInput> {
    return captureFilesystemCheckpoint({
      fs: this.options.filesystem,
      core: this.options.core,
      expectedGeneration,
      previousFiles: this.files,
    });
  }

  private applyHandledCheckpoint(checkpoint: CheckpointInput | undefined, error: unknown): void {
    if (checkpoint && error instanceof CoreRejectedError) this.applyCheckpoint(checkpoint, this.generationValue + 1);
  }

  private applyCheckpoint(checkpoint: CheckpointInput | undefined, generation: number): void {
    if (!checkpoint) throw new CheckpointError("storage-unavailable");
    this.generationValue = generation;
    this.files = [...checkpoint.files.map(file => file.path), ...(checkpoint.retainedCipherPaths ?? [])];
    this.wrappedIdentity = decodeWrappedIdentity(checkpoint.wrappedCredentials);
  }

  private async loadTransportToken(): Promise<void> {
    const result = await this.options.core.invoke({ command: "_worker_transport_token", args: {} });
    if (typeof result !== "object" || result === null || Array.isArray(result)) {
      throw new BrowserCoreError("core-error");
    }
    const deviceToken = (result as Record<string, unknown>).deviceToken;
    if (typeof deviceToken !== "string" || !deviceToken) throw new BrowserCoreError("core-error");
    this.transportToken = deviceToken;
  }

  private assertFreshFilesystem(): void {
    if (!this.options.filesystem.readdir("/").includes("peppy")) return;
    if (this.options.filesystem.readdir(ROOT).includes("client.db")) throw new CheckpointError("invalid-checkpoint");
  }

  private createRuntime(): void {
    this.runtime = new SerializedRuntime(this.options.checkpoint, () => this.fatal(new CheckpointError("storage-unavailable")), this.generationValue);
  }

  private requireReady(): void { this.requirePhase("ready"); }
  private requirePhase(expected: BrowserSessionPhase): void {
    if (this.phaseValue === "closed") throw new BrowserSessionError("closed");
    if (this.phaseValue !== expected) throw new BrowserSessionError("not-ready");
  }

  private throwIfAborted(signal: AbortSignal | undefined): void {
    if (signal?.aborted || this.lifetime.signal.aborted) throw new BrowserSessionError("cancelled");
  }

  private fatal(error: Error): void {
    this.shutdown();
    if (this.fatalReported) return;
    this.fatalReported = true;
    this.options.onFatal(error);
  }
}

function decodeWrappedIdentity(bytes: Uint8Array): string {
  if (bytes.byteLength === 0 || bytes.byteLength > 64 * 1024) throw new CheckpointError("invalid-checkpoint");
  try {
    const value = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (!value) throw new Error("empty");
    return value;
  } catch {
    throw new CheckpointError("invalid-checkpoint");
  }
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new BrowserCoreError("core-error");
}

function publicIdentity(value: CoreResult): BrowserPublicIdentity {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
  const role = (value as Record<string, unknown>).role;
  if (role !== "owner" && role !== "device" && role !== "gateway") throw new BrowserCoreError("core-error");
  return { role };
}
