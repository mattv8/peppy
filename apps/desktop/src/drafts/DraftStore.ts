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
  /** Last host version successfully acknowledged for safe stale-save reconciliation. */
  acknowledgedContent: DraftContent;
};

/** A failed outcome from `flushAll` carries the resolved key of the draft that failed, which may be offscreen. */
export type SaveOutcome = { ok: true } | { ok: false; error: string; reviewRequired?: true; key?: string };
export type DraftSaveState = "idle" | "saving" | "failed";
export type DraftNoticeEvent = "remote-update" | "replaced" | "moved" | "save-failed" | "saved";

type UndoVersion = {
  content: DraftContent;
  generation: number;
  reviewedId: string;
  reviewedRevision: string;
  reviewedContent: DraftContent;
};

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
  left.text === right.text && sameStructure(left, right);
/** Authored text and attachments only; host recipient formatting is not a local edit. */
const sameAuthoredContent = (left: DraftContent, right: DraftContent) =>
  left.text === right.text && sameAttachments(left, right);
const sameAttachments = (left: DraftContent, right: DraftContent) =>
  left.attachmentIds.join("\u0000") === right.attachmentIds.join("\u0000");
const sameStructure = (left: DraftContent, right: DraftContent) =>
  left.gatewayId === right.gatewayId &&
  left.simId === right.simId &&
  sameRecipients(left, right) &&
  sameAttachments(left, right);
const sameRecipients = (left: DraftContent, right: DraftContent) =>
  left.recipientIds.join("\u0000") === right.recipientIds.join("\u0000");
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

