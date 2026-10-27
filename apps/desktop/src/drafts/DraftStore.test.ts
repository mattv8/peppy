import { describe, expect, it, vi } from "vitest";
import type { Draft, DraftInput } from "../bridge";
import { DraftStore } from "./DraftStore";

const draft = (overrides: Partial<Draft> = {}): Draft => ({
  id: "draft-one",
  conversationId: "conversation-one",
  text: "stored text",
  recipientIds: ["+15550001"],
  attachmentIds: [],
  revision: "1",
  ...overrides,
});

const events = { changed: () => {}, rekeyed: () => {} };

describe("DraftStore authoritative coordination", () => {
  it("fails pre-hydration saves without persisting, then releases safe local edits", async () => {
    const persisted: DraftInput[] = [];
    const store = new DraftStore(async input => {
      persisted.push(input);
      return draft({ ...input, revision: "8" });
    }, events);

    const saving = store.edit("conversation-one", { text: "local text" });
    await expect(saving).resolves.toEqual({ ok: false, error: "Draft is still loading. Retry once it is available." });
    expect(persisted).toEqual([]);

    store.adopt("conversation-one", draft({ text: "", recipientIds: [], revision: "7" }));
    await vi.waitFor(() => expect(persisted).toHaveLength(1));
    expect(persisted).toMatchObject([{ id: "draft-one", expectedRevision: "7", text: "local text" }]);
  });

  it("keeps both versions when first hydration finds a competing nonempty draft", async () => {
    const persisted = vi.fn(async (input: DraftInput) => draft(input));
    const store = new DraftStore(persisted, events);

    await store.edit("conversation-one", { text: "local", gatewayId: "phone", simId: "sim-a", attachmentIds: ["local-file"] });
    store.adopt("conversation-one", draft({ text: "stored", gatewayId: "phone", simId: "sim-b", attachmentIds: ["stored-file"] }));

    expect(persisted).not.toHaveBeenCalled();
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "local", attachmentIds: ["local-file"] }, error: { code: "stale-draft" } });
    expect(store.recoveriesFor("conversation-one")).toEqual([
      expect.objectContaining({ content: expect.objectContaining({ text: "stored", attachmentIds: ["stored-file"], simId: "sim-b" }) }),
    ]);
  });

  it("records empty hydration so an empty existing conversation can save", async () => {
    const persisted = vi.fn(async (input: DraftInput) => draft({ ...input, id: "draft-created", revision: "1" }));
    const store = new DraftStore(persisted, events);

    store.adopt("conversation-one", undefined);
    await expect(store.edit("conversation-one", { text: "first reply" })).resolves.toEqual({ ok: true });

    expect(persisted).toHaveBeenCalledWith(expect.objectContaining({ conversationId: "conversation-one", id: "", expectedRevision: "0" }));
  });

  it("does not let an older authoritative event replace a newer revision", () => {
    const store = new DraftStore(async input => draft(input), events);

    store.adopt("conversation-one", draft({ text: "revision one", revision: "1" }));
    store.applyAuthoritative(draft({ text: "revision maximum minus one", revision: "18446744073709551614" }));
    store.applyAuthoritative(draft({ text: "revision maximum", revision: "18446744073709551615" }));
    store.applyAuthoritative(draft({ text: "stale", revision: "18446744073709551614" }));

    expect(store.get("conversation-one")).toMatchObject({ content: { text: "revision maximum" }, revision: "18446744073709551615" });
  });

  it("coordinates two stores through CAS without crossing conversations", async () => {
    const stored = new Map<string, Draft>([
      ["conversation-one", draft()],
      ["conversation-two", draft({ id: "draft-two", conversationId: "conversation-two", text: "two" })],
    ]);
    const persist = async (input: DraftInput) => {
      const current = stored.get(input.conversationId);
      if (current && input.expectedRevision !== current.revision)
        throw { code: "stale-draft", message: "stale" };
      const saved = draft({ ...input, id: current?.id ?? "draft-created", revision: (BigInt(current?.revision ?? "0") + 1n).toString() });
      stored.set(saved.conversationId, saved);
      return saved;
    };
    const first = new DraftStore(persist, events);
    const second = new DraftStore(persist, events);
    first.adopt("conversation-one", stored.get("conversation-one"));
    second.adopt("conversation-one", stored.get("conversation-one"));
    second.adopt("conversation-two", stored.get("conversation-two"));

    await first.edit("conversation-one", { text: "first" });
    second.applyAuthoritative(stored.get("conversation-one")!);
    await expect(second.edit("conversation-one", { text: "second" })).resolves.toEqual({ ok: true });
    await expect(second.edit("conversation-two", { text: "other" })).resolves.toEqual({ ok: true });
    expect(stored.get("conversation-one")?.text).toBe("second");
    expect(stored.get("conversation-two")?.text).toBe("other");
  });

  it("keeps a dirty second writer unchanged when shared CAS rejects its stale revision", async () => {
    let stored = draft({ text: "original", revision: "1" });
    const persist = async (input: DraftInput) => {
      if (input.expectedRevision !== stored.revision) throw { code: "stale-draft", message: "stale" };
      stored = draft({ ...input, revision: (BigInt(stored.revision) + 1n).toString() });
      return stored;
    };
    const first = new DraftStore(persist, events);
    const second = new DraftStore(persist, events);
    first.adopt("conversation-one", stored);
    second.adopt("conversation-one", stored);

    await first.edit("conversation-one", { text: "first writer" });
    await expect(second.edit("conversation-one", { text: "second writer" })).resolves.toEqual({ ok: false, error: "stale" });
    expect(second.get("conversation-one")).toMatchObject({ content: { text: "second writer" }, error: { code: "stale-draft" } });
    expect(stored.text).toBe("first writer");
  });

  it("does not overwrite a dirty receiver with another window's event", async () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft());
    const saving = store.edit("conversation-one", { text: "local edit" });
    store.applyAuthoritative(draft({ text: "remote edit", revision: "2" }));

    expect(store.get("conversation-one")?.content.text).toBe("local edit");
    await saving;
  });

  it("preserves the stored version before deliberately keeping local conflict text", async () => {
    const persisted: DraftInput[] = [];
    const store = new DraftStore(async input => {
      persisted.push(input);
      return draft({ ...input, revision: "3" });
    }, events);
    store.adopt("conversation-one", draft({ text: "original", revision: "1" }));
    await store.edit("conversation-one", { text: "local" });
    store.edit("conversation-one", { text: "local conflict" });

    await expect(store.resolveKeepLocal("conversation-one", draft({ text: "stored conflict", revision: "2" }))).resolves.toEqual({ ok: true });
    expect(store.recoveriesFor("conversation-one")).toEqual([
      expect.objectContaining({ content: expect.objectContaining({ text: "stored conflict" }) }),
    ]);
    expect(persisted.at(-1)).toMatchObject({ text: "local conflict" });
  });

  it("keeps local route and attachments recoverable when using the reviewed stored conflict", async () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft({ text: "stored", gatewayId: "gateway", simId: "sim-stored", attachmentIds: ["stored-file"] }));
    await store.edit("conversation-one", { text: "local", gatewayId: "gateway", simId: "sim-local", attachmentIds: ["local-file"] });

    await store.resolveUseStored("conversation-one", draft({ text: "stored replacement", gatewayId: "gateway", simId: "sim-stored", attachmentIds: ["stored-file"] }));
    expect(store.get("conversation-one")?.content.text).toBe("stored replacement");
    expect(store.recoveriesFor("conversation-one")).toEqual(expect.arrayContaining([
      expect.objectContaining({ content: expect.objectContaining({ text: "local", simId: "sim-local", attachmentIds: ["local-file"] }) }),
    ]));
  });

  it("retains local route and attachments when the reviewed stored draft is absent", async () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft({ text: "stored", gatewayId: "gateway", simId: "sim-stored", attachmentIds: ["stored-file"] }));
    await store.edit("conversation-one", { text: "local", gatewayId: "gateway", simId: "sim-local", attachmentIds: ["local-file"] });

    await store.resolveUseStored("conversation-one", undefined);
    expect(store.get("conversation-one")?.content.text).toBe("");
    expect(store.recoveriesFor("conversation-one")).toEqual(expect.arrayContaining([
      expect.objectContaining({ content: expect.objectContaining({ text: "local", simId: "sim-local", attachmentIds: ["local-file"] }) }),
    ]));
  });

  it("acknowledges an in-flight held save before the next CAS edit", async () => {
    let resolveFirst: ((value: Draft) => void) | undefined;
    let stored = draft({ revision: "1" });
    const store = new DraftStore(async input => {
      if (!resolveFirst) return new Promise<Draft>(resolve => { resolveFirst = resolve; });
      if (input.expectedRevision !== stored.revision) throw { code: "stale-draft", message: "stale" };
      stored = draft({ ...input, revision: "3" });
      return stored;
    }, events);
    store.adopt("conversation-one", stored);

    const saving = store.edit("conversation-one", { text: "first" });
    await vi.waitFor(() => expect(resolveFirst).toBeTypeOf("function"));
    store.hold("conversation-one");
    stored = draft({ text: "first", revision: "2" });
    resolveFirst!(stored);
    await expect(saving).resolves.toEqual({ ok: false, error: "A send is in progress; your edits are kept and saved afterwards." });
    store.release("conversation-one");
    await expect(store.edit("conversation-one", { text: "second" })).resolves.toEqual({ ok: true });
  });

  it("does not resurrect a sent draft from a delayed authoritative payload", () => {
    const store = new DraftStore(async input => draft(input), events);
    const saved = draft({ text: "sent text", revision: "9" });
    store.adopt("conversation-one", saved);
    store.markSent("conversation-one", 0);
    store.applyAuthoritative(saved);

    expect(store.get("conversation-one")?.content).toEqual({ text: "", recipientIds: [], attachmentIds: [] });
  });

  it("fences only delayed sent revisions while accepting newer same-id snapshots", () => {
    const store = new DraftStore(async input => draft(input), events);
    const sent = draft({ text: "sent text", revision: "9" });
    store.adopt("conversation-one", sent);
    store.markSent("conversation-one", 0);
    store.adopt("conversation-one", sent);
    expect(store.get("conversation-one")?.content.text).toBe("");

    store.adopt("conversation-one", draft({ text: "new text", revision: "10" }));
    store.applyAuthoritative(draft({ text: "newer text", revision: "11" }));
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "newer text" }, revision: "11" });
  });

  it("accepts a newer moved payload after a rekey collision but rejects equal and older source payloads", async () => {
    const store = new DraftStore(async input => {
      if (input.conversationId === "destination") throw { message: "offline" };
      return draft({ ...input, id: "moved-draft", conversationId: "destination", revision: "2" });
    }, events);
    store.adopt("destination", undefined);
    await store.edit("destination", { text: "destination edit" });

    await store.resolveUseStored("destination", undefined);
    await expect(store.edit("new:source", { text: "source edit" })).resolves.toEqual({ ok: true });
    expect(store.get("destination")?.content.text).toBe("source edit");

    store.applyAuthoritative(draft({ id: "moved-draft", conversationId: "destination", text: "authoritative destination", revision: "3" }));
    expect(store.get("destination")?.content.text).toBe("authoritative destination");
    store.applyAuthoritative(draft({ id: "moved-draft", conversationId: "destination", text: "stale source", revision: "2" }));
    expect(store.get("destination")?.content.text).toBe("authoritative destination");
  });
});
