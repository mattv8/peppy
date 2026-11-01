import type { AccountAccessResult } from "@peppy/generated/account-access";
import type { AttachmentRetentionRequestDto } from "@peppy/generated/attachment-retention";
import { TransportError } from "./transport.js";

const ACCOUNT_ACCESS_PATH = "/v1/account/access";

export interface AccountAccessSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
}

export interface AccountAccessTransport {
  retention(request: AttachmentRetentionRequestDto, signal: AbortSignal): Promise<string>;
}

/**
 * Reads the device-authenticated account notice. The transport only reports the HTTP
 * status and body; Rust core classifies them and decides the notice.
 */
export class AccountAccessDriver {
  public constructor(
    private readonly session: AccountAccessSession,
    private readonly transport: AccountAccessTransport,
    private readonly trustedOrigins: () => string[],
    private readonly generation: () => number,
    private readonly signal: () => AbortSignal,
  ) {}

  public async status(): Promise<AccountAccessResult> {
    const generation = this.generation();
    const signal = this.signal();
    try {
      const request = requestDto(await this.session.query("_worker_account_access_request", {}));
      assertActive(signal, generation, this.generation());
      const response = await this.send(request, signal);
      assertActive(signal, generation, this.generation());
      return accountAccessResult(await this.session.query("_worker_account_access_parse", {
        ...response,
        nowUnixSeconds: Math.floor(Date.now() / 1000),
        trustedOrigins: this.trustedOrigins(),
      }));
    } catch {
      return { kind: "temporarily_unavailable" };
    }
  }

  /** Network and cancellation failures use status 0, which core classifies as transient. */
  private async send(request: AttachmentRetentionRequestDto, signal: AbortSignal): Promise<{ status: number; body: string }> {
    try {
      return { status: 200, body: await this.transport.retention(request, signal) };
    } catch (error) {
      if (error instanceof TransportError && error.kind === "status" && typeof error.status === "number") return { status: error.status, body: "" };
      return { status: 0, body: "" };
    }
  }
}

function requestDto(value: unknown): AttachmentRetentionRequestDto {
  const request = isRecord(value) ? value : undefined;
  if (request?.method !== "GET" || request.relativePath !== ACCOUNT_ACCESS_PATH) throw new Error("invalid account access request");
  return request as unknown as AttachmentRetentionRequestDto;
}

function accountAccessResult(value: unknown): AccountAccessResult {
  const kind = isRecord(value) ? value.kind : undefined;
  if (kind === "notice" || kind === "none" || kind === "unsupported" || kind === "temporarily_unavailable") return value as AccountAccessResult;
  return { kind: "temporarily_unavailable" };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assertActive(signal: AbortSignal, expected: number, actual: number): void {
  if (signal.aborted || expected !== actual) throw new Error("account access is no longer current");
}
