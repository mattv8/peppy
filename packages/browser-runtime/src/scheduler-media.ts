import { TransportError } from "./transport.js";

const MAX_MEDIA_PER_ROUND = 4;

export interface SchedulerMediaSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
  hydrate(id: string): Promise<void>;
}

/** Implemented by the Worker host; paths are Worker-private Emscripten references. */
export interface WorkerMediaBridge {
  readCiphertext(file: string, expectedBytes: number, signal: AbortSignal): Promise<Uint8Array>;
  stageDownload(bytes: Uint8Array, signal: AbortSignal): Promise<string>;
  cleanupStagedDownload?(source: string): Promise<void>;
}
export interface MediaTransferResult { failedIds: string[]; completedIds: string[]; }
interface MediaTransport {
  json(path: string, request: { method: "GET" | "POST" | "DELETE"; body?: unknown; signal?: AbortSignal; limit?: number }): Promise<unknown>;
  upload(path: string, bytes: Uint8Array, request?: { signal?: AbortSignal }): Promise<void>;
  download(path: string, request: { signal?: AbortSignal; limit?: number }): Promise<Uint8Array>;
}

export async function transferMedia(
  session: SchedulerMediaSession,
  transport: MediaTransport,
  bridge: WorkerMediaBridge | undefined,
  signal: AbortSignal,
  deferredIds: ReadonlySet<string> = new Set(),
): Promise<MediaTransferResult> {
  const result: MediaTransferResult = { failedIds: [], completedIds: [] };
  throwIfAborted(signal);
  if (!bridge) return result;
  await transferUploads(session, transport, bridge, signal, deferredIds, result);
  await transferDownloads(session, transport, bridge, signal, deferredIds, result);
  return result;
}

/** Uploads only core-designated contact photos before their referencing envelopes are sealed. */
export async function transferContactPhotoUploads(
  session: SchedulerMediaSession,
  transport: MediaTransport,
  bridge: WorkerMediaBridge | undefined,
  ids: readonly string[],
  signal: AbortSignal,
  deferredIds: ReadonlySet<string> = new Set(),
): Promise<MediaTransferResult> {
  const result: MediaTransferResult = { failedIds: [], completedIds: [] };
  if (!bridge || ids.length === 0) return result;
  const permitted = new Set(ids);
  await transferUploads(session, transport, bridge, signal, deferredIds, result, permitted);
  return result;
}

async function transferUploads(session: SchedulerMediaSession, transport: Pick<MediaTransport, "json" | "upload">, bridge: WorkerMediaBridge, signal: AbortSignal, deferredIds: ReadonlySet<string>, result: MediaTransferResult, permittedIds?: ReadonlySet<string>): Promise<void> {
  const pending = array(await session.query("pending_uploads", {}));
  const referenceTrackedIds = await referenceTrackedUploadIds(session);
  for (const item of pending.filter(item => isEligibleAttachment(item, deferredIds) && (permittedIds === undefined || permittedIds.has(optionalAttachmentId(item) ?? ""))).slice(0, MAX_MEDIA_PER_ROUND)) {
    throwIfAborted(signal);
    try {
      const attachment = attachmentItem(item);
      await uploadAttachment(session, transport, bridge, attachment, referenceTrackedIds.has(attachment.id), signal);
      result.completedIds.push(attachment.id);
    } catch (error) {
      throwIfFatalTransferError(error, signal);
      const id = optionalAttachmentId(item);
      if (id) result.failedIds.push(id);
    }
  }
}

async function uploadAttachment(session: SchedulerMediaSession, transport: Pick<MediaTransport, "json" | "upload">, bridge: WorkerMediaBridge, attachment: AttachmentItem, referenceTracking: boolean, signal: AbortSignal): Promise<void> {
  await session.hydrate(attachment.id);
  const cipher = object(await session.query("cipher_file", { id: attachment.id }));
  const ciphertext = await bridge.readCiphertext(requiredString(cipher.file), attachment.ciphertextBytes, signal);
  if (ciphertext.byteLength !== attachment.ciphertextBytes) throw new TransportError("invalid");
  let remoteObjectId: string;
  try {
    const reserved = object(await transport.json("/v1/attachments/reserve", {
      method: "POST",
      body: { attachment_id: attachment.id, declared_ciphertext_bytes: attachment.ciphertextBytes, declared_ciphertext_sha256: requiredString(cipher.ciphertextSha256), reference_tracking: referenceTracking },
      signal,
    }));
    remoteObjectId = exactAttachmentId(reserved, attachment.id);
    await transport.upload(`/v1/attachments/${encodeURIComponent(remoteObjectId)}/upload`, ciphertext, { signal });
  } catch (error) {
    if (!isReservationConflict(error)) throw error;
    await finalizeAttachment(transport, attachment.id, signal, true);
    await session.mutate("mark_attachment_uploaded", { id: attachment.id, remoteObjectId: attachment.id });
    return;
  }
  await finalizeAttachment(transport, remoteObjectId, signal, false);
  await session.mutate("mark_attachment_uploaded", { id: attachment.id, remoteObjectId });
}

