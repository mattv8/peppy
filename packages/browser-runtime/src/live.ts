import { OriginTransport, TransportError } from "./transport.js";

const TICKET_PATH = "/v1/live/tickets";
const LIVE_PATH = "/v1/live";
const SESSION_BYTES = 16;
const BACKOFF_MIN_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;

export type LiveSocket = Pick<WebSocket, "close" | "send" | "protocol" | "readyState"> & {
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<unknown>) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
};

export interface LiveSupervisorOptions {
  transport: OriginTransport;
  origin: string;
  socket: (url: string, protocol: string) => LiveSocket;
  randomBytes?: (bytes: Uint8Array) => Uint8Array;
  schedule?: (work: () => void, delayMs: number) => unknown;
  cancel?: (handle: unknown) => void;
  onFrame?: (frameJson: string) => void;
}

export interface LiveCore {
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
}

export type LiveTypingContext = {
  trustedRoster: string[];
  localGatewayDeviceId: string;
  executor: "none" | "current_gateway";
};

export type TypingDisplayEvent = {
  event: "typing";
  peerDeviceId: string;
  conversationId: string;
  /** Canonical E.164 group member; absent for 1:1 typing. */
  participant?: string;
  state: "started" | "stopped";
  deadlineMono: string;
};

/** Core-only typing bridge. Callers supply trusted host context, never renderer input. */
export class LiveTypingEngine {
  private generation: string | undefined;
  private incarnation = Symbol("disabled");
  private readonly peerSessions = new Map<string, PeerPublicationState>();

  public constructor(
    private readonly core: LiveCore,
    private readonly transport: Pick<LiveSupervisor, "transmit">,
    private readonly clock: () => { wallMs: number; monoMs: number },
    private readonly display: (event: TypingDisplayEvent) => void,
  ) {}

  public async enable(context: LiveTypingContext): Promise<void> {
    const incarnation = this.retire();
    const value = record(await this.core.mutate("_worker_typing_enable", { ...context, clock: this.clock() }));
    if (incarnation !== this.incarnation) return;
    this.generation = decimal(value.generation);
  }

  public async disable(): Promise<void> {
    this.retire();
    await this.core.mutate("_worker_typing_disable", {});
  }

  /** The core commits the sharing transition; it remains the OD-1 admission authority. */
  public async setSharing(sharing: boolean): Promise<void> {
    await this.core.mutate("_worker_typing_set_sharing", { sharing });
  }

  public async send(conversationId: string, gatewayDeviceId: string, state: "started" | "stopped"): Promise<boolean> {
    const expectedGeneration = this.generation;
    const incarnation = this.incarnation;
    if (!expectedGeneration) return false;
    const value = await this.core.mutate("_worker_typing_seal", {
      conversationId,
      gatewayDeviceId,
      origin: "vault_device",
      state,
      clock: this.clock(),
    });
    if (!this.isCurrent(incarnation, expectedGeneration) || value === null) return false;
    const sealed = record(value);
    if (decimal(sealed.generation) !== expectedGeneration || typeof sealed.frameJson !== "string") return false;
    if (!this.isCurrent(incarnation, expectedGeneration)) return false;
    return this.transport.transmit(sealed.frameJson);
  }

  public async receive(frameJson: string): Promise<void> {
    const expectedGeneration = this.generation;
    const incarnation = this.incarnation;
    if (!expectedGeneration) return;
    const outcome = record(await this.core.mutate("_worker_typing_open", { frameJson, clock: this.clock() }));
    if (!this.isCurrent(incarnation, expectedGeneration) || decimal(outcome.generation) !== expectedGeneration) return;
    this.applySessionUpdate(outcome.sessionUpdate);
    this.publishAction(outcome.action, incarnation, expectedGeneration);
    if (typeof outcome.replyFrameJson === "string" && this.isCurrent(incarnation, expectedGeneration)) {
      this.transport.transmit(outcome.replyFrameJson);
    }
  }

  private retire(): symbol {
    const incarnation = Symbol("live");
    this.incarnation = incarnation;
    this.generation = undefined;
    this.peerSessions.clear();
    return incarnation;
  }

  private isCurrent(incarnation: symbol, generation: string): boolean {
    return incarnation === this.incarnation && generation === this.generation;
  }

