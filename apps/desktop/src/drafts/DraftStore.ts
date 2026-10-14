import type { Draft, DraftInput } from "../bridge";
import { errorCode, errorDetail, errorText } from "../bridgeErrors";

export type DraftContent = Pick<
  Draft,
  "text" | "recipientIds" | "attachmentIds" | "gatewayId" | "simId"
>;

/** Local key for a new-recipient draft that has no native draft/conversation ID yet. */
const NEW_DRAFT_PREFIX = "new:";
export const isLocalDraftKey = (key: string) => key.startsWith(NEW_DRAFT_PREFIX);

export type DraftSlot = {
  /** Conversation ID once known, otherwise a `new:` local key. */
  key: string;
  /** Native identity; empty strings ask the host to create the draft (and conversation). */
  id: string;
  conversationId: string;
  /** Last revision acknowledged by the host; used as the CAS `expectedRevision`. */
  revision: string;
  content: DraftContent;
  /** Local edit counter, and the newest edit the host has acknowledged. */
  generation: number;
  savedGeneration: number;
  error?: { message: string; code?: string };
  /** While a send is in flight, saves are deferred so they cannot race the atomic send/clear. */
  held: boolean;
  queue: Promise<unknown>;
};

export type SaveOutcome = { ok: true } | { ok: false; error: string };

export const blankContent = (): DraftContent => ({
  text: "",
  recipientIds: [],
  attachmentIds: [],
});
const contentOf = (draft: Draft): DraftContent => ({
  text: draft.text,
  recipientIds: draft.recipientIds,
  attachmentIds: draft.attachmentIds,
  gatewayId: draft.gatewayId,
  simId: draft.simId,
});
export const isDirty = (slot: DraftSlot) => slot.generation > slot.savedGeneration;
const sameDraftContent = (left: DraftContent, right: DraftContent) =>
  left.text === right.text &&
  left.gatewayId === right.gatewayId &&
  left.simId === right.simId &&
  left.recipientIds.join("\u0000") === right.recipientIds.join("\u0000") &&
  left.attachmentIds.join("\u0000") === right.attachmentIds.join("\u0000");
export const shouldConfirmPostFlushDelete = (
  requestedKey: string,
  currentKey: string,
  currentContent: DraftContent,
  hadContent: boolean,
  confirmedKey: string | null,
) =>
  Boolean(currentContent.text || currentContent.attachmentIds.length) &&
  (!hadContent || currentKey !== requestedKey) &&
  confirmedKey !== currentKey;

type DraftStoreEvents = {
  changed(): void;
  rekeyed(from: string, to: string): void;
  unavailable?(from: string, to?: string, message?: string): void;
};

export type DraftRecovery = {
  id: string;
  content: DraftContent;
  destination?: string;
  message: string;
};

/**
 * Per-conversation draft state with serialized CAS saves. A failed save keeps the local edits
 * dirty with an error; callers that must not lose content (send, close) flush and check the
 * outcome. Host snapshots never overwrite dirty or held drafts.
 */
export class DraftStore {
  private slots = new Map<string, DraftSlot>();
  private aliases = new Map<string, string>();
  private tombstones = new Set<string>();
  private recoveries = new Map<string, DraftRecovery>();
  private newCount = 0;

  constructor(
    private readonly persist: (input: DraftInput) => Promise<Draft>,
    private readonly events: DraftStoreEvents,
  ) {}

  /** Follows `new:` keys to the conversation ID the host assigned. */
  resolve(key: string): string {
    let current = key;
    for (
      let next = this.aliases.get(current);
      next !== undefined;
      next = this.aliases.get(current)
    )
      current = next;
    return current;
  }

  get(key: string): DraftSlot | undefined {
    return this.slots.get(this.resolve(key));
  }

  localDrafts(): DraftSlot[] {
    return [...this.slots.values()].filter((slot) => isLocalDraftKey(slot.key));
  }

  recoveriesFor(conversationId?: string): DraftRecovery[] {
    return [...this.recoveries.values()].filter(
      recovery => conversationId
        ? !recovery.destination || recovery.destination === conversationId || recovery.id === conversationId
        : !recovery.destination,
    );
  }

