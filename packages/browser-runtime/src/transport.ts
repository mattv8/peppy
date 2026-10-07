const MAX_JSON_BYTES = 1024 * 1024;
const MAX_SNAPSHOT_PAGE_BYTES = 8 * 1024 * 1024;
const MAX_ATTACHMENT_CIPHERTEXT_BYTES = 33 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES = 4 * 1024;
const JSON_DEADLINE_MS = 20_000;
const TRANSFER_DEADLINE_MS = 30_000;
const DEVICE_TOKEN = /^[a-fA-F0-9]{96}$/;
const SERVER_CODE = /^[a-z_]{1,64}$/;
const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/;
const ENCODED_BACKSLASH = /%5c/i;
const CREDENTIAL_QUERY_PARAMETER = /^(?:access_?token|authorization|bearer|device_?token|token)$/i;

export type TransportErrorKind = "offline" | "revoked" | "status" | "too-large" | "invalid" | "cancelled";
export type JsonMethod = "GET" | "POST" | "DELETE";

export class TransportError extends Error {
  public constructor(
    public readonly kind: TransportErrorKind,
    public readonly status?: number,
    public readonly code?: string,
  ) {
    super("Transport request failed");
  }
}

export interface OriginTransportOptions {
  origin: string;
  deviceToken: () => string | undefined;
  fetch?: typeof fetch;
}

export interface JsonRequest {
  method: JsonMethod;
  body?: unknown;
  signal?: AbortSignal;
  limit?: number;
}

export interface TransferRequest {
  signal?: AbortSignal;
  limit?: number;
}

export interface UploadRequest {
  signal?: AbortSignal;
}

export interface PublicCopyRequest extends UploadRequest {
  fileName: string;
}

/** Worker-owned, device-authenticated HTTP transport. It does not schedule or retry requests. */
export class OriginTransport {
  private readonly origin: URL;
  private readonly fetcher: typeof fetch;

  public constructor(private readonly options: OriginTransportOptions) {
    this.origin = trustedOrigin(options.origin);
    this.fetcher = options.fetch ?? fetch.bind(globalThis);
  }

  public async json<T>(path: string, request: JsonRequest): Promise<T> {
    const limit = boundedLimit(request.limit, MAX_JSON_BYTES, MAX_SNAPSHOT_PAGE_BYTES);
    let body: string | undefined;
    try {
      body = request.body === undefined ? undefined : JSON.stringify(request.body);
    } catch {
      throw new TransportError("invalid");
    }
    if (body === undefined && request.body !== undefined) throw new TransportError("invalid");
    if (body !== undefined && new TextEncoder().encode(body).byteLength > MAX_SNAPSHOT_PAGE_BYTES) {
      throw new TransportError("too-large");
    }
    const call = await this.request(path, request.method, body, request.signal, JSON_DEADLINE_MS, body === undefined ? undefined : "application/json");
    return completeResponse(call, async () => {
      await assertSuccess(call.response, call.signal);
      const bytes = await readBounded(call.response, limit, call.signal, request.signal);
      try {
        return JSON.parse(new TextDecoder().decode(bytes)) as T;
      } catch {
        throw new TransportError("invalid");
      }
    });
  }

  /** Sends a successful JSON request whose response body is deliberately ignored. */
  public async discard(path: string, request: Omit<JsonRequest, "limit">): Promise<void> {
    let body: string | undefined;
    try {
      body = request.body === undefined ? undefined : JSON.stringify(request.body);
    } catch {
      throw new TransportError("invalid");
    }
    if (body === undefined && request.body !== undefined) throw new TransportError("invalid");
    const call = await this.request(path, request.method, body, request.signal, JSON_DEADLINE_MS, body === undefined ? undefined : "application/json");
    await completeResponse(call, async () => {
      await assertSuccess(call.response, call.signal);
      await readBounded(call.response, MAX_ERROR_BODY_BYTES, call.signal, request.signal);
    });
  }

