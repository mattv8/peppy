import { type CheckpointInput, CheckpointError, IndexedDbCheckpointStore, SerializedRuntime } from "./checkpoint.js";
import { BrowserCoreError, CoreRejectedError, type CoreResult } from "./core.js";
import { captureFilesystemCheckpoint, hydrateCipher, restoreFilesystemCheckpoint, type EmscriptenFilesystem } from "./filesystem.js";
import type { ParsedCredentialFile } from "./credential-files.js";

const ROOT = "/peppy";
const MAX_PLAINTEXT_MEDIA_BYTES = 32 * 1024 * 1024;
const MAX_CIPHERTEXT_MEDIA_BYTES = 33 * 1024 * 1024;
const MAX_PREVIEW_URL_BYTES = 2 * 1024 * 1024;
const CIPHER_FILE = /^client\.db\.media\/cipher\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.ppss$/;

export type BrowserSessionPhase = "unenrolled" | "locked" | "ready" | "closed";

export class BrowserSessionError extends Error {
  public constructor(public readonly code: "busy" | "cancelled" | "not-ready" | "closed") {
    super(code);
  }
}

export interface BrowserCoreFacade {
  invoke(request: { command: string; args: Record<string, unknown> }): Promise<CoreResult>;
}

export interface BrowserSessionOptions {
  origin: string;
  core: BrowserCoreFacade;
  filesystem: EmscriptenFilesystem;
  checkpoint: IndexedDbCheckpointStore;
  onFatal: (error: Error) => void;
}

export class BrowserSession {
  private phaseValue: BrowserSessionPhase = "closed";
  private generationValue = 0;
  private files: readonly string[] = [];
  private wrappedIdentity?: string;
  private transportToken?: string;
  private lifecycleBusy = false;
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

  public async enroll(metadata: unknown, deviceToken: string, passphrase: string, signal?: AbortSignal): Promise<void> {
    return this.lifecycle("unenrolled", signal, async () => {
      await this.options.core.invoke({ command: "_worker_enroll_identity", args: { metadata, deviceToken, passphrase } });
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
    });
  }

  /** Worker-private parse; the renderer never receives its token-bearing result. */
  public async parseCredentialFile(bytes: Uint8Array): Promise<ParsedCredentialFile> {
    this.requirePhase("unenrolled");
    const value = await this.options.core.invoke({ command: "_worker_parse_credential_file", args: { bytes: [...bytes] } });
    if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
    const parsed = value as Record<string, unknown>;
    if (parsed.format === "legacy" && typeof parsed.deviceToken === "string" && "metadata" in parsed) {
      return { format: "legacy", metadata: parsed.metadata, deviceToken: parsed.deviceToken };
    }
    if (parsed.format === "portable" && typeof parsed.deviceToken === "string") {
      return { format: "portable", deviceToken: parsed.deviceToken };
    }
    throw new BrowserCoreError("core-error");
  }

  /** Rust verifies the fetched portable vault before returning token-free enrollment metadata. */
  public async portableIdentityMetadata(bytes: Uint8Array, vault: Record<string, unknown>): Promise<unknown> {
    this.requirePhase("unenrolled");
    return this.options.core.invoke({ command: "_worker_portable_identity_metadata", args: { bytes: [...bytes], vault } });
  }

