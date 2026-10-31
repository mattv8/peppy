import {
  MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS,
  SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION,
  type AttachmentRetentionNoticeAcknowledgementInput,
  type AttachmentRetentionNoticeAcknowledgementResult,
  type AttachmentRetentionRequestDto,
  type AttachmentRetentionStatusResult,
  type AttachmentStorageStatusDto,
  type PendingAttachmentUseOperationDto,
} from "@peppy/generated/attachment-retention";
import { attachmentRetentionMethod, isAttachmentRemovedByStoragePolicy, TransportError } from "./transport.js";

export const MAX_RETENTION_PASSES = 8;

export interface RetentionSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
}

export interface RetentionTransport {
  retention(request: AttachmentRetentionRequestDto, signal: AbortSignal): Promise<string>;
}

type Negotiation = "unresolved" | "enabled" | "unsupported";

export type RetentionNegotiationState = "unresolved" | "enabled" | "unsupported";
export interface RetentionRoundPreparation {
  negotiationResolved: boolean;
  protocolEnabled: boolean;
  dueExhausted: boolean;
}
interface DrainResult { exhausted: boolean; progressed: boolean; }

/** Runs only Worker-private retention commands; renderer RPC never receives these controls. */
export class AttachmentRetentionDriver {
  private negotiation: Negotiation = "unresolved";
  private negotiationPromise?: Promise<boolean>;
  private drainPromise?: Promise<DrainResult>;
  private readyAdvertised = false;
  private reconciliationNeeded = true;

  public constructor(
    private readonly session: RetentionSession,
    private readonly transport: RetentionTransport,
  ) {}

  public get negotiationState(): RetentionNegotiationState { return this.negotiation; }
  public get negotiationResolved(): boolean { return this.negotiation !== "unresolved"; }

  public negotiate(signal: AbortSignal): Promise<boolean> {
    throwIfAborted(signal);
    if (this.negotiation !== "unresolved") return Promise.resolve(this.negotiation === "enabled");
    if (this.negotiationPromise) return this.negotiationPromise;
    const attempt = this.performNegotiation(signal);
    this.negotiationPromise = attempt;
    void attempt.finally(() => { if (this.negotiationPromise === attempt) this.negotiationPromise = undefined; }).catch(() => undefined);
    return attempt;
  }

  public async prepareRound(signal: AbortSignal): Promise<RetentionRoundPreparation> {
    await this.negotiate(signal);
    if (this.negotiation === "unresolved") return { negotiationResolved: false, protocolEnabled: false, dueExhausted: false };
    if (this.negotiation === "unsupported") return { negotiationResolved: true, protocolEnabled: false, dueExhausted: true };
    try {
      await this.withdrawInvalidReadiness(signal);
      const result = await this.drainDue(signal);
      if (result.progressed) this.reconciliationNeeded = true;
      return { negotiationResolved: true, protocolEnabled: true, dueExhausted: result.exhausted };
    } catch (error) {
      throwIfAborted(signal);
      if (isDeferredTransportFailure(error)) return { negotiationResolved: true, protocolEnabled: true, dueExhausted: false };
      throw error;
    }
  }

  public async reconcile(signal: AbortSignal): Promise<boolean> {
    if (this.negotiation !== "enabled") return this.negotiation === "unsupported";
    try {
      await this.withdrawInvalidReadiness(signal);
      const result = await this.drainDue(signal);
      if (result.progressed) this.reconciliationNeeded = true;
      if (!result.exhausted) return false;
      let ready = readyValue(await this.session.query("_worker_attachment_retention_ready", {}));
      throwIfAborted(signal);
      if (!ready && this.reconciliationNeeded) {
        await this.session.mutate("_worker_attachment_retention_mark_reconciled", {});
        throwIfAborted(signal);
        this.reconciliationNeeded = false;
        ready = readyValue(await this.session.query("_worker_attachment_retention_ready", {}));
        throwIfAborted(signal);
      }
      if (!ready) return false;
      if (!this.readyAdvertised) await this.setCapabilityReady(true, signal);
      this.readyAdvertised = true;
      return true;
    } catch (error) {
      throwIfAborted(signal);
      if (isDeferredTransportFailure(error)) return false;
      throw error;
    }
  }

  private async withdrawInvalidReadiness(signal: AbortSignal): Promise<void> {
    if (!this.readyAdvertised) return;
    const ready = readyValue(await this.session.query("_worker_attachment_retention_ready", {}));
    throwIfAborted(signal);
    if (ready) return;
    await this.setCapabilityReady(false, signal);
    this.readyAdvertised = false;
    this.reconciliationNeeded = true;
  }