export type DraftStoreEvents = {
  changed(): void;
  rekeyed(from: string, to: string): void;
  notice?(key: string, event: DraftNoticeEvent): void;
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
  private retiredDraftIds = new Map<string, { conversationId: string; revision: string }>();
  private hydrated = new Set<string>();
  private bufferedAuthoritative = new Map<string, Draft>();
  private undoVersions = new Map<string, UndoVersion>();
  /** Copy-only local content displaced by a remote structural change or move. */
  private retainedVersions = new Map<string, DraftContent[]>();
  private reviewRequired = new Set<string>();
  private pendingSentVerification = new Map<string, number>();
  private recoverySaves = new Map<string, Promise<SaveOutcome>>();
  private saveStates = new Map<string, DraftSaveState>();
  private newCount = 0;
  private sessionGeneration = 0;

  constructor(
    private readonly persist: (input: DraftInput) => Promise<Draft>,
    private readonly events: DraftStoreEvents,
    private readonly loadDraft: (conversationId: string) => Promise<Draft | undefined> = async () => undefined,
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

  isHydrated(key: string): boolean {
    const slot = this.get(key);
    return Boolean(slot && (isLocalDraftKey(slot.key) || this.hydrated.has(slot.conversationId)));
  }

  saveState(key: string): DraftSaveState {
    return this.saveStates.get(this.resolve(key)) ?? "idle";
  }

  clearUndo(key: string): void {
    const resolved = this.resolve(key);
    this.undoVersions.delete(resolved);
    this.retainedVersions.delete(resolved);
  }

  acknowledgeReview(key: string): void {
    this.reviewRequired.delete(this.resolve(key));
  }

  /**
   * Settles a stale send for review. The caller has already loaded `host`; this method never loads,
   * sends, or writes. The supplied copies are kept copy-only and undo is dropped. A present host is
   * adopted for review; an absent host keeps the local slot, fails as not-found, and is not recreated.
   * Failures carry the resolved key so the caller can point at the affected draft.
   */
  reconcileForReview(key: string, host: Draft | undefined, retained: DraftContent[]): SaveOutcome {
    const resolved = this.resolve(key);
    for (const content of retained) this.retainCopy(resolved, content);
    this.undoVersions.delete(resolved);
    this.pendingSentVerification.delete(resolved);
    const slot = this.slots.get(resolved) ?? (host && !this.tombstones.has(resolved) ? this.ensure(resolved) : undefined);
    if (!slot || this.tombstones.has(resolved)) {
      this.events.changed();
      return { ok: false, error: "Draft no longer exists.", key: resolved };
    }
    slot.held = false;
    if (!host) {
      slot.error = { message: "Draft no longer exists.", code: "not-found" };
      this.reviewRequired.add(slot.key);
      this.saveStates.set(slot.key, "failed");
      this.events.notice?.(slot.key, "save-failed");
      this.events.changed();
      return { ok: false, error: "Draft no longer exists.", key: slot.key };
    }
    const event = host.conversationId === slot.conversationId ? "remote-update" : "moved";
    if (this.adoptHost(slot, host, event)) {
      this.events.changed();
      const outcome = this.reviewOutcome(slot);
      return outcome.ok ? outcome : { ...outcome, key: slot.key };
    }
    const message = "Draft was moved to a conversation with unsaved edits. Your text is kept for copying.";
    slot.error = { message, code: "moved" };
    this.reviewRequired.add(slot.key);
    this.saveStates.set(slot.key, "failed");
    this.events.changed();
    return { ok: false, error: message, key: slot.key };
  }

  hasUndo(key: string): boolean {
    const slot = this.get(key);
    const undo = this.undoVersions.get(this.resolve(key));
    return Boolean(slot && undo && undo.generation === slot.generation && sameRecipients(slot.content, undo.content));
  }

  retainedText(key: string): string | undefined {
    return this.retainedTexts(key).join("\n\n--- retained draft ---\n\n") || this.undoVersions.get(this.resolve(key))?.content.text;
  }

  retainedTexts(key: string): string[] {
    return (this.retainedVersions.get(this.resolve(key)) ?? []).map(version => version.text);
  }

  reset(): void {
    this.sessionGeneration += 1;
    this.slots.clear();
    this.aliases.clear();
    this.tombstones.clear();
    this.retiredDraftIds.clear();
    this.hydrated.clear();
    this.bufferedAuthoritative.clear();
    this.undoVersions.clear();
    this.retainedVersions.clear();
    this.pendingSentVerification.clear();
    this.recoverySaves.clear();
    this.reviewRequired.clear();
    this.saveStates.clear();
    this.events.changed();
  }

  retry(key: string): Promise<SaveOutcome> {
    const slot = this.get(key);
    if (!slot) return Promise.resolve({ ok: false, error: "This draft was deleted or moved elsewhere." });
    if (slot.error?.code === "not-found") return Promise.resolve({ ok: false, error: slot.error.message });
    slot.error = undefined;
    if (this.pendingSentVerification.has(slot.key)) return this.verifySentRevision(slot, this.pendingSentVerification.get(slot.key)!);
    if (!isDirty(slot)) {
      this.saveStates.set(slot.key, "idle");
      this.events.changed();
      return Promise.resolve({ ok: true });
    }
    return this.enqueue(slot);
  }

  /** Explicitly creates a replacement for a draft the host has deleted, preserving local content. */
  saveAsNew(key: string): Promise<SaveOutcome> {
    const slot = this.get(key);
    if (!slot) return Promise.resolve({ ok: false, error: "This draft was deleted or moved elsewhere." });
    const pending = this.recoverySaves.get(slot.key);
    if (pending) return pending;
    if (slot.error?.code !== "not-found") return Promise.resolve({ ok: false, error: slot.error?.message ?? "Draft recovery is unavailable." });

    const recoveryKey = slot.key;
    const session = this.sessionGeneration;
    const recovery = slot.queue.catch(() => undefined).then(() => this.saveAsNewRecovery(slot, session));
    this.recoverySaves.set(recoveryKey, recovery);
    slot.queue = recovery.catch(() => undefined);
    void recovery.finally(() => {
      if (this.recoverySaves.get(recoveryKey) === recovery) this.recoverySaves.delete(recoveryKey);
    });
    return recovery;
  }

  /** Explicitly removes only this window's unrecoverable local state; it never writes to the host. */
  discardLocalChanges(key: string): boolean {
    const slot = this.get(key);
    if (!slot || slot.error?.code !== "not-found" || this.recoverySaves.has(slot.key)) return false;
    if (slot.id) this.retireIdentity(slot);
    this.slots.delete(slot.key);
    this.hydrated.delete(slot.conversationId);
    this.undoVersions.delete(slot.key);
    this.retainedVersions.delete(slot.key);
    this.reviewRequired.delete(slot.key);
    this.pendingSentVerification.delete(slot.key);
    this.saveStates.delete(slot.key);
    this.recoverySaves.delete(slot.key);
    this.events.changed();
    return true;
  }

  async undo(key: string): Promise<SaveOutcome> {
    const slot = this.get(key);
    const resolved = this.resolve(key);
    const undo = this.undoVersions.get(resolved);
    if (!slot || !undo) return { ok: false, error: "No retained draft text is available." };
    if (!sameRecipients(slot.content, undo.content))
      return { ok: false, error: "Draft recipients changed; local text is kept for copying." };
    const generation = slot.generation;
    const session = this.sessionGeneration;
    const slotKey = slot.key;
    let current: Draft | undefined;
    try {
      current = await this.loadDraft(slot.conversationId);
    } catch (error) {
      if (!this.isCurrent(slot, slotKey, session)) return { ok: true };
      return { ok: false, error: errorText(error) };
    }
    if (!this.isCurrent(slot, slotKey, session)) return { ok: true };
    if (
      !current ||
      current.id !== undo.reviewedId ||
      current.revision !== undo.reviewedRevision ||
      !sameDraftContent(contentOf(current), undo.reviewedContent) ||
      slot.generation !== generation
    )
      return { ok: false, error: "Draft structure changed; local text is kept for review." };
    slot.content = { ...slot.content, text: undo.content.text };
    slot.generation += 1;
    this.undoVersions.delete(slot.key);
    this.events.changed();
    return this.enqueue(slot);
  }

  localDrafts(): DraftSlot[] {
    return [...this.slots.values()].filter((slot) => isLocalDraftKey(slot.key));
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
    this.undoVersions.delete(slot.key);
    slot.content = { ...slot.content, ...patch };
    slot.generation += 1;
    this.events.changed();
    if (slot.error?.code === "not-found") return Promise.resolve({ ok: false, error: slot.error.message });
    this.reviewRequired.delete(slot.key);
    return this.enqueue(slot);
  }

  /** Applies a snapshot and records that this conversation's initial draft lookup completed. */
  adopt(conversationId: string, draft: Draft | undefined) {
    if (this.tombstones.has(conversationId)) return;
    const firstHydration = !this.hydrated.has(conversationId);
    this.hydrated.add(conversationId);
    const slot = this.get(conversationId) ?? this.ensure(conversationId);
    const buffered = this.bufferedAuthoritative.get(conversationId);
    if (buffered && draft && buffered.id === draft.id && this.shouldAdopt(draft, buffered)) draft = buffered;
    if (buffered && (!draft || buffered.id !== draft.id)) void this.verifyBufferedIdentity(conversationId, draft);
    this.bufferedAuthoritative.delete(conversationId);
    if (draft && this.isRetiredInConversation(draft)) return;
    if (slot && (isDirty(slot) || slot.held)) {
      if (!firstHydration) return;
      if (draft && this.hasRecoverableContent(draft) && !sameDraftContent(slot.content, contentOf(draft))) {
        this.retainCopy(slot.key, contentOf(draft));
        this.reviewRequired.add(slot.key);
        slot.error = { message: "A stored draft conflicts with local edits. Review both versions before saving.", code: "stale-draft" };
        this.events.changed();
        return;
      }
      if (draft && !slot.id) {
        this.installIdentity(slot, draft);
        slot.acknowledgedContent = contentOf(draft);
      }
      if (!slot.held && !slot.error) void this.enqueue(slot);
      return;
    }
    if (draft) {
      const target = slot;
      if (!this.shouldAdopt(target, draft)) return;
      Object.assign(target, {
        id: draft.id,
        conversationId: draft.conversationId,
        revision: draft.revision,
        content: contentOf(draft),
        error: undefined,
        acknowledgedContent: contentOf(draft),
      });
    } else {
      Object.assign(slot, {
        id: "",
        revision: "0",
        content: blankContent(),
        error: undefined,
        acknowledgedContent: blankContent(),
      });
    }
    this.events.changed();
  }

  /** Applies a saved-draft notification without allowing an older revision to regress local state. */
  applyAuthoritative(draft: Draft): void {
    if (this.tombstones.has(draft.conversationId) || this.isRetiredInConversation(draft)) return;
    if (!this.hydrated.has(draft.conversationId)) {
      const buffered = this.bufferedAuthoritative.get(draft.conversationId);
      if (!buffered || this.shouldAdopt(buffered, draft))
        this.bufferedAuthoritative.set(draft.conversationId, draft);
      return;
    }
    const slot = this.get(draft.conversationId);
    if (!slot || isDirty(slot) || slot.held || !this.shouldAdopt(slot, draft)) return;
    const incomingContent = contentOf(draft);
    const changed = !sameDraftContent(slot.content, incomingContent);
    const textOnlyChange = changed && slot.id === draft.id && sameStructure(slot.content, incomingContent);
    if (textOnlyChange) this.preserveUndo(slot, slot.content, draft);
    else if (changed || slot.id !== draft.id) {
      this.retainCopy(slot.key, slot.content);
      this.undoVersions.delete(slot.key);
    }
    Object.assign(slot, {
      id: draft.id,
      conversationId: draft.conversationId,
      revision: draft.revision,
      content: incomingContent,
      error: undefined,
      acknowledgedContent: incomingContent,
    });
    if (changed) this.events.notice?.(slot.key, "remote-update");
    this.events.changed();
  }

  /**
   * Waits for queued saves. Unsaved or failed content blocks as a non-review failure; only a clean,
   * durably adopted host state reports review-required.
   */
  async flush(key: string): Promise<SaveOutcome> {
    const slot = this.get(key);
    if (!slot) return { ok: true };
    await slot.queue;
    if (this.reviewRequired.has(slot.key)) {
      if (isDirty(slot) || this.saveStates.get(slot.key) === "failed") return this.unsavedOutcome(slot);
      return this.reviewOutcome(slot);
    }
    return isDirty(slot) ? this.enqueue(slot) : { ok: true };
  }

  /** Flush all live draft slots and report the first failure after every attempt, keyed to its draft. */
  async flushAll(): Promise<SaveOutcome> {
    let review: SaveOutcome | undefined;
    let failure: SaveOutcome | undefined;
    for (const slot of [...this.slots.values()]) {
      const outcome = await this.flush(slot.key);
      if (outcome.ok) continue;
      const attributed: SaveOutcome = { ...outcome, key: slot.key };
      if (outcome.reviewRequired) review ??= attributed;
      else failure ??= attributed;
    }
    return failure ?? review ?? { ok: true };
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
    const slot = this.slots.get(resolved);
    if (slot?.id) this.retireIdentity(slot);
    this.tombstones.add(resolved);
    this.slots.delete(resolved);
    this.events.changed();
  }

  /** The host cleared the sent draft; edits typed during the send stay as fresh content on the same draft id. */
  markSent(key: string, sentGeneration: number, revision?: string, postSendContent?: Partial<DraftContent>) {
    const slot = this.get(key);
    if (!slot) return;
    if (!revision) {
      slot.held = true;
      const cleared = { ...slot.content, text: "", attachmentIds: [] };
      slot.content = postSendContent ? { ...cleared, ...postSendContent } : cleared;
      if (postSendContent && slot.generation <= sentGeneration) slot.generation = sentGeneration + 1;
      slot.acknowledgedContent = cleared;
      slot.savedGeneration = sentGeneration;
      this.pendingSentVerification.set(slot.key, sentGeneration);
      void this.verifySentRevision(slot, sentGeneration);
      this.events.changed();
      return;
    }
    Object.assign(slot, {
      revision,
      held: false,
      error: undefined,
    });
    slot.acknowledgedContent = { ...slot.content, text: "", attachmentIds: [] };
    if (slot.generation > sentGeneration) {
      if (postSendContent) slot.content = { ...slot.acknowledgedContent, ...postSendContent };
      void this.enqueue(slot);
    } else {
      slot.content = { ...slot.content, text: "", attachmentIds: [] };
      slot.savedGeneration = slot.generation;
      slot.acknowledgedContent = slot.content;
    }
    this.events.changed();
  }

  private async verifySentRevision(slot: DraftSlot, sentGeneration: number): Promise<SaveOutcome> {
    const session = this.sessionGeneration;
    const key = slot.key;
    let current: Draft | undefined;
    try {
      current = await this.loadDraft(slot.conversationId);
    } catch (error) {
      if (!this.isCurrent(slot, key, session)) return { ok: true };
      slot.held = false;
      this.failSave(slot, errorText(error), errorCode(error));
      return { ok: false, error: errorText(error) };
    }
    if (!this.isCurrent(slot, key, session)) return { ok: true };
    if (!current) {
      this.pendingSentVerification.delete(slot.key);
      slot.held = false;
      this.retainCopy(slot.key, slot.content);
      this.reviewRequired.add(slot.key);
      return this.failSave(slot, "Sent draft could not be verified before saving new edits.", "not-found");
    }
    if (current.id !== slot.id) {
      this.pendingSentVerification.delete(slot.key);
      slot.held = false;
      this.retainCopy(slot.key, slot.content);
      this.undoVersions.delete(slot.key);
      this.adoptHost(slot, current, "remote-update");
      this.events.changed();
      return this.reviewOutcome(slot);
    }
    const hostContent = contentOf(current);
    const acknowledgedBeforeSendVerification = slot.acknowledgedContent;
    const hasPostSendEdits = slot.generation > sentGeneration;
    this.pendingSentVerification.delete(slot.key);
    slot.revision = current.revision;
    slot.held = false;
    slot.error = undefined;
    this.saveStates.set(slot.key, "idle");
    slot.acknowledgedContent = hostContent;
    if (!hasPostSendEdits) {
      slot.content = hostContent;
      slot.savedGeneration = slot.generation;
    } else if (!sameDraftContent(hostContent, acknowledgedBeforeSendVerification)) {
      this.retainCopy(slot.key, hostContent);
    }
    if (isDirty(slot)) return this.enqueue(slot);
    this.events.changed();
    return { ok: true };
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
        acknowledgedContent: blankContent(),
      };
      this.slots.set(resolved, slot);
    }
    return slot;
  }

  private enqueue(slot: DraftSlot): Promise<SaveOutcome> {
    const session = this.sessionGeneration;
    const run = slot.queue.catch(() => undefined).then(() => this.save(slot, session));
    slot.queue = run.catch(() => undefined);
    return run;
  }

  private async save(slot: DraftSlot, session = this.sessionGeneration): Promise<SaveOutcome> {
    const key = slot.key;
    if (!this.isCurrent(slot, key, session)) return { ok: true };
    if (this.isUnavailable(slot))
      return { ok: false, error: "This draft was deleted or moved elsewhere." };
    if (slot.held)
      return {
        ok: false,
        error:
          "A send is in progress; your edits are kept and saved afterwards.",
      };
    if (this.pendingSentVerification.has(slot.key))
      return {
        ok: false,
        error: "Sent draft must be verified before saving new edits.",
      };
    if (!isLocalDraftKey(slot.key) && !this.hydrated.has(slot.conversationId))
      return { ok: false, error: "Draft is still loading. Retry once it is available." };
    if (!isDirty(slot))
      return slot.error
        ? { ok: false, error: slot.error.message }
        : { ok: true };
    this.saveStates.set(slot.key, "saving");
    const generation = slot.generation;
    let saved: Draft | undefined;
    let submittedContent: DraftContent | undefined;
    for (let attempts = 0; attempts < 3; attempts += 1) {
      const input: DraftInput = {
        id: slot.id,
        conversationId: slot.conversationId,
        ...slot.content,
        expectedRevision: slot.revision,
      };
      submittedContent = { ...slot.content };
      try {
        saved = await this.persist(input);
        if (!this.isCurrent(slot, key, session)) return { ok: true };
        break;
      } catch (error) {
        if (!this.isCurrent(slot, key, session)) return { ok: true };
        if (this.isUnavailable(slot) || slot.held)
          return this.failSave(slot, "This draft changed while saving. Retry after it is available.");
        const code = errorCode(error);
        if (code === "stale-draft") {
          const caughtUp = await this.catchUpStaleSave(slot, session);
          if (session !== this.sessionGeneration) return { ok: true };
          if (caughtUp) continue;
          return slot.error ? { ok: false, error: slot.error.message } : this.reviewOutcome(slot);
        }
        if (code === "draft-moved" || code === "not-found") {
          const adopted = await this.adoptMovedOrMissing(slot, error, session);
          if (session !== this.sessionGeneration) return { ok: true };
          return adopted ? this.reviewOutcome(slot) : this.failSave(slot, errorText(error), code);
        }
        return this.failSave(slot, errorText(error), code);
      }
    }
    if (!saved) return this.failSave(slot, "Draft save could not be reconciled after 3 attempts.", "stale-draft");
    if (this.isUnavailable(slot)) return { ok: false, error: "This draft was deleted or moved elsewhere." };
    if (saved.conversationId && saved.conversationId !== slot.conversationId) {
      if (!slot.id && !slot.conversationId) {
        if (submittedContent && sameAuthoredContent(contentOf(saved), submittedContent) && this.rekey(slot, saved.conversationId)) {
          this.hydrated.add(saved.conversationId);
        } else {
          this.retainCopy(slot.key, slot.content);
          this.adoptHost(slot, saved, "moved");
          return this.reviewOutcome(slot);
        }
      } else {
        this.retainCopy(slot.key, slot.content);
        this.undoVersions.delete(slot.key);
        this.adoptHost(slot, saved, "moved");
        return this.reviewOutcome(slot);
      }
    }
    this.hydrated.add(saved.conversationId || slot.conversationId);
    Object.assign(slot, {
      id: saved.id,
      revision: saved.revision,
      savedGeneration: Math.max(slot.savedGeneration, generation),
      error: undefined,
      acknowledgedContent: contentOf(saved),
    });
    this.fenceUndoAfterSave(slot, generation, saved);
    if (slot.held) {
      this.saveStates.set(slot.key, "idle");
      return { ok: false, error: "A send is in progress; your edits are kept and saved afterwards." };
    }
    if (slot.generation === generation) slot.content = contentOf(saved);
    this.events.changed();
    this.saveStates.set(slot.key, "idle");
    this.events.notice?.(slot.key, "saved");
    return { ok: true };
  }

  private async saveAsNewRecovery(slot: DraftSlot, session: number): Promise<SaveOutcome> {
    const key = slot.key;
    if (!this.isCurrent(slot, key, session)) return { ok: true };
    if (slot.error?.code !== "not-found") return { ok: true };
    if (slot.held) return { ok: false, error: "A send is in progress; your edits are kept and saved afterwards." };
    const generation = slot.generation;
    const submittedContent = { ...slot.content };
    this.saveStates.set(key, "saving");
    this.events.changed();
    let saved: Draft;
    try {
      saved = await this.persist({
        id: "",
        conversationId: slot.conversationId,
        ...submittedContent,
        expectedRevision: "0",
      });
    } catch (error) {
      if (!this.isCurrent(slot, key, session)) return { ok: true };
      if (errorCode(error) !== "not-found") return this.failSave(slot, errorText(error), "not-found");
      try {
        saved = await this.persist({
          id: "",
          conversationId: "",
          ...submittedContent,
          expectedRevision: "0",
        });
      } catch (fallbackError) {
        if (!this.isCurrent(slot, key, session)) return { ok: true };
        return this.failSave(slot, errorText(fallbackError), "not-found");
      }
    }
    if (!this.isCurrent(slot, key, session)) return { ok: true };
    const recoveryKey = slot.key;
    if (saved.conversationId !== slot.conversationId && !this.rekey(slot, saved.conversationId))
      return this.failSave(slot, "New draft could not be opened safely. Your text is kept.", "not-found");
    if (recoveryKey !== slot.key) this.aliases.set(recoveryKey, slot.key);

    Object.assign(slot, {
      id: saved.id,
      revision: saved.revision,
      savedGeneration: Math.max(slot.savedGeneration, generation),
      error: undefined,
      acknowledgedContent: contentOf(saved),
    });
    this.reviewRequired.delete(slot.key);
    this.hydrated.add(saved.conversationId);
    this.saveStates.set(slot.key, "idle");
    if (slot.generation === generation) slot.content = contentOf(saved);
    this.events.changed();
    this.events.notice?.(slot.key, "saved");
    if (isDirty(slot)) void this.enqueue(slot);
    return { ok: true };
  }

  /** Reconciles a rejected CAS write without replacing recipient, attachment, or route changes. */
  private async catchUpStaleSave(slot: DraftSlot, session = this.sessionGeneration): Promise<boolean> {
    const key = slot.key;
    let current: Draft | undefined;
    try {
      current = await this.loadDraft(slot.conversationId);
    } catch (error) {
      if (!this.isCurrent(slot, key, session)) return false;
      this.failSave(slot, errorText(error), errorCode(error));
      return false;
    }
    if (!this.isCurrent(slot, key, session)) return false;
    if (!current) {
      this.failSave(slot, "Draft no longer exists.", "not-found");
      return false;
    }
    if (current.id !== slot.id && slot.id) {
      this.retainCopy(slot.key, slot.content);
      this.undoVersions.delete(slot.key);
      this.adoptHost(slot, current, current.conversationId === slot.conversationId ? "remote-update" : "moved");
      return false;
    }
    const hostContent = contentOf(current);
    if (!sameStructure(slot.acknowledgedContent, hostContent)) {
      this.retainCopy(slot.key, slot.content);
      this.undoVersions.delete(slot.key);
      this.adoptHost(slot, current, "remote-update");
      return false;
    }
    if (!sameDraftContent(slot.acknowledgedContent, hostContent)) {
      this.preserveUndo(slot, hostContent, current);
      this.events.notice?.(slot.key, "replaced");
    }
    this.installIdentity(slot, current);
    slot.acknowledgedContent = hostContent;
    return true;
  }

  private async adoptMovedOrMissing(slot: DraftSlot, error: unknown, session = this.sessionGeneration): Promise<boolean> {
    const destination = errorDetail(error, "conversationId");
    if (!destination) return false;
    let host: Draft | undefined;
    try {
      host = await this.loadDraft(destination);
    } catch {
      return false;
    }
    if (!this.isCurrent(slot, slot.key, session)) return false;
    if (!host) return false;
    this.retainCopy(slot.key, slot.content);
    this.undoVersions.delete(slot.key);
    this.adoptHost(slot, host, "moved");
    return true;
  }

  private adoptHost(slot: DraftSlot, host: Draft, event: "moved" | "remote-update"): boolean {
    const previousKey = slot.key;
    if (host.conversationId !== slot.conversationId && !this.rekey(slot, host.conversationId)) {
      const destination = this.slots.get(host.conversationId);
      if (destination) {
        this.retainCopy(destination.key, slot.content);
        this.reviewRequired.add(destination.key);
        this.events.notice?.(destination.key, "moved");
      }
      this.reviewRequired.add(host.conversationId);
      return false;
    }
    Object.assign(slot, {
      id: host.id,
      conversationId: host.conversationId,
      revision: host.revision,
      content: contentOf(host),
      acknowledgedContent: contentOf(host),
      savedGeneration: slot.generation,
      error: undefined,
    });
    this.saveStates.set(slot.key, "idle");
    this.reviewRequired.add(slot.key);
    this.events.notice?.(slot.key, event);
    if (previousKey !== slot.key) this.events.changed();
    return true;
  }

  private preserveUndo(slot: DraftSlot, content: DraftContent, reviewed: Draft) {
    if (!content.text && !content.attachmentIds.length) return;
    this.undoVersions.set(slot.key, {
      content: { ...content },
      generation: slot.generation,
      reviewedId: reviewed.id,
      reviewedRevision: reviewed.revision,
      reviewedContent: contentOf(reviewed),
    });
  }

  private fenceUndoAfterSave(slot: DraftSlot, generation: number, saved: Draft) {
    const undo = this.undoVersions.get(slot.key);
    if (!undo || undo.generation !== generation || slot.generation !== generation) return;
    undo.reviewedId = saved.id;
    undo.reviewedRevision = saved.revision;
    undo.reviewedContent = contentOf(saved);
  }

  private retainCopy(key: string, content: DraftContent) {
    if (!content.text && !content.attachmentIds.length) return;
    const retained = this.retainedVersions.get(key) ?? [];
    if (retained.some(version => sameDraftContent(version, content))) return;
    retained.push({ ...content });
    this.retainedVersions.set(key, retained);
  }

  /** Unsaved content under a pending review blocks close as a plain failure; it is never auto-saved over review. */
  private unsavedOutcome(slot: DraftSlot): SaveOutcome {
    slot.error ??= { message: "Draft has unsaved changes. Retry saving before closing." };
    if (this.saveStates.get(slot.key) !== "failed") {
      this.saveStates.set(slot.key, "failed");
      this.events.changed();
    }
    return { ok: false, error: slot.error.message };
  }

  private reviewOutcome(slot: DraftSlot): SaveOutcome {
    this.reviewRequired.add(slot.key);
    this.saveStates.set(slot.key, "idle");
    return { ok: false, error: "Draft changed in another window. Review the retained text before sending.", reviewRequired: true };
  }

  private failSave(slot: DraftSlot, message: string, code?: string): SaveOutcome {
    slot.error = { message, code };
    this.saveStates.set(slot.key, "failed");
    this.events.notice?.(slot.key, "save-failed");
    this.events.changed();
    return { ok: false, error: message };
  }

  private async verifyBufferedIdentity(conversationId: string, snapshot: Draft | undefined) {
    const session = this.sessionGeneration;
    const slot = this.get(conversationId);
    try {
      const current = await this.loadDraft(conversationId);
      if (!slot || !this.isCurrent(slot, slot.key, session) || (snapshot && slot.id !== snapshot.id) || isDirty(slot)) return;
      if (!current) {
        if (snapshot) this.retireDraftIdentity(snapshot.id, conversationId, snapshot.revision);
        Object.assign(slot, {
          id: "",
          revision: "0",
          content: blankContent(),
          acknowledgedContent: blankContent(),
          error: undefined,
        });
        this.events.changed();
        return;
      }
      if (this.shouldAdopt(slot, current)) this.applyAuthoritative(current);
    } catch {
      // The hydrated snapshot remains authoritative until a later host event arrives.
    }
  }

  private rekey(slot: DraftSlot, conversationId: string): boolean {
    const from = slot.key;
    if (from === conversationId) return true;
    const destination = this.slots.get(conversationId);
    if (destination && destination !== slot && isDirty(destination)) {
      return false;
    }
    this.slots.delete(from);
    this.migrateRetained(from, conversationId);
    if (isLocalDraftKey(from)) this.aliases.set(from, conversationId);
    slot.key = conversationId;
    slot.conversationId = conversationId;
    this.slots.set(conversationId, slot);
    this.hydrated.add(conversationId);
    this.events.rekeyed(from, conversationId);
    return true;
  }

  private migrateRetained(from: string, to: string) {
    const retained = this.undoVersions.get(from);
    if (retained) {
      this.undoVersions.delete(from);
      this.undoVersions.set(to, retained);
    }
    const retainedCopy = this.retainedVersions.get(from);
    if (retainedCopy) {
      this.retainedVersions.delete(from);
      for (const content of retainedCopy) this.retainCopy(to, content);
    }
    if (this.reviewRequired.delete(from)) this.reviewRequired.add(to);
    const pending = this.pendingSentVerification.get(from);
    if (pending !== undefined) {
      this.pendingSentVerification.delete(from);
      this.pendingSentVerification.set(to, pending);
    }
  }

  private shouldAdopt(slot: Pick<DraftSlot, "id" | "revision">, incoming: Draft): boolean {
    if (slot.id !== incoming.id) return true;
    try {
      return BigInt(incoming.revision) > BigInt(slot.revision);
    } catch {
      return false;
    }
  }

  private installIdentity(slot: DraftSlot, draft: Draft) {
    Object.assign(slot, {
      id: draft.id,
      conversationId: draft.conversationId,
      revision: draft.revision,
    });
  }

  private hasRecoverableContent(draft: Draft) {
    const content = contentOf(draft);
    return Boolean(content.text || content.attachmentIds.length || content.recipientIds.length);
  }

  private isUnavailable(slot: DraftSlot) {
    return this.tombstones.has(slot.key) || this.slots.get(slot.key) !== slot;
  }

  private isCurrent(slot: DraftSlot, key: string, session: number): boolean {
    return session === this.sessionGeneration && this.slots.get(key) === slot && slot.key === key;
  }

  private retireIdentity(slot: DraftSlot) {
    this.retireDraftIdentity(slot.id, slot.conversationId, slot.revision);
  }

  private isRetiredInConversation(draft: Draft) {
    const retired = this.retiredDraftIds.get(draft.id);
    if (!retired || retired.conversationId !== draft.conversationId) return false;
    try {
      return BigInt(draft.revision) <= BigInt(retired.revision);
    } catch {
      return true;
    }
  }

  private retireDraftIdentity(id: string, conversationId: string, revision: string) {
    this.retiredDraftIds.set(id, { conversationId, revision });
  }
}