  private applySessionUpdate(value: unknown): void {
    if (value === undefined || value === null) return;
    const update = record(value);
    if (typeof update.peerDeviceId !== "string") throw new TransportError("invalid");
    const sessionToken = decimal(update.currentSessionToken);
    const validationRevision = BigInt(decimal(update.validationRevision));
    const current = this.peerSessions.get(update.peerDeviceId);
    if (current && validationRevision <= current.validationRevision) return;
    this.peerSessions.set(update.peerDeviceId, {
      sessionToken,
      validationRevision,
      sequences: current?.sessionToken === sessionToken ? current.sequences : new Map(),
    });
  }

  private publishAction(value: unknown, incarnation: symbol, generation: string): void {
    if (value === undefined || value === null) return;
    const action = record(value);
    if (action.kind !== "display" || typeof action.peerDeviceId !== "string" || typeof action.conversationId !== "string"
      || (action.state !== "started" && action.state !== "stopped")) return;
    const sessionToken = decimal(action.sessionToken);
    const sequence = BigInt(decimal(action.sequence));
    const deadlineMono = decimal(action.deadlineMono);
    const peer = this.peerSessions.get(action.peerDeviceId);
    if (!peer || peer.sessionToken !== sessionToken) return;
    // Typing is keyed by conversation and group participant; a 1:1 signal carries no participant (null or absent).
    const participant = typeof action.participant === "string" ? action.participant : undefined;
    const key = participant === undefined ? action.conversationId : `${action.conversationId}\n${participant}`;
    const priorSequence = peer.sequences.get(key);
    if (priorSequence !== undefined && sequence <= priorSequence) return;
    if (!this.isCurrent(incarnation, generation) || BigInt(deadlineMono) <= monotonic(this.clock().monoMs)) return;
    peer.sequences.set(key, sequence);
    if (!this.isCurrent(incarnation, generation)) return;
    this.display({
      event: "typing",
      peerDeviceId: action.peerDeviceId,
      conversationId: action.conversationId,
      ...(participant === undefined ? {} : { participant }),
      state: action.state,
      deadlineMono,
    });
  }
}

type PeerPublicationState = {
  sessionToken: string;
  validationRevision: bigint;
  sequences: Map<string, bigint>;
};

type TicketResponse = { ticket: string; expires_at_ms: number; lifetime_handle: string };
type LiveHello = { type: "hello"; protocol_version: 2; max_ttl_ms: 15_000; max_ciphertext_bytes: 5_400 };

/** Worker-private ticketed live transport. It deliberately exposes neither its ticket nor its socket to ports. */
export class LiveSupervisor {
  private sessionId: string;
  private lifetimeHandle: string | undefined;
  private socketValue: LiveSocket | undefined;
  private issueAbort: AbortController | undefined;
  private retryHandle: unknown;
  private stopped = true;
  private backoffMs = BACKOFF_MIN_MS;

  public constructor(private readonly options: LiveSupervisorOptions) {
    this.sessionId = this.newSessionId();
  }