  discardRecovery(id: string) {
    this.recoveries.delete(id);
    this.events.changed();
  }

  /** Retires content whose host identity moved outside this window's save queue. */
  retireUnavailable(key: string, error: unknown): string | undefined {
    const slot = this.get(key);
    const destination = errorDetail(error, "conversationId");
    if (!slot) return destination;
    const moved = errorCode(error) === "draft-moved";
    this.retire(
      slot,
      slot.content,
      destination,
      moved
        ? "Draft moved elsewhere. Review the retained text before sending again."
        : "Draft was deleted elsewhere. Retained text can be copied or restored to a new draft.",
    );
    return destination;
  }

  /** Starts a draft; the host assigns both IDs on save. */
  createNew(recipient?: string): { key: string; saved: Promise<SaveOutcome> } {
    const key = `${NEW_DRAFT_PREFIX}${++this.newCount}`;
    this.ensure(key);
    return { key, saved: this.edit(key, { recipientIds: recipient ? [recipient] : [] }) };
  }

  edit(key: string, patch: Partial<DraftContent>): Promise<SaveOutcome> {
    const resolved = this.resolve(key);
    if (this.tombstones.has(key) || this.tombstones.has(resolved))
      return Promise.resolve({ ok: false, error: "This draft was deleted or moved elsewhere." });
    const slot = this.ensure(key);
    slot.content = { ...slot.content, ...patch };
    slot.generation += 1;
    this.events.changed();
    return this.enqueue(slot);
  }

  /** Applies the host's view of a conversation's draft unless local edits are pending. */
  adopt(conversationId: string, draft: Draft | undefined) {
    const slot = this.get(conversationId);
    if (slot && (isDirty(slot) || slot.held)) return;
    if (draft) {
      this.tombstones.delete(conversationId);
      const target = slot ?? this.ensure(conversationId);
      Object.assign(target, {
        id: draft.id,
        conversationId: draft.conversationId,
        revision: draft.revision,
        content: contentOf(draft),
        error: undefined,
      });
    } else if (slot) {
      this.tombstones.delete(conversationId);
      Object.assign(slot, {
        id: "",
        revision: "0",
        content: blankContent(),
        error: undefined,
      });
    } else return;
    this.events.changed();
  }

  /** Waits for queued saves and retries once if edits are still unacknowledged. */
  async flush(key: string): Promise<SaveOutcome> {
    const slot = this.get(key);
    if (!slot) return { ok: true };
    await slot.queue;
    return isDirty(slot) ? this.enqueue(slot) : { ok: true };
  }

  /** Flush all live draft slots and report the first failure after every attempt. */
  async flushAll(): Promise<SaveOutcome> {
    let firstFailure: SaveOutcome | undefined;
    for (const key of [...this.slots.keys()]) {
      const outcome = await this.flush(key);
      if (!outcome.ok && !firstFailure) firstFailure = outcome;
    }
    return firstFailure ?? { ok: true };
  }

  /** After a stale-revision conflict the person may keep this window's text over the stored one. */
  rebase(key: string, stored: Draft | undefined) {
    const slot = this.get(key);
    if (!slot) return;
    if (stored)
      Object.assign(slot, { id: stored.id, revision: stored.revision });
    else Object.assign(slot, { id: "", revision: "0" });
  }

  hold(key: string) {
    const slot = this.get(key);
    if (slot) slot.held = true;
  }

  release(key: string) {
    const slot = this.get(key);
    if (!slot) return;
    slot.held = false;
    if (isDirty(slot)) void this.enqueue(slot);
  }

  /** A discarded draft is removed only after its serialized saves have completed. */
  remove(key: string) {
    const resolved = this.resolve(key);
    this.tombstones.add(resolved);
    this.slots.delete(resolved);
    this.events.changed();
  }