  public async upload(path: string, bytes: Uint8Array, request: UploadRequest = {}): Promise<void> {
    if (bytes.byteLength > MAX_ATTACHMENT_CIPHERTEXT_BYTES) throw new TransportError("too-large");
    const call = await this.request(path, "PUT", bytes as unknown as BodyInit, request.signal, TRANSFER_DEADLINE_MS, "application/octet-stream");
    await completeResponse(call, async () => {
      await assertSuccess(call.response, call.signal);
      await readBounded(call.response, MAX_ERROR_BODY_BYTES, call.signal, request.signal);
    });
  }

  /** Uploads a bounded, re-encoded public derivative and returns the server response. */
  public async postPublicCopy<T>(path: string, bytes: Uint8Array, request: PublicCopyRequest): Promise<T> {
    if (bytes.byteLength === 0 || bytes.byteLength > MAX_ATTACHMENT_CIPHERTEXT_BYTES || !/^[A-Za-z0-9_.-]{1,128}$/.test(request.fileName)) throw new TransportError("invalid");
    const call = await this.request(path, "POST", bytes as unknown as BodyInit, request.signal, TRANSFER_DEADLINE_MS, "application/octet-stream", { "X-File-Name": request.fileName });
    return completeResponse(call, async () => {
      await assertSuccess(call.response, call.signal);
      const response = await readBounded(call.response, MAX_JSON_BYTES, call.signal, request.signal);
      try { return JSON.parse(new TextDecoder().decode(response)) as T; } catch { throw new TransportError("invalid"); }
    });
  }

  public async download(path: string, request: TransferRequest): Promise<Uint8Array> {
    const limit = boundedLimit(request.limit, MAX_ATTACHMENT_CIPHERTEXT_BYTES, MAX_ATTACHMENT_CIPHERTEXT_BYTES);
    const call = await this.request(path, "GET", undefined, request.signal, TRANSFER_DEADLINE_MS);
    return completeResponse(call, async () => {
      await assertSuccess(call.response, call.signal);
      return await readBounded(call.response, limit, call.signal, request.signal);
    });
  }

  private async request(path: string, method: string, body: BodyInit | undefined, signal: AbortSignal | undefined, deadline: number, contentType?: string, extraHeaders?: Record<string, string>): Promise<ActiveRequest> {
    if (signal?.aborted) throw new TransportError("cancelled");
    const url = requestUrl(this.origin, path);
    let token: string | undefined;
    try {
      token = this.options.deviceToken();
    } catch {
      throw new TransportError("invalid");
    }
    if (token === undefined || !DEVICE_TOKEN.test(token)) throw new TransportError("invalid");
    const controller = new AbortController();
    let requestStarted = false;
    let deadlineExpired = false;
    const abortFromCaller = () => controller.abort();
    signal?.addEventListener("abort", abortFromCaller, { once: true });
    const timer = setTimeout(() => {
      deadlineExpired = true;
      controller.abort();
    }, deadline);
    try {
      const headers = new Headers({ Authorization: `Bearer ${token}` });
      if (contentType !== undefined) headers.set("Content-Type", contentType);
      for (const [name, value] of Object.entries(extraHeaders ?? {})) headers.set(name, value);
      const response = await this.fetcher(url, {
        method,
        body,
        headers,
        signal: controller.signal,
        redirect: "error",
        credentials: "omit",
        cache: "no-store",
        referrerPolicy: "no-referrer",
      });
      if (response.redirected || (response.url !== "" && response.url !== url)) {
        cancelBody(response);
        throw new TransportError("invalid");
      }
      requestStarted = true;
      return { response, signal: controller.signal, finish: () => { clearTimeout(timer); signal?.removeEventListener("abort", abortFromCaller); } };
    } catch (error: unknown) {
      if (error instanceof TransportError) throw error;
      if (signal?.aborted) throw new TransportError("cancelled");
      if (deadlineExpired) throw new TransportError("offline");
      throw new TransportError("offline");
    } finally { if (!requestStarted) { clearTimeout(timer); signal?.removeEventListener("abort", abortFromCaller); } }
  }
}

