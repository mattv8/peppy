const MAX_REQUEST_BYTES = 8 * 1024 * 1024;

export interface EmscriptenCoreModule {
  HEAPU8: Uint8Array;
  UTF8ToString(pointer: number): string;
  _peppy_browser_alloc(length: number): number;
  _peppy_browser_dispatch(pointer: number, length: number): number;
  _peppy_browser_free_request(pointer: number, length: number): void;
  _peppy_browser_free_response(pointer: number): void;
}

export type BrowserCoreFactory = () => Promise<EmscriptenCoreModule>;

export interface CoreRequest {
  command: string;
  args: Record<string, unknown>;
}

export type CoreResult = Record<string, unknown> | readonly unknown[] | string | number | boolean | null;

const HANDLED_CORE_CODES = [
  "already-open", "attachment-invalid", "attachment-local", "core", "credential-mismatch",
  "credentials-required", "database-key-mismatch", "gateway-offline", "gateway-unavailable",
  "gateway-unsupported", "gateways-unknown", "invalid-request", "locked", "not-found",
  "request-too-large", "stale-draft", "unknown-command", "unlock-failed",
  "invalid-contact-edit", "contact-book-read-only", "contact-photo-invalid",
  "identity-exists", "identity-unavailable", "epoch-mismatch", "invalid-rotation", "unavailable",
  "invalid-draft", "invalid-route", "invalid-recipient", "invalid-attachment", "empty-message",
  "message-too-long", "mms-unsupported", "mms-too-many-recipients", "mms-too-large", "mms-reply-blocked",
  "public-copy-unsupported", "public-copy-too-large",
] as const;

export type CoreErrorCode = typeof HANDLED_CORE_CODES[number];

export class BrowserCoreError extends Error {
  public constructor(
    public readonly code: string,
    public readonly details?: Readonly<{ currentRevision?: string }>,
  ) {
    super(code === "core-unavailable" ? "Core runtime unavailable" : "Core operation failed");
  }
}

/** A well-formed, handled Rust rejection; unlike a trap it may have committed a restore guard. */
export class CoreRejectedError extends BrowserCoreError {
  public constructor(code: string, details?: Readonly<{currentRevision?: string}>) {
    super(code, details);
  }
}

function isSafeResult(value: unknown): value is CoreResult {
  return value === null || typeof value === "string" || typeof value === "number" ||
    typeof value === "boolean" || Array.isArray(value) ||
    (typeof value === "object" && value !== null && !Array.isArray(value));
}

function responseResult(json: string): CoreResult {
  let envelope: unknown;
  try {
    envelope = JSON.parse(json);
  } catch {
    throw new BrowserCoreError("core-error");
  }
  if (typeof envelope !== "object" || envelope === null || Array.isArray(envelope)) {
    throw new BrowserCoreError("core-error");
  }
  const candidate = envelope as { ok?: unknown; value?: unknown; error?: unknown };
  if (candidate.ok === false) throw coreErrorDetails(candidate.error);
  if (candidate.ok !== true || !isSafeResult(candidate.value)) {
    throw new BrowserCoreError("core-error");
  }
  return candidate.value;
}

function coreErrorDetails(error: unknown): BrowserCoreError {
  if (typeof error !== "object" || error === null || Array.isArray(error)) return new BrowserCoreError("core-error");
  const candidate = error as { code?: unknown; currentRevision?: unknown };
  if (typeof candidate.code !== "string" || candidate.code === "core-poisoned" || !/^[a-z][a-z0-9-]{0,63}$/.test(candidate.code)) {
    return new BrowserCoreError("core-error");
  }
  const code = candidate.code;
  const details = typeof candidate.currentRevision === "string" &&
    /^(0|[1-9][0-9]{0,19})$/.test(candidate.currentRevision) &&
    BigInt(candidate.currentRevision) <= 18_446_744_073_709_551_615n
    ? { currentRevision: candidate.currentRevision }
    : undefined;
  return new CoreRejectedError(code, details);
}

function requestBytes(request: CoreRequest): Uint8Array {
  const bytes = new TextEncoder().encode(JSON.stringify(request));
  if (bytes.byteLength === 0 || bytes.byteLength > MAX_REQUEST_BYTES) {
    throw new BrowserCoreError("core-unavailable");
  }
  return bytes;
}

function validRange(heap: Uint8Array, pointer: number, length: number): boolean {
  return Number.isSafeInteger(pointer) && Number.isSafeInteger(length) && pointer > 0 &&
    length > 0 && pointer <= heap.byteLength - length;
}
function validPointer(heap: Uint8Array, pointer: number): boolean {
  return Number.isSafeInteger(pointer) && pointer > 0 && pointer < heap.byteLength;
}

/** A narrow owner for the fixed browser C ABI; it deliberately exposes no FS or raw ABI calls. */
export class BrowserCore {
  private modulePromise?: Promise<EmscriptenCoreModule>;
  public constructor(private readonly factory: BrowserCoreFactory) {}

  public async invoke(request: CoreRequest): Promise<CoreResult> {
    const module = await this.module().catch(() => { throw new BrowserCoreError("core-unavailable"); });
    const bytes = requestBytes(request);
    let pointer = 0;
    let allocated = false;
    try {
      pointer = module._peppy_browser_alloc(bytes.byteLength);
      if (!validRange(module.HEAPU8, pointer, bytes.byteLength)) {
        throw new BrowserCoreError("core-unavailable");
      }
      allocated = true;
      module.HEAPU8.set(bytes, pointer);
      const responsePointer = module._peppy_browser_dispatch(pointer, bytes.byteLength);
      if (!validPointer(module.HEAPU8, responsePointer)) {
        throw new BrowserCoreError("core-error");
      }
      try {
        return responseResult(module.UTF8ToString(responsePointer));
      } finally {
        module._peppy_browser_free_response(responsePointer);
      }
    } catch (error: unknown) {
      if (error instanceof BrowserCoreError) throw error;
      throw new BrowserCoreError("core-error");
    } finally {
      bytes.fill(0);
      if (allocated) {
        // Re-read HEAPU8 because Emscripten may grow linear memory during dispatch.
        if (validRange(module.HEAPU8, pointer, bytes.byteLength)) module.HEAPU8.fill(0, pointer, pointer + bytes.byteLength);
        module._peppy_browser_free_request(pointer, bytes.byteLength);
      }
    }
  }

  private module(): Promise<EmscriptenCoreModule> {
    this.modulePromise ??= Promise.resolve().then(this.factory);
    return this.modulePromise;
  }
}