  private async performNegotiation(signal: AbortSignal): Promise<boolean> {
    const request = requestDto(await this.session.query("_worker_attachment_retention_capability_request", { cleanupReady: false }));
    throwIfAborted(signal);
    try {
      await this.transport.retention(request, signal);
    } catch (error) {
      throwIfAborted(signal);
      if (isOldServer(error)) {
        this.negotiation = "unsupported";
        return false;
      }
      if (isDeferredTransportFailure(error)) return false;
      throw error;
    }
    throwIfAborted(signal);
    await this.session.mutate("_worker_attachment_retention_configure_protocol", { enabled: true });
    throwIfAborted(signal);
    this.negotiation = "enabled";
    return true;
  }

  private async setCapabilityReady(cleanupReady: boolean, signal: AbortSignal): Promise<void> {
    const request = requestDto(await this.session.query("_worker_attachment_retention_capability_request", { cleanupReady }));
    throwIfAborted(signal);
    await this.transport.retention(request, signal);
    throwIfAborted(signal);
  }

  private drainDue(signal: AbortSignal): Promise<DrainResult> {
    throwIfAborted(signal);
    if (this.drainPromise) return this.drainPromise;
    const drain = this.performDrain(signal);
    this.drainPromise = drain;
    void drain.finally(() => { if (this.drainPromise === drain) this.drainPromise = undefined; }).catch(() => undefined);
    return drain;
  }

  private async performDrain(signal: AbortSignal): Promise<DrainResult> {
    let progressed = false;
    if (!pendingValue(await this.session.query("_worker_attachment_retention_preparation_pending", {}))) return { exhausted: true, progressed };
    throwIfAborted(signal);
    for (let pass = 0; pass < MAX_RETENTION_PASSES; pass += 1) {
      throwIfAborted(signal);
      const prepared = operations(await this.session.mutate("_worker_attachment_retention_prepare", { limit: MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS }));
      throwIfAborted(signal);
      for (const operation of prepared) await this.transmit(operation, signal);
      progressed ||= prepared.length > 0;
      if (prepared.length > 0) continue;
      const pending = pendingValue(await this.session.query("_worker_attachment_retention_preparation_pending", {}));
      throwIfAborted(signal);
      if (!pending) return { exhausted: true, progressed };
    }
    const pending = pendingValue(await this.session.query("_worker_attachment_retention_preparation_pending", {}));
    throwIfAborted(signal);
    return { exhausted: !pending, progressed };
  }

  private async transmit(operation: PendingAttachmentUseOperationDto, signal: AbortSignal): Promise<void> {
    const request = requestDto(await this.session.query("_worker_attachment_retention_operation_request", operation));
    throwIfAborted(signal);
    let body: string;
    try {
      body = await this.transport.retention(request, signal);
    } catch (error) {
      throwIfAborted(signal);
      if (!isAttachmentRemovedByStoragePolicy(error)) throw error;
      await this.session.mutate("_worker_attachment_retention_report_removed", { attachmentId: operation.attachmentId });
      throwIfAborted(signal);
      return;
    }
    throwIfAborted(signal);
    const response = await this.session.query("_worker_attachment_retention_parse_operation_response", { body });
    throwIfAborted(signal);
    await this.session.mutate("_worker_attachment_retention_apply_response", record(response));
    throwIfAborted(signal);
  }
}

interface NoticeScope { id: string; generation: number; binding: string; }

export class AttachmentRetentionNoticeFacade {
  private scope?: NoticeScope;

  public constructor(
    private readonly session: RetentionSession,
    private readonly transport: RetentionTransport,
    private readonly generation: () => number,
    private readonly signal: () => AbortSignal,
  ) {}

  public clear(): void { this.scope = undefined; }

  public async status(): Promise<AttachmentRetentionStatusResult> {
    const generation = this.generation();
    const signal = this.signal();
    try {
      const binding = await this.activeBinding(signal, generation);
      const request = requestDto(await this.session.query("_worker_attachment_retention_status_request", {}));
      assertActive(signal, generation, this.generation());
      const body = await this.transport.retention(request, signal);
      assertActive(signal, generation, this.generation());
      const status = record(await this.session.query("_worker_attachment_retention_parse_status", { body })) as AttachmentStorageStatusDto;
      assertActive(signal, generation, this.generation());
      const scope = { id: crypto.randomUUID(), generation, binding };
      this.scope = scope;
      return { kind: "available", scopeId: scope.id, status };
    } catch (error) {
      return statusFailure(error);
    }
  }

