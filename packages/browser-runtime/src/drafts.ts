import { TransportError } from "./transport.js";

export type DraftChangesResponse = { changes: DraftChangeWire[]; next_after: string; more: boolean };
export type DraftChangeWire = Record<string, unknown>;
export type DraftPushWire = { draftId: string; revisionId: string; request: Record<string, unknown> };
export type DraftPushOutcomeWire =
  | { kind: "accepted"; branchId: string; expiresAtMs: number; conflict: boolean }
  | { kind: "terminated"; reason: "sent" | "discarded" | "expired" }
  | { kind: "branch-limit" }
  | { kind: "attachment-unavailable" }
  | { kind: "rejected" };

// Two MAX_DRAFT_RECORD_BYTES (256 KiB) sealed records plus base64 and JSON remain below the 1 MiB browser response cap.
const DRAFT_PAGE_LIMIT = 2;
const DRAFT_PAGE_BYTES = 1024 * 1024;

type DraftHttpTransport = {
  json(path: string, request: { method: "GET" | "POST" | "PUT"; body?: unknown; signal?: AbortSignal; limit?: number }): Promise<unknown>;
  discard(path: string, request: { method: "POST"; body?: unknown; signal?: AbortSignal }): Promise<void>;
};

/** Worker-private authenticated REST adapter for encrypted draft records. */
export class DraftTransport {
  public constructor(private readonly transport: DraftHttpTransport) {}

  public async draftChanges(after: string, signal: AbortSignal): Promise<DraftChangesResponse> {
    canonicalCursor(after);
    return draftChanges(await this.transport.json(`/v1/drafts?after=${encodeURIComponent(after)}&limit=${DRAFT_PAGE_LIMIT}`, { method: "GET", signal, limit: DRAFT_PAGE_BYTES }));
  }

  public async putDraft(push: DraftPushWire, signal: AbortSignal): Promise<DraftPushOutcomeWire> {
    try {
      const response = draftRevision(await this.transport.json(`/v1/drafts/${id(push.draftId)}/revisions/${id(push.revisionId)}`, { method: "PUT", body: push.request, signal }));
      return { kind: "accepted", branchId: response.branch_id, expiresAtMs: rfc3339Ms(response.expires_at), conflict: response.conflict };
    } catch (error) {
      return pushOutcome(error);
    }
  }

  public async acceptDraftSend(input: { draftId: string; revisionId: string; commandId: string }, signal: AbortSignal): Promise<{ commandId: string; duplicate: boolean }> {
    const response = object(await this.transport.json(`/v1/drafts/${id(input.draftId)}/send`, { method: "POST", body: { revision_id: id(input.revisionId), command_id: id(input.commandId) }, signal }));
    const commandId = id(response.command_id);
    if (commandId !== input.commandId || typeof response.duplicate !== "boolean") throw new TransportError("invalid");
    return { commandId, duplicate: response.duplicate };
  }

  public discardDraft(draftId: string, signal: AbortSignal): Promise<void> {
    return this.transport.discard(`/v1/drafts/${id(draftId)}/discard`, { method: "POST", body: {}, signal });
  }

  public async probeAuthenticatedRoute(signal: AbortSignal): Promise<boolean> {
    try {
      object(await this.transport.json("/v1/gateways", { method: "GET", signal }));
      return true;
    } catch {
      return false;
    }
  }
}

function pushOutcome(error: unknown): DraftPushOutcomeWire {
  if (!(error instanceof TransportError) || error.kind !== "status") throw error;
  if (error.status === 410) {
    if (error.code === "draft_sent") return { kind: "terminated", reason: "sent" };
    if (error.code === "draft_discarded") return { kind: "terminated", reason: "discarded" };
    if (error.code === "draft_expired") return { kind: "terminated", reason: "expired" };
  }
  if (error.status === 409 && error.code === "draft_branch_limit") return { kind: "branch-limit" };
  if (error.status === 422 && error.code === "draft_attachment_unavailable") return { kind: "attachment-unavailable" };
  if (
    (error.status === 413 && error.code === "draft_record_too_large")
    || (error.status === 422 && error.code === "draft_invalid_request")
  ) return { kind: "rejected" };
  throw error;
}


function draftChanges(value: unknown): DraftChangesResponse {
  const response = object(value);
  if (!Array.isArray(response.changes) || typeof response.more !== "boolean") throw new TransportError("invalid");
  canonicalCursor(response.next_after);
  return { changes: response.changes.map(change => object(change)), next_after: response.next_after, more: response.more };
}

function draftRevision(value: unknown): { branch_id: string; expires_at: string; conflict: boolean } {
  const response = object(value);
  if (typeof response.conflict !== "boolean") throw new TransportError("invalid");
  return { branch_id: id(response.branch_id), expires_at: rfc3339(response.expires_at), conflict: response.conflict };
}

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid");
  return value as Record<string, unknown>;
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(value)) throw new TransportError("invalid");
  return value;
}
function canonicalCursor(value: unknown): asserts value is string {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) throw new TransportError("invalid");
}
function rfc3339(value: unknown): string {
  if (typeof value !== "string" || !Number.isSafeInteger(Date.parse(value))) throw new TransportError("invalid");
  return value;
}
function rfc3339Ms(value: unknown): number { return Date.parse(rfc3339(value)); }