  /** Produces a Worker-only credential payload for a transient browser download capability. */
  public async exportCredential(): Promise<{ filename: string; bytes: Uint8Array }> {
    this.requireReady();
    return this.runtime.read(async () => {
      this.requireReady();
      const value = await this.options.core.invoke({ command: "_worker_export_credential", args: {} });
      this.requireReady();
      if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
      const output = value as Record<string, unknown>;
      const bytes = output.bytes;
      if (output.filename !== "peppy-credentials.json") throw new BrowserCoreError("core-error");
      if (!Array.isArray(bytes) || bytes.length === 0 || bytes.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new BrowserCoreError("core-error");
      return { filename: output.filename, bytes: Uint8Array.from(bytes) };
    });
  }

  public async unlock(passphrase: string, signal?: AbortSignal): Promise<void> {
    return this.lifecycle("locked", signal, async () => {
      const wrappedIdentity = this.wrappedIdentity;
      if (!wrappedIdentity) throw new CheckpointError("invalid-checkpoint");
      await this.options.core.invoke({ command: "_worker_unlock_identity", args: { wrappedIdentity, passphrase } });
      await this.commitCheckpoint();
      this.throwIfAborted(signal);
      await this.loadTransportToken();
      this.throwIfAborted(signal);
      this.phaseValue = "ready";
    });
  }

  public async rotate(profile: unknown, header: unknown, passphrase: string): Promise<void> {
    this.requireReady();
    let checkpoint: CheckpointInput | undefined;
    try {
      const result = await this.runtime.mutate({
        invoke: () => this.options.core.invoke({ command: "_worker_rotate_identity", args: { profile, header, passphrase } }),
        capture: async generation => checkpoint = await this.capture(generation),
      });
      this.applyCheckpoint(checkpoint, result.generation);
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
    return this.runtime.read(async () => {
      try {
        await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
        const value = await this.options.core.invoke({ command: "preview_attachment", args: { id } });
        const previewUrl = typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>).previewUrl : undefined;
        if (typeof previewUrl !== "string" || previewUrl.length > MAX_PREVIEW_URL_BYTES || !/^data:image\/png;base64,[A-Za-z0-9+/]*={0,2}$/.test(previewUrl)) { this.previewedIds.add(id); return undefined; }
        if (this.previews.size >= 32) this.previews.delete(this.previews.keys().next().value!);
        this.previews.set(id, previewUrl);
        return previewUrl;
      } catch {
        this.previewedIds.add(id);
        return undefined;
      }
    });
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

  /** Hydrates ciphertext and deletes the Worker-only plaintext export in the same serialized read. */
  public async exportAttachment(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array }> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id)) throw new BrowserSessionError("cancelled");
    const outputId = crypto.randomUUID();
    const path = `${ROOT}/worker-output/${outputId}`;
    return this.runtime.read(async () => {
      this.requireReady();
      try {
        await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
        const value = await this.options.core.invoke({ command: "export_attachment", args: { id, outputId } });
        if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
        const output = value as Record<string, unknown>;
        const length = output.length;
        if (output.file !== `worker-output/${outputId}` || typeof length !== "number" || !Number.isSafeInteger(length) || length < 0 || length > MAX_PLAINTEXT_MEDIA_BYTES || typeof output.displayName !== "string" || typeof output.type !== "string") throw new BrowserCoreError("core-error");
        const bytes = new Uint8Array(this.options.filesystem.readFile(path));
        if (bytes.byteLength !== length) throw new BrowserCoreError("core-error");
        return { bytes, displayName: output.displayName, type: output.type };
      } finally {
        try { this.options.filesystem.unlink(path); } catch {}
      }
    });
  }

  /** Produces and consumes a private metadata-free derivative without exposing a Worker path. */
  public async publicImage(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array; name: string }> {
    this.requireReady();
    if (!/^[0-9a-f-]{36}$/.test(id)) throw new BrowserSessionError("cancelled");
    const outputId = crypto.randomUUID();
    const path = `${ROOT}/worker-output/${outputId}`;
    return this.runtime.read(async () => {
      this.requireReady();
      try {
        await hydrateCipher(id, this.generationValue, this.options.checkpoint, this.options.filesystem);
        const value = await this.options.core.invoke({ command: "public_image", args: { id, outputId } });
        if (typeof value !== "object" || value === null || Array.isArray(value)) throw new BrowserCoreError("core-error");
        const output = value as Record<string, unknown>;
        const byteSize = output.byteSize;
        const name = output.name;
        if (output.file !== `worker-output/${outputId}` || typeof byteSize !== "number" || !Number.isSafeInteger(byteSize) || byteSize <= 0 || byteSize > MAX_PLAINTEXT_MEDIA_BYTES || typeof name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(name)) throw new BrowserCoreError("core-error");
        const bytes = new Uint8Array(this.options.filesystem.readFile(path));
        if (bytes.byteLength !== byteSize) throw new BrowserCoreError("core-error");
        return { ...output, name, bytes };
      } finally {
        try { this.options.filesystem.unlink(path); } catch {}
      }
    });
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