interface ActiveRequest {
  response: Response;
  signal: AbortSignal;
  finish: () => void;
}

async function completeResponse<T>(call: ActiveRequest, operation: () => Promise<T>): Promise<T> {
  try {
    return await operation();
  } catch (error: unknown) {
    cancelBody(call.response);
    throw error;
  } finally {
    call.finish();
  }
}

function cancelBody(response: Response): void {
  if (response.body === null) return;
  try {
    void response.body.cancel().catch(() => undefined);
  } catch {
    // Cancellation is best-effort and must not replace the transport error.
  }
}

function trustedOrigin(value: string): URL {
  let origin: URL;
  try { origin = new URL(value); } catch { throw new TransportError("invalid"); }
  const loopbackHttp = origin.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname);
  if ((!loopbackHttp && origin.protocol !== "https:") || origin.username !== "" || origin.password !== "" || origin.search !== "" || origin.hash !== "" || origin.pathname !== "/") {
    throw new TransportError("invalid");
  }
  return origin;
}

function requestUrl(origin: URL, path: string): string {
  if (!path.startsWith("/v1/") || path.startsWith("//") || path.includes("\\") || ENCODED_BACKSLASH.test(path) || CONTROL_CHARACTER.test(path) || path.includes("#")) {
    throw new TransportError("invalid");
  }
  let url: URL;
  try {
    const rawPathname = path.split("?", 1)[0];
    const decodedSegments = decodeURIComponent(rawPathname).split("/");
    if (decodedSegments.some((segment) => segment === "." || segment === "..")) throw new TransportError("invalid");
    url = new URL(path, origin);
  } catch {
    throw new TransportError("invalid");
  }
  if (!url.pathname.startsWith("/v1/") || url.origin !== origin.origin || [...url.searchParams.keys()].some((key) => CREDENTIAL_QUERY_PARAMETER.test(key))) {
    throw new TransportError("invalid");
  }
  return url.toString();
}

function boundedLimit(limit: number | undefined, fallback: number, maximum: number): number {
  const value = limit ?? fallback;
  if (!Number.isSafeInteger(value) || value <= 0 || value > maximum) throw new TransportError("invalid");
  return value;
}

async function assertSuccess(response: Response, deadlineSignal: AbortSignal): Promise<void> {
  if (response.ok) return;
  if (response.status === 401) throw new TransportError("revoked");
  const code = await readBounded(response, MAX_ERROR_BODY_BYTES, deadlineSignal)
    .then(serverCode)
    .catch(() => undefined);
  throw new TransportError("status", response.status, code);
}

function serverCode(bytes: Uint8Array): string | undefined {
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    const code = typeof value === "object" && value !== null && "code" in value ? value.code : undefined;
    return typeof code === "string" && SERVER_CODE.test(code) ? code : undefined;
  } catch {
    return undefined;
  }
}

async function readBounded(response: Response, limit: number, deadlineSignal: AbortSignal, callerSignal?: AbortSignal): Promise<Uint8Array> {
  if (response.body === null) return new Uint8Array();
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    const length = response.headers.get("content-length");
    if (length !== null && Number(length) > limit) throw new TransportError("too-large");
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      total += next.value.byteLength;
      if (total > limit) throw new TransportError("too-large");
      chunks.push(next.value);
    }
  } catch (error: unknown) {
    try {
      void reader.cancel().catch(() => undefined);
    } catch {
      // Cancellation is best-effort and must not replace the transport error.
    }
    if (error instanceof TransportError) throw error;
    throw new TransportError(callerSignal?.aborted ? "cancelled" : deadlineSignal.aborted ? "offline" : "offline");
  } finally {
    reader.releaseLock();
  }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
  return body;
}