async function transferDownloads(session: SchedulerMediaSession, transport: Pick<MediaTransport, "download">, bridge: WorkerMediaBridge, signal: AbortSignal, deferredIds: ReadonlySet<string>, result: MediaTransferResult): Promise<void> {
  const pending = array(await session.query("pending_downloads", {}));
  for (const item of pending.filter(item => isEligibleAttachment(item, deferredIds)).slice(0, MAX_MEDIA_PER_ROUND)) {
    throwIfAborted(signal);
    try {
      const attachment = attachmentItem(item);
      await downloadAttachment(session, transport, bridge, attachment, requiredString(object(item).remoteObjectId), signal);
      result.completedIds.push(attachment.id);
    } catch (error) {
      throwIfFatalTransferError(error, signal);
      const id = optionalAttachmentId(item);
      if (id) result.failedIds.push(id);
    }
  }
}

async function downloadAttachment(session: SchedulerMediaSession, transport: Pick<MediaTransport, "download">, bridge: WorkerMediaBridge, attachment: AttachmentItem, remoteObjectId: string, signal: AbortSignal): Promise<void> {
  const ciphertext = await transport.download(`/v1/attachments/${encodeURIComponent(remoteObjectId)}`, { signal, limit: attachment.ciphertextBytes });
  if (ciphertext.byteLength !== attachment.ciphertextBytes) throw new TransportError("invalid");
  const source = await bridge.stageDownload(ciphertext, signal);
  try {
    await session.mutate("install_downloaded_attachment", { id: attachment.id, source });
  } finally {
    await bridge.cleanupStagedDownload?.(source);
  }
}

async function finalizeAttachment(transport: Pick<MediaTransport, "json" | "upload">, id: string, signal: AbortSignal, requireDuplicate: boolean): Promise<void> {
  const finalized = object(await transport.json(`/v1/attachments/${encodeURIComponent(id)}/finalize`, { method: "POST", signal }));
  if (exactAttachmentId(finalized, id) !== id || (requireDuplicate && finalized.duplicate !== true)) throw new TransportError("invalid");
}

async function referenceTrackedUploadIds(session: SchedulerMediaSession): Promise<Set<string>> {
  const state = object(await session.query("contact_photo_transfer_state", {}));
  return new Set(array(state.uploads).map(item => {
    const upload = object(item);
    if (upload.reference_tracking !== true) throw new TransportError("invalid");
    return requiredString(upload.attachment_id);
  }));
}

interface AttachmentItem { id: string; ciphertextBytes: number; }
function attachmentItem(value: unknown): AttachmentItem {
  const item = object(value);
  const ciphertextBytes = item.ciphertextBytes;
  if (typeof ciphertextBytes !== "number" || !Number.isSafeInteger(ciphertextBytes) || ciphertextBytes <= 0) throw new TransportError("invalid");
  return { id: requiredString(item.id), ciphertextBytes };
}

function optionalAttachmentId(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const id = (value as { id?: unknown }).id;
  return typeof id === "string" && id.length > 0 ? id : undefined;
}

function isEligibleAttachment(value: unknown, deferredIds: ReadonlySet<string>): boolean {
  const id = optionalAttachmentId(value);
  return !id || !deferredIds.has(id);
}

function exactAttachmentId(value: Record<string, unknown>, expected: string): string {
  const id = requiredString(value.attachment_id);
  if (id !== expected) throw new TransportError("invalid");
  return id;
}
function isReservationConflict(error: unknown): boolean { return error instanceof TransportError && error.kind === "status" && error.status === 409; }
export function throwIfFatalTransferError(error: unknown, signal: AbortSignal): void {
  if (signal.aborted || error instanceof TransportError && (error.kind === "cancelled" || error.kind === "revoked" || error.kind === "offline")) throw error;
}

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid");
  return value as Record<string, unknown>;
}

function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new TransportError("invalid");
  return value;
}

function requiredString(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) throw new TransportError("invalid");
  return value;
}

function throwIfAborted(signal: AbortSignal): void {
  if (signal.aborted) throw new TransportError("cancelled");
}
