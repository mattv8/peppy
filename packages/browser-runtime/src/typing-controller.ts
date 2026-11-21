import { CoreRejectedError } from "./core.js";
import { LiveTypingEngine, type LiveCore, type TypingDisplayEvent } from "./live.js";
import { TransportError } from "./transport.js";

export type TypingSignal = {
  conversationId: string;
  state: "started" | "stopped";
  expiresAtMs?: string;
  participant?: string;
};

type Supervisor = {
  start(): void;
  stop(): Promise<void>;
  transmit(frame: string): boolean;
};

type Options = {
  core: LiveCore;
  clock: () => { wallMs: number; monoMs: number };
  publish: (signal: TypingSignal) => void;
  onError: (error: unknown) => void;
  supervisor: (callbacks: {
    onReady: () => void;
    onClosed: () => void;
    onFrame: (frame: string) => void;
  }) => Supervisor;
};

export class TypingController {
  private readonly supervisor: Supervisor;
  private readonly engine: LiveTypingEngine;
  private readonly interests = new Map<object, string>();
  private running = false;
  private ready = false;
  private enabled = false;
  private sharing = true;
  private key: string | null = null;
  private incarnation = 0;
  private pending: Promise<void> = Promise.resolve();
  private stopping?: Promise<void>;

  public constructor(private readonly options: Options) {
    this.supervisor = options.supervisor({
      onReady: () => {
        if (!this.running) return;
        this.ready = true;
        this.background(this.refreshContext());
      },
      onClosed: () => {
        if (!this.running) return;
        this.ready = false;
        this.enabled = false;
        this.key = null;
        this.incarnation++;
        this.background(this.engine.disable());
      },
      onFrame: frame => {
        if (this.running && this.ready) this.background(this.engine.receive(frame));
      },
    });
    this.engine = new LiveTypingEngine(options.core, this.supervisor, options.clock, event => this.publish(event));
  }

  public start(): void {
    if (this.running) return;
    this.running = true;
    this.incarnation++;
    this.supervisor.start();
  }

  public stop(): Promise<void> {
    if (this.stopping) return this.stopping;
    if (!this.running) return Promise.resolve();
    this.running = false;
    this.ready = false;
    this.enabled = false;
    this.key = null;
    this.incarnation++;
    this.interests.clear();
    const stopped = this.supervisor.stop();
    const disabled = this.engine.disable().catch(() => {});
    this.stopping = Promise.all([stopped, disabled]).then(() => {}).finally(() => { this.stopping = undefined; });
    return this.stopping;
  }

  public refreshContext(): Promise<void> {
    const incarnation = this.incarnation;
    const refresh = this.pending.then(async () => {
      if (!this.current(incarnation)) return;
      try {
        const result = await this.options.core.invoke("_worker_typing_context_key", {});
        if (!this.current(incarnation)) return;
        if (typeof result !== "object" || result === null || !("key" in result)
          || (typeof result.key !== "string" && result.key !== null)) throw new TransportError("invalid");
        const key = result.key;
        if (key === this.key) return;
        if (this.enabled) {
          this.enabled = false;
          await this.engine.disable();
          if (!this.current(incarnation)) return;
        }
        this.key = key;
        if (key === null) return;
        await this.engine.enable();
        if (!this.current(incarnation)) return;
        this.enabled = true;
        await this.engine.setSharing(this.sharing);
      } catch (error) {
        if (!(error instanceof CoreRejectedError)) throw error;
        this.enabled = false;
        this.key = null;
      }
    });
    this.pending = refresh.catch(() => {});
    return refresh;
  }

  public async setSharing(enabled: boolean): Promise<void> {
    this.sharing = enabled;
    if (!this.running || !this.ready || !this.enabled) return;
    try {
      await this.engine.setSharing(enabled);
    } catch (error) {
      if (!(error instanceof CoreRejectedError)) throw error;
    }
  }

  public async send(port: object, conversationId: string, state: "started" | "stopped"): Promise<void> {
    if (!this.running) return;
    const previous = this.interests.get(port);
    if (state === "started") {
      this.interests.set(port, conversationId);
      if (previous && previous !== conversationId) await this.stopInterest(previous);
      if (this.interests.get(port) !== conversationId) return;
      await this.sendFrame(conversationId, state);
    } else {
      if (previous === conversationId) this.interests.delete(port);
      await this.stopInterest(conversationId);
    }
  }

  public async disconnect(port: object): Promise<void> {
    const previous = this.interests.get(port);
    this.interests.delete(port);
    if (previous) await this.stopInterest(previous);
  }

  private async stopInterest(conversationId: string): Promise<void> {
    if (![...this.interests.values()].includes(conversationId)) await this.sendFrame(conversationId, "stopped");
  }

  private async sendFrame(conversationId: string, state: "started" | "stopped"): Promise<void> {
    if (!this.running || !this.ready || !this.enabled) return;
    try {
      await this.engine.send(conversationId, state);
    } catch (error) {
      if (!(error instanceof CoreRejectedError)) throw error;
    }
  }

  private current(incarnation: number): boolean {
    return this.running && this.ready && this.incarnation === incarnation;
  }

  private background(work: Promise<void>): void {
    void work.catch(error => {
      if (!(error instanceof CoreRejectedError)) this.options.onError(error);
    });
  }

  private publish(event: TypingDisplayEvent): void {
    if (!this.running || !this.ready) return;
    const clock = this.options.clock();
    this.options.publish({
      conversationId: event.conversationId,
      state: event.state,
      ...(event.state === "started" ? { expiresAtMs: String(clock.wallMs + Number(event.deadlineMono) - clock.monoMs) } : {}),
      ...(event.participant === undefined ? {} : { participant: event.participant }),
    });
  }
}
