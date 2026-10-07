import { TransportError } from "./transport.js";

const ATTACHMENT_ID = /^[0-9a-f-]{36}$/;
const SAFE_TOKEN = /^[A-Za-z0-9_-]{1,128}$/;
const SAFE_NAME = /^[A-Za-z0-9_.-]{1,128}$/;

export type PublicCopy = { url: string; expiresInSeconds: number };

export interface PublicImageSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  publicImage?(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array; name: string }>;
}

export interface PublicCopyTransport {
  postPublicCopy<T>(path: string, bytes: Uint8Array, request: { fileName: string }): Promise<T>;
}

type PublicCopyResponse = { token?: unknown; safe_name?: unknown; expires_in_seconds?: unknown };

/** Explicitly confirmed publication of a locally re-encoded, private Worker derivative. */
export async function publishAttachment(session: PublicImageSession, transport: PublicCopyTransport, args: Record<string, unknown>): Promise<PublicCopy> {
  const id = args.id;
  if (args.confirmed !== true || typeof id !== "string" || !ATTACHMENT_ID.test(id) || !session.publicImage) throw { code: "invalid-request" };
  const remote = await session.query("_worker_attachment_remote", { id });
  const remoteObjectId = remoteObject(remote);
  if (!remoteObjectId) throw { code: "public-copy-unavailable" };
  const image = await session.publicImage(id);
  if (!(image.bytes instanceof Uint8Array) || image.bytes.byteLength === 0 || typeof image.name !== "string" || !SAFE_NAME.test(image.name)) throw { code: "public-copy-too-large" };
  const response = await transport.postPublicCopy<PublicCopyResponse>(`/v1/attachments/${encodeURIComponent(remoteObjectId)}/public-copies`, image.bytes, { fileName: image.name });
  if (!SAFE_TOKEN.test(String(response.token)) || !SAFE_NAME.test(String(response.safe_name))) throw new TransportError("invalid");
  const expiresInSeconds = response.expires_in_seconds;
  if (expiresInSeconds !== undefined && (typeof expiresInSeconds !== "number" || !Number.isSafeInteger(expiresInSeconds) || expiresInSeconds < 0 || expiresInSeconds > 31_536_000)) throw new TransportError("invalid");
  return { url: `/file/mms-usercontent/${response.token}/${response.safe_name}`, expiresInSeconds: typeof expiresInSeconds === "number" ? expiresInSeconds : 0 };
}

function remoteObject(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  const remoteObjectId = (value as Record<string, unknown>).remoteObjectId;
  if (remoteObjectId === null) return undefined;
  if (typeof remoteObjectId !== "string" || !SAFE_TOKEN.test(remoteObjectId)) throw { code: "invalid-request" };
  return remoteObjectId;
}