  public async acknowledge(input: AttachmentRetentionNoticeAcknowledgementInput): Promise<AttachmentRetentionNoticeAcknowledgementResult> {
    if (!validText(input.scopeId) || !validText(input.policyVersion)) return "stale_scope";
    if (input.policyVersion !== SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION) return "unsupported_policy_version";
    const scope = this.scope;
    const signal = this.signal();
    if (!scope || scope.id !== input.scopeId || scope.generation !== this.generation() || signal.aborted) return "stale_scope";
    try {
      if (await this.activeBinding(signal, scope.generation) !== scope.binding) return "stale_scope";
      const request = requestDto(await this.session.query("_worker_attachment_retention_notice_acknowledgement_request", { policyVersion: input.policyVersion }));
      if (this.scope !== scope) return "stale_scope";
      assertActive(signal, scope.generation, this.generation());
      await this.transport.retention(request, signal);
      assertActive(signal, scope.generation, this.generation());
      return "acknowledged";
    } catch (error) {
      const failure = statusFailure(error);
      return failure.kind === "unsupported" ? "unsupported" : failure.kind === "temporarily_unavailable" ? "temporarily_unavailable" : "stale_scope";
    }
  }

  private async activeBinding(signal: AbortSignal, generation: number): Promise<string> {
    const identity = record(await this.session.query("_worker_identity_metadata", {}));
    assertActive(signal, generation, this.generation());
    return `${requiredText(identity.vaultId)}:${requiredText(identity.deviceId)}`;
  }
}

function readyValue(value: unknown): boolean { return record(value).ready === true; }
function pendingValue(value: unknown): boolean { return record(value).pending === true; }
function operations(value: unknown): PendingAttachmentUseOperationDto[] {
  const result = record(value).operations;
  if (!Array.isArray(result) || result.length > MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS || result.some(item => !isOperation(item))) throw new TransportError("invalid");
  return result;
}
function isOperation(value: unknown): value is PendingAttachmentUseOperationDto {
  if (!isRecord(value)) return false;
  return (value.operation === "register" || value.operation === "release")
    && typeof value.useId === "string" && value.useId.length > 0
    && typeof value.attachmentId === "string" && value.attachmentId.length > 0
    && typeof value.deviceId === "string" && value.deviceId.length > 0
    && ["message_history", "pending_send", "contact_photo", "restore_critical"].includes(String(value.kind));
}
function requestDto(value: unknown): AttachmentRetentionRequestDto {
  const request = record(value);
  if (typeof request.method !== "string" || typeof request.relativePath !== "string" || typeof request.expectedSuccessStatus !== "number") throw new TransportError("invalid");
  attachmentRetentionMethod(request.method);
  return request as AttachmentRetentionRequestDto;
}
function record(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new TransportError("invalid");
  return value;
}
function isRecord(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function isOldServer(error: unknown): boolean { return error instanceof TransportError && error.kind === "status" && (error.status === 404 || error.status === 405); }
function isDeferredTransportFailure(error: unknown): boolean { return error instanceof TransportError && error.kind !== "cancelled" && error.kind !== "revoked" && error.kind !== "invalid"; }
function statusFailure(error: unknown): Exclude<AttachmentRetentionStatusResult, { kind: "available" }> {
  if (error instanceof TransportError && error.kind === "status" && error.status === 501 && error.code === "attachment_retention_unsupported") return { kind: "unsupported" };
  if (error instanceof TransportError && (error.kind === "revoked" || error.status === 403 || error.status === 503 && error.code === "attachment_retention_unavailable")) return { kind: "temporarily_unavailable" };
  if (error instanceof TransportError && (error.kind === "offline" || error.kind === "cancelled")) return { kind: "temporarily_unavailable" };
  throw error;
}
function validText(value: unknown): value is string { return typeof value === "string" && value.length > 0; }
function requiredText(value: unknown): string { if (!validText(value)) throw new TransportError("invalid"); return value; }
function assertActive(signal: AbortSignal, expected: number, actual: number): void { if (signal.aborted || expected !== actual) throw new TransportError("cancelled"); }
function throwIfAborted(signal: AbortSignal): void { if (signal.aborted) throw new TransportError("cancelled"); }