  public start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    void this.connect();
  }

  /** Retires live state before its owning BrowserSession clears the transport token. */
  public async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    this.cancelRetry();
    this.issueAbort?.abort();
    this.issueAbort = undefined;
    const socket = this.socketValue;
    this.socketValue = undefined;
    socket?.close();
    const sessionId = this.sessionId;
    this.lifetimeHandle = undefined;
    try {
      await this.options.transport.discard(TICKET_PATH, {
        method: "DELETE",
        body: { session_id: sessionId },
        expectedSuccessStatus: 204,
      });
    } catch {
      // Lease expiry and server revalidation are the correctness fallback.
    }
  }

  public suspend(): Promise<void> { return this.stop(); }
  public lock(): Promise<void> { return this.stop(); }

  /** Frames are admitted by the core before this transmit boundary. */
  public transmit(frameJson: string): boolean {
    const socket = this.socketValue;
    if (this.stopped || !socket || socket.readyState !== WebSocket.OPEN) return false;
    socket.send(frameJson);
    return true;
  }

  private async connect(): Promise<void> {
    if (this.stopped || this.issueAbort) return;
    const controller = new AbortController();
    this.issueAbort = controller;
    let ticket: TicketResponse | undefined;
    try {
      ticket = await this.issueTicket(controller.signal);
      if (this.stopped || controller.signal.aborted) return;
      await this.open(ticket);
      this.backoffMs = BACKOFF_MIN_MS;
    } catch (error) {
      if (!this.stopped && !controller.signal.aborted) this.retry(error);
    } finally {
      ticket = undefined;
      if (this.issueAbort === controller) this.issueAbort = undefined;
    }
  }

  private async issueTicket(signal: AbortSignal): Promise<TicketResponse> {
    try {
      const response = await this.options.transport.json<TicketResponse>(TICKET_PATH, {
        method: "POST",
        body: this.lifetimeHandle === undefined
          ? { session_id: this.sessionId }
          : { session_id: this.sessionId, lifetime_handle: this.lifetimeHandle },
        signal,
        expectedSuccessStatus: 201,
      });
      if (!validTicket(response)) throw new TransportError("invalid");
      this.lifetimeHandle = response.lifetime_handle;
      return response;
    } catch (error) {
      if (isTerminalSession(error)) {
        this.sessionId = this.newSessionId();
        this.lifetimeHandle = undefined;
        return this.issueTicket(signal);
      }
      throw error;
    }
  }

  private open(ticket: TicketResponse): Promise<void> {
    const protocol = `peppy-live-v2.${this.sessionId}.${ticket.ticket}`;
    const socket = this.options.socket(liveUrl(this.options.origin), protocol);
    this.socketValue = socket;
    return new Promise((resolve, reject) => {
      let helloReceived = false;
      let refused = false;
      socket.onopen = () => {
        if (socket.protocol !== protocol) {
          socket.close();
          reject(new TransportError("invalid"));
        }
      };
      socket.onmessage = event => {
        if (refused) return;
        if (!helloReceived) {
          if (!typingCapability(event.data)) {
            refused = true;
            socket.close();
            reject(new TransportError("invalid"));
            return;
          }
          helloReceived = true;
          resolve();
          return;
        }
        if (typeof event.data === "string") this.options.onFrame?.(event.data);
      };
      socket.onerror = () => { if (!helloReceived) reject(new TransportError("offline")); };
      socket.onclose = () => {
        if (this.socketValue === socket) this.socketValue = undefined;
        if (!helloReceived) reject(new TransportError("offline"));
        else if (!this.stopped) this.retry();
      };
    });
  }

  private retry(_error?: unknown): void {
    if (this.stopped || this.retryHandle !== undefined) return;
    const delay = this.backoffMs;
    this.backoffMs = Math.min(this.backoffMs * 2, BACKOFF_MAX_MS);
    this.retryHandle = (this.options.schedule ?? ((work, wait) => setTimeout(work, wait)))(() => {
      this.retryHandle = undefined;
      void this.connect();
    }, delay);
  }

  private cancelRetry(): void {
    if (this.retryHandle === undefined) return;
    (this.options.cancel ?? clearTimeout)(this.retryHandle as ReturnType<typeof setTimeout>);
    this.retryHandle = undefined;
  }

  private newSessionId(): string {
    const bytes = new Uint8Array(SESSION_BYTES);
    const random = this.options.randomBytes ?? (value => crypto.getRandomValues(value));
    return base64url(random(bytes));
  }
}

function liveUrl(origin: string): string {
  const url = new URL(origin);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.pathname = LIVE_PATH;
  url.search = "";
  url.hash = "";
  return url.toString();
}

function validTicket(value: TicketResponse): boolean {
  return typeof value.ticket === "string" && /^[A-Za-z0-9_-]{43}$/.test(value.ticket)
    && Number.isSafeInteger(value.expires_at_ms) && value.expires_at_ms > 0
    && typeof value.lifetime_handle === "string" && /^[A-Za-z0-9_-]{11}$/.test(value.lifetime_handle);
}

function isTerminalSession(error: unknown): boolean {
  return error instanceof TransportError && error.kind === "status" && error.status === 409
    && (error.code === "live_ticket_session_terminal" || error.code === "live_ticket_lifetime_refused");
}

function typingCapability(value: unknown): value is LiveHello {
  if (typeof value !== "string") return false;
  try {
    const hello = JSON.parse(value) as Partial<LiveHello>;
    return hello.type === "hello" && hello.protocol_version === 2 && hello.max_ttl_ms === 15_000
      && hello.max_ciphertext_bytes === 5_400;
  } catch {
    return false;
  }
}

function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid");
  return value as Record<string, unknown>;
}

function decimal(value: unknown): string {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) throw new TransportError("invalid");
  return value;
}

function monotonic(value: number): bigint {
  if (!Number.isSafeInteger(value) || value < 0) throw new TransportError("invalid");
  return BigInt(value);
}