  /** The host cleared the sent draft; edits typed during the send start a fresh draft. */
  markSent(key: string, sentGeneration: number) {
    const slot = this.get(key);
    if (!slot) return;
    Object.assign(slot, {
      id: "",
      revision: "0",
      held: false,
      error: undefined,
    });
    if (slot.generation > sentGeneration) {
      void this.enqueue(slot);
    } else {
      slot.content = blankContent();
      slot.savedGeneration = slot.generation;
    }
    this.events.changed();
  }

  private ensure(key: string): DraftSlot {
    const resolved = this.resolve(key);
    let slot = this.slots.get(resolved);
    if (!slot) {
      slot = {
        key: resolved,
        id: "",
        conversationId: isLocalDraftKey(resolved) ? "" : resolved,
        revision: "0",
        content: blankContent(),
        generation: 0,
        savedGeneration: 0,
        held: false,
        queue: Promise.resolve(),
      };
      this.slots.set(resolved, slot);
    }
    return slot;
  }

  private enqueue(slot: DraftSlot): Promise<SaveOutcome> {
    const run = slot.queue.then(() => this.save(slot));
    slot.queue = run;
    return run;
  }

  private async save(slot: DraftSlot): Promise<SaveOutcome> {
    if (this.tombstones.has(slot.key) || this.slots.get(slot.key) !== slot)
      return { ok: false, error: "This draft was deleted or moved elsewhere." };
    if (slot.held)
      return {
        ok: false,
        error:
          "A send is in progress; your edits are kept and saved afterwards.",
      };
    if (!isDirty(slot))
      return slot.error
        ? { ok: false, error: slot.error.message }
        : { ok: true };
    const generation = slot.generation;
    const input: DraftInput = {
      id: slot.id,
      conversationId: slot.conversationId,
      ...slot.content,
      expectedRevision: slot.revision,
    };
    let saved: Draft;
    try {
      saved = await this.persist(input);
    } catch (error) {
      const code = errorCode(error);
      if (code === "draft-moved" || code === "not-found") {
        this.retireUnavailable(slot.key, error);
        return { ok: true };
      }
      slot.error = { message: errorText(error), code };
      this.events.changed();
      return { ok: false, error: slot.error.message };
    }
    Object.assign(slot, {
      id: saved.id,
      revision: saved.revision,
      savedGeneration: Math.max(slot.savedGeneration, generation),
      error: undefined,
    });
    if (slot.generation === generation) slot.content = contentOf(saved);
    if (saved.conversationId && saved.conversationId !== slot.conversationId) {
      if (slot.generation > generation && !sameDraftContent(contentOf(saved), input)) {
        this.retire(
          slot,
          slot.content,
          saved.conversationId,
          "Draft moved while you were editing. Your newer text is retained separately; review it before sending again.",
        );
        return { ok: false, error: "Draft moved while you were editing. Your newer text is retained separately." };
      }
      if (!this.rekey(slot, saved.conversationId)) return { ok: false, error: "Draft moved while another window has edits. Your text is retained separately." };
    }
    this.events.changed();
    return { ok: true };
  }

  private rekey(slot: DraftSlot, conversationId: string): boolean {
    const from = slot.key;
    slot.conversationId = conversationId;
    if (from === conversationId) return true;
    const destination = this.slots.get(conversationId);
    if (destination && destination !== slot && isDirty(destination)) {
      this.retire(
        slot,
        slot.content,
        conversationId,
        "This draft moved while the destination has newer edits. Your text is retained separately.",
      );
      return false;
    }
    this.slots.delete(from);
    if (isLocalDraftKey(from)) this.aliases.set(from, conversationId);
    slot.key = conversationId;
    this.slots.set(conversationId, slot);
    this.events.rekeyed(from, conversationId);
    return true;
  }

  private retire(
    slot: DraftSlot,
    content: DraftContent,
    destination: string | undefined,
    message: string,
  ) {
    const from = slot.key;
    if (content.text || content.attachmentIds.length || content.recipientIds.length)
      this.recoveries.set(from, { id: from, content: { ...content }, destination, message });
    this.slots.delete(from);
    if (destination && isLocalDraftKey(from)) this.aliases.set(from, destination);
    this.events.unavailable?.(from, destination, message);
    this.events.changed();
  }
}

/* ------------------------------------------------------------------ presentational pieces */
