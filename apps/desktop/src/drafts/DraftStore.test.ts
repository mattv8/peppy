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
  it("does not restore retained text onto different recipients", async () => {
    const remote = draft({ text: "remote", recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async input => {
      if (input.expectedRevision === "1") throw { code: "stale-draft", message: "stale" };
      return draft({ ...input, revision: "3" });
    }, events, async () => remote);
    store.adopt("conversation-one", draft({ text: "original" }));

    await store.edit("conversation-one", { text: "retained" });

    expect(store.hasUndo("conversation-one")).toBe(false);
    expect(store.retainedText("conversation-one")).toBe("retained");
    await expect(store.undo("conversation-one")).resolves.toEqual({ ok: false, error: "No retained draft text is available." });
  });

  it("returns review-required from flush after structural adoption", async () => {
    const remote = draft({ recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async () => { throw { code: "stale-draft", message: "stale" }; }, events, async () => remote);
    store.adopt("conversation-one", draft());

    const pending = store.edit("conversation-one", { text: "local" });
    await expect(pending).resolves.toMatchObject({ ok: false, reviewRequired: true });
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false, reviewRequired: true });
  });

  it("keeps the hydration snapshot when a buffered event has a different identity", () => {
    const store = new DraftStore(async input => draft(input), events);
    store.applyAuthoritative(draft({ id: "old-id", text: "discarded", revision: "9" }));
    store.adopt("conversation-one", draft({ id: "new-id", text: "current", revision: "1" }));

    expect(store.get("conversation-one")).toMatchObject({ id: "new-id", content: { text: "current" } });
  });

  it("uses a verified buffered identity when the hydration snapshot is stale", async () => {
    const current = draft({ id: "current", text: "current host", revision: "2" });
    const store = new DraftStore(async input => draft(input), events, async () => current);
    store.applyAuthoritative(current);
    store.adopt("conversation-one", draft({ id: "stale", text: "stale snapshot", revision: "9" }));

    await vi.waitFor(() => expect(store.get("conversation-one")).toMatchObject({ id: "current", content: { text: "current host" } }));
  });

  it("clears an unverified hydration identity so its delayed event cannot resurrect it", async () => {
    const store = new DraftStore(async input => draft(input), events, async () => undefined);
    store.applyAuthoritative(draft({ id: "old", text: "old event", revision: "2" }));
    store.adopt("conversation-one", draft({ id: "snapshot", text: "snapshot", revision: "1" }));

    await vi.waitFor(() => expect(store.get("conversation-one")).toMatchObject({ id: "", content: { text: "" } }));
    store.applyAuthoritative(draft({ id: "snapshot", text: "resurrected", revision: "1" }));
    expect(store.get("conversation-one")?.content.text).toBe("");
  });

  it("releases a failed sent-revision verification so retry can verify again", async () => {
    let attempts = 0;
    const host = draft({ text: "", revision: "2" });
    const store = new DraftStore(async input => draft(input), events, async () => {
      attempts += 1;
      if (attempts === 1) throw { code: "offline", message: "offline" };
      return host;
    });
    store.adopt("conversation-one", draft({ text: "sent" }));

    store.markSent("conversation-one", 0);
    await vi.waitFor(() => expect(store.saveState("conversation-one")).toBe("failed"));
    await expect(store.retry("conversation-one")).resolves.toEqual({ ok: true });
    expect(store.get("conversation-one")?.held).toBe(false);
  });

  it("keeps complete local content and fails conservatively when the host row is absent after a stale save", async () => {
    const writes: DraftInput[] = [];
    const store = new DraftStore(async input => {
      writes.push(input);
      throw { code: "stale-draft", message: "stale" };
    }, events, async () => undefined);
    store.adopt("conversation-one", draft({ text: "stored", gatewayId: "gateway", simId: "sim-stored", attachmentIds: ["stored-file"] }));

    await expect(store.edit("conversation-one", { text: "local", gatewayId: "gateway", simId: "sim-local", attachmentIds: ["local-file"] }))
      .resolves.toEqual({ ok: false, error: "Draft no longer exists." });

    expect(writes).toHaveLength(1);
    expect(store.saveState("conversation-one")).toBe("failed");
    expect(store.get("conversation-one")).toMatchObject({
      id: "draft-one",
      conversationId: "conversation-one",
      content: { text: "local", recipientIds: ["+15550001"], gatewayId: "gateway", simId: "sim-local", attachmentIds: ["local-file"] },
      error: { code: "not-found" },
    });
  });

  it("recreates a deleted draft only on explicit recovery with its complete local content", async () => {
    const writes: DraftInput[] = [];
    let missing = true;
    const store = new DraftStore(async input => {
      writes.push(input);
      if (missing) throw { code: "not-found", message: "deleted" };
      return draft({ ...input, id: "replacement", conversationId: "conversation-recovered", revision: "1" });
    }, events);
    store.adopt("conversation-one", draft({ gatewayId: "gateway", simId: "sim", attachmentIds: ["file"] }));

    await expect(store.edit("conversation-one", { text: "kept", recipientIds: ["+15550002"] })).resolves.toMatchObject({ ok: false });
    expect(writes).toHaveLength(1);
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false });

    missing = false;
    await expect(store.saveAsNew("conversation-one")).resolves.toEqual({ ok: true });
    expect(writes.at(-1)).toMatchObject({ id: "", conversationId: "conversation-one", expectedRevision: "0", text: "kept", recipientIds: ["+15550002"], attachmentIds: ["file"], gatewayId: "gateway", simId: "sim" });
    expect(store.get("conversation-one")).toMatchObject({ id: "replacement", conversationId: "conversation-recovered", revision: "1", error: undefined });
    await expect(store.flush("conversation-one")).resolves.toEqual({ ok: true });
  });

  it("falls back to a blank conversation only when the original conversation no longer exists", async () => {
    const writes: DraftInput[] = [];
    const store = new DraftStore(async input => {
      writes.push(input);
      if (!input.id) {
        if (input.conversationId) throw { code: "not-found", message: "no history" };
        return draft({ ...input, id: "replacement", conversationId: "conversation-new", revision: "1" });
      }
      throw { code: "not-found", message: "deleted" };
    }, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "kept" });

    await expect(store.saveAsNew("conversation-one")).resolves.toEqual({ ok: true });
    expect(writes.slice(-2)).toEqual([
      expect.objectContaining({ id: "", conversationId: "conversation-one", expectedRevision: "0" }),
      expect.objectContaining({ id: "", conversationId: "", expectedRevision: "0" }),
    ]);
    expect(store.get("conversation-one")).toMatchObject({ conversationId: "conversation-new", id: "replacement" });
  });

  it("keeps deleted-draft recovery actionable after a failed create and coalesces double clicks", async () => {
    let resolveCreate!: (value: Draft) => void;
    const persist = vi.fn(async (input: DraftInput) => {
      if (input.id) throw { code: "not-found", message: "deleted" };
      return new Promise<Draft>(resolve => { resolveCreate = resolve; });
    });
    const store = new DraftStore(persist, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "kept" });

    const first = store.saveAsNew("conversation-one");
    const second = store.saveAsNew("conversation-one");
    expect(first).toBe(second);
    await vi.waitFor(() => expect(resolveCreate).toBeTypeOf("function"));
    resolveCreate(draft({ id: "replacement", text: "kept", revision: "2" }));
    await expect(first).resolves.toEqual({ ok: true });
    expect(persist).toHaveBeenCalledTimes(2);
  });

  it("retains complete content and allows another explicit recovery attempt after creation fails", async () => {
    let recoverAttempts = 0;
    const store = new DraftStore(async input => {
      if (input.id) throw { code: "not-found", message: "deleted" };
      recoverAttempts += 1;
      if (recoverAttempts === 1) throw { code: "io", message: "offline" };
      return draft({ ...input, id: "replacement", revision: "2" });
    }, events);
    store.adopt("conversation-one", draft({ attachmentIds: ["file"], gatewayId: "gateway", simId: "sim" }));
    await store.edit("conversation-one", { text: "kept" });

    await expect(store.saveAsNew("conversation-one")).resolves.toEqual({ ok: false, error: "offline" });
    expect(store.get("conversation-one")).toMatchObject({ error: { code: "not-found" }, content: { text: "kept", attachmentIds: ["file"], gatewayId: "gateway", simId: "sim" } });
    await expect(store.saveAsNew("conversation-one")).resolves.toEqual({ ok: true });
  });

  it("saves edits made during deleted-draft recovery against the new identity", async () => {
    let resolveCreate!: (value: Draft) => void;
    const writes: DraftInput[] = [];
    const store = new DraftStore(async input => {
      writes.push(input);
      if (input.id) throw { code: "not-found", message: "deleted" };
      if (!resolveCreate) return new Promise(resolve => { resolveCreate = resolve; });
      return draft({ ...input, revision: "3" });
    }, events);
    store.adopt("conversation-one", draft({ text: "old" }));
    await store.edit("conversation-one", { text: "first" });

    const recovery = store.saveAsNew("conversation-one");
    await vi.waitFor(() => expect(resolveCreate).toBeTypeOf("function"));
    await store.edit("conversation-one", { text: "during recovery", attachmentIds: ["new-file"] });
    resolveCreate(draft({ id: "replacement", text: "first", revision: "2" }));
    await expect(recovery).resolves.toEqual({ ok: true });
    await vi.waitFor(() => expect(writes.at(-1)).toMatchObject({ id: "replacement", expectedRevision: "2", text: "during recovery", attachmentIds: ["new-file"] }));
  });

  it("ignores a deleted-draft recovery completion after session reset", async () => {
    let resolveCreate!: (value: Draft) => void;
    const store = new DraftStore(async input => {
      if (input.id) throw { code: "not-found", message: "deleted" };
      return new Promise(resolve => { resolveCreate = resolve; });
    }, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "kept" });
    const recovery = store.saveAsNew("conversation-one");
    await vi.waitFor(() => expect(resolveCreate).toBeTypeOf("function"));
    store.reset();
    resolveCreate(draft({ id: "replacement", text: "kept", revision: "2" }));

    await expect(recovery).resolves.toEqual({ ok: true });
    expect(store.get("conversation-one")).toBeUndefined();
  });

  it("keeps local content blocked when blank recovery finds an occupied host draft", async () => {
    const occupied = draft({ id: "occupied", text: "other window", attachmentIds: ["other-file"], revision: "4" });
    const store = new DraftStore(async input => {
      if (input.id) throw { code: "not-found", message: "deleted" };
      throw { code: "draft-moved", message: "occupied" };
    }, events);
    store.adopt("conversation-one", draft({ text: "stored" }));
    await store.edit("conversation-one", { text: "local", attachmentIds: ["local-file"] });

    await expect(store.saveAsNew("conversation-one")).resolves.toEqual({ ok: false, error: "occupied" });
    expect(occupied).toMatchObject({ text: "other window", attachmentIds: ["other-file"] });
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "local", attachmentIds: ["local-file"] }, error: { code: "not-found" } });
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false });
  });

  it("discards only the failed local slot without a host write", async () => {
    const persist = vi.fn(async () => { throw { code: "not-found", message: "deleted" }; });
    const store = new DraftStore(persist, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "local", attachmentIds: ["file"] });

    expect(store.discardLocalChanges("conversation-one")).toBe(true);
    expect(store.get("conversation-one")).toBeUndefined();
    expect(persist).toHaveBeenCalledTimes(1);
  });

  it("allows a discarded local failure to hydrate and save again in the same conversation", async () => {
    const store = new DraftStore(async input => {
      if (input.id === "draft-one") throw { code: "not-found", message: "deleted" };
      return draft({ ...input, id: "replacement", revision: "2" });
    }, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "local" });
    expect(store.discardLocalChanges("conversation-one")).toBe(true);

    store.adopt("conversation-one", undefined);
    await expect(store.edit("conversation-one", { text: "fresh" })).resolves.toEqual({ ok: true });
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "fresh" }, id: "replacement" });
  });

  it("moves retained text to the destination key after a verified draft move", async () => {
    const destination = draft({ id: "destination-draft", conversationId: "conversation-two", text: "remote", revision: "2" });
    const store = new DraftStore(async () => {
      throw { code: "draft-moved", message: "moved", details: { conversationId: "conversation-two" } };
    }, events, async () => destination);
    store.adopt("conversation-one", draft({ text: "local" }));

    await expect(store.edit("conversation-one", { text: "local changed" })).resolves.toMatchObject({ ok: false, reviewRequired: true });
    expect(store.retainedText("conversation-two")).toBe("local changed");
  });

  it("keeps a dirty destination and exposes moved text there without recovery cards", async () => {
    const destination = draft({ id: "destination-draft", conversationId: "conversation-two", text: "remote", revision: "2" });
    const store = new DraftStore(async input => {
      if (input.conversationId === "conversation-one")
        throw { code: "draft-moved", message: "moved", details: { conversationId: "conversation-two" } };
      return draft(input);
    }, events, async () => destination);
    store.adopt("conversation-two", draft({ conversationId: "conversation-two", id: "existing", text: "destination" }));
    store.hold("conversation-two");
    await store.edit("conversation-two", { text: "destination dirty" });
    store.adopt("conversation-one", draft({ text: "source" }));

    await expect(store.edit("conversation-one", { text: "source dirty" })).resolves.toMatchObject({ ok: false, reviewRequired: true });
    expect(store.get("conversation-two")?.content.text).toBe("destination dirty");
    expect(store.retainedText("conversation-two")).toBe("source dirty");
  });

  it("fast-forwards a stale save when the loaded host draft still matches the acknowledged content", async () => {
    let stored = draft({ text: "original", revision: "2" });
    const persist = vi.fn(async (input: DraftInput) => {
      if (input.expectedRevision !== stored.revision) throw { code: "stale-draft", message: "stale" };
      stored = draft({ ...input, revision: "3" });
      return stored;
    });
    const store = new DraftStore(persist, events, async () => stored);
    store.adopt("conversation-one", draft({ text: "original", revision: "1" }));

    await expect(store.edit("conversation-one", { text: "local edit" })).resolves.toEqual({ ok: true });

    expect(persist).toHaveBeenLastCalledWith(expect.objectContaining({ expectedRevision: "2", text: "local edit" }));
    expect(store.get("conversation-one")).toMatchObject({ revision: "3", content: { text: "local edit" } });
  });

  it("adopts structural remote changes after a stale save and makes the local version safely undoable", async () => {
    const remote = draft({ text: "remote", recipientIds: ["+15550002"], attachmentIds: ["remote-file"], revision: "2" });
    const persist = vi.fn(async (input: DraftInput) => {
      if (input.expectedRevision === "1") throw { code: "stale-draft", message: "stale" };
      return draft({ ...input, revision: "3" });
    });
    const store = new DraftStore(persist, events, async () => remote);
    store.adopt("conversation-one", draft({ text: "original", revision: "1" }));

    await expect(store.edit("conversation-one", { text: "local" })).resolves.toMatchObject({ ok: false, reviewRequired: true });
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "remote", recipientIds: ["+15550002"], attachmentIds: ["remote-file"] }, revision: "2" });

    await expect(store.undo("conversation-one")).resolves.toEqual({ ok: false, error: "No retained draft text is available." });
    expect(store.retainedText("conversation-one")).toBe("local");
  });

  it("keeps a sent identity and returned revision while preserving recipients and route", () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft({ text: "send me", attachmentIds: ["file"], gatewayId: "gateway", simId: "sim" }));

    store.markSent("conversation-one", 0, "9");

    expect(store.get("conversation-one")).toMatchObject({
      id: "draft-one",
      revision: "9",
      content: { text: "", attachmentIds: [], recipientIds: ["+15550001"], gatewayId: "gateway", simId: "sim" },
    });
  });

  it("saves post-send text with the returned revision instead of resending the sent body", async () => {
    const inputs: DraftInput[] = [];
    const store = new DraftStore(async input => {
      inputs.push(input);
      return draft({ ...input, revision: "10" });
    }, events);
    store.adopt("conversation-one", draft({ text: "sent body", attachmentIds: ["sent-file"], revision: "8" }));
    store.hold("conversation-one");
    await store.edit("conversation-one", { text: "sent body plus typing", attachmentIds: ["sent-file", "new-file"] });

    store.markSent("conversation-one", 0, "9", { text: "new typing", attachmentIds: ["new-file"] });
    await vi.waitFor(() => expect(inputs).toHaveLength(1));

    expect(inputs[0]).toMatchObject({ expectedRevision: "9", text: "new typing", attachmentIds: ["new-file"] });
  });

  it("reconciles an authoritative event received before hydration", () => {
    const store = new DraftStore(async input => draft(input), events);
    store.applyAuthoritative(draft({ text: "event", revision: "3" }));
    store.adopt("conversation-one", draft({ text: "snapshot", revision: "2" }));

    expect(store.get("conversation-one")).toMatchObject({ content: { text: "event" }, revision: "3" });
    expect(store.isHydrated("conversation-one")).toBe(true);
  });

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
    expect(store.retainedText("conversation-one")).toBe("stored");
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
    const second = new DraftStore(persist, events, async conversationId => stored.get(conversationId));
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
    const second = new DraftStore(persist, events, async () => stored);
    first.adopt("conversation-one", stored);
    second.adopt("conversation-one", stored);

    await first.edit("conversation-one", { text: "first writer" });
    await expect(second.edit("conversation-one", { text: "second writer" })).resolves.toEqual({ ok: true });
    expect(second.get("conversation-one")).toMatchObject({ content: { text: "second writer" }, error: undefined });
    expect(stored.text).toBe("second writer");
  });

  it("does not overwrite a dirty receiver with another window's event", async () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft());
    const saving = store.edit("conversation-one", { text: "local edit" });
    store.applyAuthoritative(draft({ text: "remote edit", revision: "2" }));

    expect(store.get("conversation-one")?.content.text).toBe("local edit");
    await saving;
  });

  it("preserves a clean text-only remote replacement as one generation-fenced undo", async () => {
    const notices: string[] = [];
    let host = draft({ text: "A", revision: "1" });
    const persist = vi.fn(async input => {
      host = draft({ ...input, revision: "3" });
      return host;
    });
    const store = new DraftStore(persist, { ...events, notice: (_, event) => notices.push(event) }, async () => host);
    store.adopt("conversation-one", host);
    const remote = draft({ text: "B", revision: "2" });
    host = remote;

    store.applyAuthoritative(remote);
    store.applyAuthoritative(remote);

    expect(store.get("conversation-one")?.content.text).toBe("B");
    expect(store.hasUndo("conversation-one")).toBe(true);
    expect(notices).toEqual(["remote-update"]);
    await expect(store.undo("conversation-one")).resolves.toEqual({ ok: true });
    expect(persist).toHaveBeenCalledWith(expect.objectContaining({ text: "A", expectedRevision: "2" }));
  });

  it("does not restore an old undo after later local edits and host updates", async () => {
    let host = draft({ text: "A", revision: "1" });
    const writes: DraftInput[] = [];
    const store = new DraftStore(async input => {
      writes.push(input);
      if (input.expectedRevision !== host.revision) throw { code: "stale-draft", message: "stale" };
      host = draft({ ...input, revision: (BigInt(host.revision) + 1n).toString() });
      return host;
    }, events, async () => host);
    store.adopt("conversation-one", host);
    store.applyAuthoritative(draft({ text: "B", revision: "2" }));

    await store.edit("conversation-one", { text: "C" });
    host = draft({ text: "D", revision: "4" });
    store.applyAuthoritative(host);
    await store.edit("conversation-one", { text: "E" });

    await expect(store.undo("conversation-one")).resolves.toEqual({ ok: false, error: "No retained draft text is available." });
    expect(store.get("conversation-one")?.content.text).toBe("E");
    expect(writes.map(write => write.text)).not.toContain("A");
  });

  it("rejects undo when the loaded host no longer matches the reviewed revision", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft({ ...input, revision: "4" }));
    const store = new DraftStore(persist, events, async () => draft({ text: "D", revision: "3" }));
    store.adopt("conversation-one", draft({ text: "A", revision: "1" }));
    store.applyAuthoritative(draft({ text: "B", revision: "2" }));

    await expect(store.undo("conversation-one")).resolves.toEqual({ ok: false, error: "Draft structure changed; local text is kept for review." });
    expect(store.get("conversation-one")?.content.text).toBe("B");
    expect(persist).not.toHaveBeenCalled();
  });

  it("fences an undo load when typing changes the draft while it awaits the host", async () => {
    let resolveLoad!: (value: Draft) => void;
    const persist = vi.fn(async (input: DraftInput) => draft({ ...input, revision: "3" }));
    const store = new DraftStore(persist, events, async () => new Promise(resolve => { resolveLoad = resolve; }));
    store.adopt("conversation-one", draft({ text: "A", revision: "1" }));
    store.applyAuthoritative(draft({ text: "B", revision: "2" }));

    const pendingUndo = store.undo("conversation-one");
    await vi.waitFor(() => expect(resolveLoad).toBeTypeOf("function"));
    await store.edit("conversation-one", { text: "typing" });
    resolveLoad(draft({ text: "B", revision: "2" }));

    await expect(pendingUndo).resolves.toEqual({ ok: false, error: "Draft structure changed; local text is kept for review." });
    expect(store.get("conversation-one")?.content.text).toBe("typing");
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
    store.markSent("conversation-one", 0, "10");
    store.applyAuthoritative(saved);

    expect(store.get("conversation-one")?.content).toMatchObject({ text: "", recipientIds: ["+15550001"], attachmentIds: [] });
  });

  it("fences only delayed sent revisions while accepting newer same-id snapshots", () => {
    const store = new DraftStore(async input => draft(input), events);
    const sent = draft({ text: "sent text", revision: "9" });
    store.adopt("conversation-one", sent);
    store.markSent("conversation-one", 0, "10");
    store.adopt("conversation-one", sent);
    expect(store.get("conversation-one")?.content.text).toBe("");

    store.adopt("conversation-one", draft({ text: "new text", revision: "10" }));
    store.applyAuthoritative(draft({ text: "newer text", revision: "11" }));
    expect(store.get("conversation-one")).toMatchObject({ content: { text: "newer text" }, revision: "11" });
  });

  it("accepts a newer moved payload but rejects equal and older source payloads", async () => {
    const store = new DraftStore(async input => draft({ ...input, id: "moved-draft", conversationId: "destination", revision: "2" }), events);
    store.adopt("destination", undefined);
    await store.edit("destination", { text: "destination edit" });

    store.adopt("source", draft({ conversationId: "source", id: "source-draft", text: "" }));
    await expect(store.edit("source", { text: "source edit" })).resolves.toMatchObject({ ok: false, reviewRequired: true });
    expect(store.get("destination")?.content.text).toBe("source edit");

    store.applyAuthoritative(draft({ id: "moved-draft", conversationId: "destination", text: "authoritative destination", revision: "3" }));
    expect(store.get("destination")?.content.text).toBe("authoritative destination");
    store.applyAuthoritative(draft({ id: "moved-draft", conversationId: "destination", text: "stale source", revision: "2" }));
    expect(store.get("destination")?.content.text).toBe("authoritative destination");
  });

  it("installs and hydrates a newly created draft before later edits flush", async () => {
    const saved: DraftInput[] = [];
    const store = new DraftStore(async input => {
      saved.push(input);
      return draft({ ...input, id: "created", conversationId: "conversation-created", revision: String(saved.length) });
    }, events);

    const { key, saved: created } = store.createNew("+15550001");
    await expect(created).resolves.toEqual({ ok: true });
    await expect(store.edit(key, { text: "after creation" })).resolves.toEqual({ ok: true });
    await expect(store.flush(key)).resolves.toEqual({ ok: true });

    expect(store.isHydrated(key)).toBe(true);
    expect(saved.at(-1)).toMatchObject({ conversationId: "conversation-created", id: "created", text: "after creation" });
  });

  it("keeps retained copy text after later edits clear undo", async () => {
    const remote = draft({ text: "remote", recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async () => { throw { code: "stale-draft", message: "stale" }; }, events, async () => remote);
    store.adopt("conversation-one", draft({ text: "original" }));

    await store.edit("conversation-one", { text: "unique local copy" });
    await store.edit("conversation-one", { text: "later edit" });

    expect(store.retainedText("conversation-one")).toBe("unique local copy");
  });

  it("does not mutate the moved source identity when a dirty destination collides", async () => {
    const destination = draft({ id: "destination", conversationId: "conversation-two", text: "host", revision: "2" });
    const store = new DraftStore(async input => {
      if (input.conversationId === "conversation-one") throw { code: "draft-moved", details: { conversationId: "conversation-two" }, message: "moved" };
      return draft(input);
    }, events, async () => destination);
    store.adopt("conversation-two", draft({ id: "destination", conversationId: "conversation-two", text: "destination" }));
    store.hold("conversation-two");
    await store.edit("conversation-two", { text: "unsaved destination" });
    store.adopt("conversation-one", draft({ text: "source" }));

    await expect(store.edit("conversation-one", { text: "source unsaved" })).resolves.toMatchObject({ reviewRequired: true });
    expect(store.get("conversation-one")).toMatchObject({ conversationId: "conversation-one", id: "draft-one" });
    expect(store.get("conversation-two")?.content.text).toBe("unsaved destination");
  });

  it("clears sent content before missing-revision verification and never resends it after failure", async () => {
    let loads = 0;
    const writes: DraftInput[] = [];
    const store = new DraftStore(async input => {
      writes.push(input);
      return draft({ ...input, revision: "3" });
    }, events, async () => {
      loads += 1;
      if (loads === 1) throw { message: "offline" };
      return draft({ text: "", attachmentIds: [], revision: "2" });
    });
    store.adopt("conversation-one", draft({ text: "sent prefix", attachmentIds: ["sent-file"] }));

    store.markSent("conversation-one", 0);
    await vi.waitFor(() => expect(store.saveState("conversation-one")).toBe("failed"));
    expect(store.get("conversation-one")?.content).toMatchObject({ text: "", attachmentIds: [] });
    await store.edit("conversation-one", { text: "new text", attachmentIds: ["new-file"] });
    await expect(store.retry("conversation-one")).resolves.toEqual({ ok: true });

    expect(writes).toEqual([expect.objectContaining({ text: "new text", attachmentIds: ["new-file"] })]);
  });

  it("settles missing sent verification without retrying a deleted host row", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const store = new DraftStore(persist, events, async () => undefined);
    store.adopt("conversation-one", draft({ text: "sent" }));

    store.markSent("conversation-one", 0, undefined, { text: "next" });
    await vi.waitFor(() => expect(store.saveState("conversation-one")).toBe("failed"));
    await expect(store.edit("conversation-one", { text: "later" })).resolves.toMatchObject({ ok: false });
    await expect(store.retry("conversation-one")).resolves.toMatchObject({ ok: false });
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false });

    expect(store.get("conversation-one")).toMatchObject({ held: false, content: { text: "later" } });
    expect(store.retainedText("conversation-one")).toContain("next");
    expect(persist).not.toHaveBeenCalled();
  });

  it("adopts a different verified sent row for review and retains next-message text", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const replacement = draft({ id: "replacement", text: "host", revision: "3" });
    const store = new DraftStore(persist, events, async () => replacement);
    store.adopt("conversation-one", draft({ text: "sent" }));

    store.markSent("conversation-one", 0, undefined, { text: "next" });
    await vi.waitFor(() => expect(store.get("conversation-one")?.id).toBe("replacement"));

    expect(store.get("conversation-one")).toMatchObject({ held: false, content: { text: "host" } });
    expect(store.retainedText("conversation-one")).toBe("next");
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false, reviewRequired: true });
    expect(persist).not.toHaveBeenCalled();
  });

  it("returns a real save failure from flushAll after an earlier review requirement", async () => {
    const remote = draft({ recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async input => {
      if (input.conversationId === "conversation-two") throw { message: "disk full", code: "io" };
      throw { code: "stale-draft", message: "stale" };
    }, events, async () => remote);
    store.adopt("conversation-one", draft());
    store.adopt("conversation-two", draft({ id: "two", conversationId: "conversation-two" }));
    void store.edit("conversation-one", { text: "review" });
    void store.edit("conversation-two", { text: "failure" });

    await expect(store.flushAll()).resolves.toEqual({ ok: false, error: "disk full", key: "conversation-two" });
  });

  it("attributes a flushAll failure to the offscreen draft key that failed", async () => {
    const store = new DraftStore(async input => {
      if (input.conversationId === "conversation-two") throw { message: "disk full", code: "io" };
      return draft(input);
    }, events);
    store.adopt("conversation-one", draft({ id: "one", conversationId: "conversation-one", text: "" }));
    store.adopt("conversation-two", draft({ id: "two", conversationId: "conversation-two", text: "" }));
    await expect(store.edit("conversation-one", { text: "saved" })).resolves.toEqual({ ok: true });
    void store.edit("conversation-two", { text: "failing" });

    await expect(store.flushAll()).resolves.toEqual({ ok: false, error: "disk full", key: "conversation-two" });
  });

  it("reports a real disk failure from a dirty destination collision ahead of review", async () => {
    const destination = draft({ id: "destination", conversationId: "conversation-two", text: "host", revision: "2" });
    const store = new DraftStore(async input => {
      if (input.conversationId === "conversation-one")
        throw { code: "draft-moved", details: { conversationId: "conversation-two" }, message: "moved" };
      if (input.text === "disk fail") throw { code: "io", message: "disk full" };
      return draft(input);
    }, events, async () => destination);
    store.adopt("conversation-two", draft({ id: "destination", conversationId: "conversation-two", text: "destination" }));
    await expect(store.edit("conversation-two", { text: "disk fail" })).resolves.toEqual({ ok: false, error: "disk full" });
    store.adopt("conversation-one", draft({ text: "source" }));
    await expect(store.edit("conversation-one", { text: "source unsaved" })).resolves.toMatchObject({ reviewRequired: true });

    await expect(store.flush("conversation-two")).resolves.toEqual({ ok: false, error: "disk full" });
    await expect(store.flushAll()).resolves.toEqual({ ok: false, error: "disk full", key: "conversation-two" });
  });

  it("blocks flushAll with a non-review failure for a dirty first-hydration conflict and still attempts every slot", async () => {
    const writes: string[] = [];
    const store = new DraftStore(async input => {
      writes.push(input.conversationId);
      if (input.conversationId === "conversation-two" && input.text === "disk fail") throw { code: "io", message: "disk full" };
      return draft(input);
    }, events);
    await store.edit("conversation-one", { text: "local first", attachmentIds: ["local"] });
    store.adopt("conversation-two", draft({ id: "two", conversationId: "conversation-two", text: "two" }));
    await expect(store.edit("conversation-two", { text: "disk fail" })).resolves.toEqual({ ok: false, error: "disk full" });
    store.adopt("conversation-one", draft({ text: "stored first", attachmentIds: ["stored"] }));
    writes.length = 0;

    await expect(store.flush("conversation-one")).resolves.toEqual({
      ok: false,
      error: "A stored draft conflicts with local edits. Review both versions before saving.",
    });
    await expect(store.flushAll()).resolves.toEqual({
      ok: false,
      error: "A stored draft conflicts with local edits. Review both versions before saving.",
      key: "conversation-one",
    });
    expect(writes).toEqual(["conversation-two"]);
  });

  it("creates without a moved notice when the host normalizes the entered recipient", async () => {
    const notices: string[] = [];
    const store = new DraftStore(async input => draft({
      ...input,
      id: "created",
      conversationId: "conversation-created",
      recipientIds: ["+18015551234"],
      revision: "1",
    }), { ...events, notice: (_, event) => notices.push(event) });

    const { key, saved } = store.createNew("(801) 555-1234");
    await expect(saved).resolves.toEqual({ ok: true });

    expect(notices).not.toContain("moved");
    expect(store.isHydrated(key)).toBe(true);
    expect(store.get(key)).toMatchObject({ conversationId: "conversation-created", id: "created", revision: "1" });
    expect(store.hasUndo(key)).toBe(false);
  });

  it("allows a reviewed draft to flush after acknowledgement without another edit", async () => {
    const remote = draft({ recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async () => { throw { code: "stale-draft", message: "stale" }; }, events, async () => remote);
    store.adopt("conversation-one", draft());

    await store.edit("conversation-one", { text: "reviewed" });
    store.acknowledgeReview("conversation-one");

    await expect(store.flush("conversation-one")).resolves.toEqual({ ok: true });
  });

  it("does not apply a delayed undo load after reset", async () => {
    let resolveUndoLoad: ((value: Draft | undefined) => void) | undefined;
    const remote = draft({ text: "remote", revision: "2" });
    const store = new DraftStore(async input => draft(input), events, async () => new Promise(resolve => { resolveUndoLoad = resolve; }));
    store.adopt("conversation-one", draft({ text: "original" }));
    store.applyAuthoritative(remote);
    const undo = store.undo("conversation-one");
    await vi.waitFor(() => expect(resolveUndoLoad).toBeTypeOf("function"));
    store.reset();
    resolveUndoLoad!(remote);

    expect(await undo).toEqual({ ok: true });
    expect(store.get("conversation-one")).toBeUndefined();
  });

  it("reset clears plaintext and fences delayed save, load, and undo callbacks", async () => {
    let resolveSave: ((value: Draft) => void) | undefined;
    let resolveLoad: ((value: Draft | undefined) => void) | undefined;
    const store = new DraftStore(async input => new Promise(resolve => { resolveSave = resolve; }), events, async () => new Promise(resolve => { resolveLoad = resolve; }));
    store.adopt("conversation-one", draft({ text: "secret" }));
    const save = store.edit("conversation-one", { text: "secret edit" });
    await vi.waitFor(() => expect(resolveSave).toBeTypeOf("function"));
    store.markSent("conversation-one", 0);
    await vi.waitFor(() => expect(resolveLoad).toBeTypeOf("function"));
    store.reset();
    resolveSave!(draft({ text: "old completion", revision: "2" }));
    resolveLoad!(draft({ text: "old load", revision: "2" }));
    await save;

    expect(store.get("conversation-one")).toBeUndefined();
    expect(store.localDrafts()).toEqual([]);
  });

  it("adopts an existing host draft returned during creation and retains newer local typing", async () => {
    let release!: () => void;
    const existing = draft({ id: "existing", conversationId: "existing-conversation", text: "existing host", revision: "4" });
    const store = new DraftStore(async () => new Promise(resolve => { release = () => resolve(existing); }), events, async () => existing);
    const { key, saved } = store.createNew("+15550001");
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    const typed = store.edit(key, { text: "typed during creation" });
    release();

    await expect(saved).resolves.toMatchObject({ reviewRequired: true });
    await typed;
    expect(store.get(key)?.content.text).toBe("existing host");
    expect(store.retainedText(key)).toBe("typed during creation");
  });

  it("keeps a dirty destination unchanged when creation lands on its existing host draft", async () => {
    const existing = draft({ id: "existing", conversationId: "existing-conversation", text: "host", revision: "4" });
    const store = new DraftStore(async input => {
      if (!input.conversationId) return existing;
      return draft(input);
    }, events, async () => existing);
    store.adopt("existing-conversation", draft({ ...existing, text: "destination" }));
    store.hold("existing-conversation");
    await store.edit("existing-conversation", { text: "dirty destination" });

    const { key, saved } = store.createNew("+15550001");
    await expect(saved).resolves.toMatchObject({ reviewRequired: true });

    expect(store.get("existing-conversation")?.content.text).toBe("dirty destination");
    expect(store.get(key)).toMatchObject({ conversationId: "", id: "" });
    expect(store.retainedText("existing-conversation")).toBeUndefined();
  });

  it("retains every distinct moved copy after later edits without enabling undo", async () => {
    const hosts = [
      draft({ id: "destination-one", conversationId: "destination-one", text: "host one", revision: "2" }),
      draft({ id: "destination-two", conversationId: "destination-two", text: "host two", revision: "3" }),
    ];
    let index = 0;
    const store = new DraftStore(async () => { throw { code: "draft-moved", details: { conversationId: hosts[index].conversationId } }; }, events, async () => hosts[index++]);
    store.adopt("conversation-one", draft({ text: "start" }));

    await store.edit("conversation-one", { text: "first moved copy" });
    await store.edit("destination-one", { text: "second moved copy" });

    expect(store.hasUndo("destination-two")).toBe(false);
    expect(store.retainedText("destination-two")).toContain("first moved copy");
    expect(store.retainedText("destination-two")).toContain("second moved copy");
    await store.edit("destination-two", { text: "host edit" });
    expect(store.retainedText("destination-two")).toContain("first moved copy");
  });

  it("keeps a copy-only retained version after a successful-save move", async () => {
    let release!: () => void;
    let firstSave = true;
    const destination = draft({ id: "destination", conversationId: "conversation-two", text: "host", revision: "2" });
    const store = new DraftStore(async input => {
      if (!firstSave) return draft({ ...input, revision: "3" });
      firstSave = false;
      return new Promise(resolve => { release = () => resolve(destination); });
    }, events);
    store.adopt("conversation-one", draft({ text: "original" }));
    const first = store.edit("conversation-one", { text: "first local" });
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    const newer = store.edit("conversation-one", { text: "newer local" });
    release();

    await first;
    await newer;
    expect(store.hasUndo("conversation-two")).toBe(false);
    expect(store.retainedText("conversation-two")).toBe("newer local");
    await store.edit("conversation-two", { text: "host edit" });
    expect(store.retainedText("conversation-two")).toBe("newer local");
  });

  it("uses the verified host content as the base for post-send edits", async () => {
    const writes: DraftInput[] = [];
    const concurrent = draft({ text: "other window text", revision: "12" });
    const store = new DraftStore(async input => {
      writes.push(input);
      return draft({ ...input, revision: "13" });
    }, events, async () => concurrent);
    store.adopt("conversation-one", draft({ text: "sent", revision: "10" }));

    store.markSent("conversation-one", 0, undefined, { text: "post-send" });
    await vi.waitFor(() => expect(writes).toHaveLength(1));

    expect(writes[0]).toMatchObject({ expectedRevision: "12", text: "post-send" });
    expect(store.retainedText("conversation-one")).toBe("other window text");
  });

  it("adopts a verified buffered create after an empty hydration snapshot", async () => {
    const created = draft({ text: "created after snapshot", revision: "1" });
    const store = new DraftStore(async input => draft(input), events, async () => created);
    store.applyAuthoritative(created);
    store.adopt("conversation-one", undefined);

    await vi.waitFor(() => expect(store.get("conversation-one")).toMatchObject({ id: "draft-one", content: { text: "created after snapshot" } }));
  });

  it("emits remote-update for structural and same-conversation identity host changes", async () => {
    const notices: string[] = [];
    const structural = draft({ recipientIds: ["+15550002"], revision: "2" });
    const store = new DraftStore(async () => { throw { code: "stale-draft" }; }, { ...events, notice: (_, event) => notices.push(event) }, async () => structural);
    store.adopt("conversation-one", draft({ text: "base" }));
    await store.edit("conversation-one", { text: "local" });
    store.applyAuthoritative(draft({ id: "new-id", text: "identity host", revision: "3" }));

    expect(notices).toContain("remote-update");
    expect(notices).not.toContain("moved");
  });

  it("clears a clean failed save state on retry", async () => {
    const store = new DraftStore(async () => { throw { message: "offline" }; }, events);
    store.adopt("conversation-one", draft());
    await store.edit("conversation-one", { text: "failed" });
    store.get("conversation-one")!.savedGeneration = store.get("conversation-one")!.generation;

    await expect(store.retry("conversation-one")).resolves.toEqual({ ok: true });
    expect(store.saveState("conversation-one")).toBe("idle");
  });

  it("flushes slots after a real failure and still returns that failure", async () => {
    const writes: string[] = [];
    const store = new DraftStore(async input => {
      writes.push(input.conversationId);
      if (input.conversationId === "conversation-two") throw { message: "disk full" };
      return draft({ ...input, revision: "2" });
    }, events);
    for (const key of ["conversation-one", "conversation-two", "conversation-three"])
      store.adopt(key, draft({ conversationId: key, id: key, text: "" }));
    await store.edit("conversation-one", { text: "one" });
    void store.edit("conversation-two", { text: "two" });
    void store.edit("conversation-three", { text: "three" });

    await expect(store.flushAll()).resolves.toEqual({ ok: false, error: "disk full", key: "conversation-two" });
    expect(writes).toContain("conversation-three");
  });

  it("retains the stored first-hydration conflict without relying on legacy recovery", async () => {
    const store = new DraftStore(async input => draft(input), events);
    await store.edit("conversation-one", { text: "local first", attachmentIds: ["local"] });
    store.adopt("conversation-one", draft({ text: "stored first", attachmentIds: ["stored"] }));

    expect(store.retainedText("conversation-one")).toBe("stored first");
    const outcome = await store.flush("conversation-one");
    expect(outcome).toEqual({ ok: false, error: "A stored draft conflicts with local edits. Review both versions before saving." });
    expect(outcome).not.toHaveProperty("reviewRequired");
  });
});

describe("DraftStore stale-send review reconciliation", () => {
  const content = (text: string, recipientIds = ["+15550001"]) => ({ text, recipientIds, attachmentIds: [] });

  it("keeps the original unsent body and next message as copy-only retained text", () => {
    const store = new DraftStore(async input => draft(input), events);
    store.adopt("conversation-one", draft({ text: "current" }));

    store.reconcileForReview("conversation-one", draft({ text: "host", revision: "2" }), [content("original unsent"), content("next message")]);

    expect(store.retainedTexts("conversation-one")).toEqual(["original unsent", "next message"]);
    expect(store.hasUndo("conversation-one")).toBe(false);
  });

  it("adopts the authoritative host body and recipients without any send or write", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const loadDraft = vi.fn(async () => undefined);
    const store = new DraftStore(persist, events, loadDraft);
    store.adopt("conversation-one", draft({ text: "local stale", recipientIds: ["+15550001"] }));
    const host = draft({ text: "host body", recipientIds: ["+15550002"], revision: "5" });

    store.reconcileForReview("conversation-one", host, [content("local stale")]);

    expect(store.get("conversation-one")).toMatchObject({ id: "draft-one", revision: "5", content: { text: "host body", recipientIds: ["+15550002"] } });
    expect(persist).not.toHaveBeenCalled();
    expect(loadDraft).not.toHaveBeenCalled();
  });

  it("blocks flush with review until the reviewed host state is acknowledged", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const store = new DraftStore(persist, events);
    store.adopt("conversation-one", draft({ text: "local" }));

    const outcome = store.reconcileForReview("conversation-one", draft({ text: "host", revision: "2" }), []);
    expect(outcome).toEqual({ ok: false, error: "Draft changed in another window. Review the retained text before sending.", reviewRequired: true, key: "conversation-one" });
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false, reviewRequired: true });

    store.acknowledgeReview("conversation-one");
    await expect(store.flush("conversation-one")).resolves.toEqual({ ok: true });
    expect(persist).not.toHaveBeenCalled();
  });

  it("settles a held send and a pending sent verification so the reviewed host is not blocked", () => {
    const store = new DraftStore(async input => draft(input), events, async () => undefined);
    store.adopt("conversation-one", draft({ text: "sent" }));
    store.markSent("conversation-one", 0);
    expect(store.get("conversation-one")?.held).toBe(true);

    store.reconcileForReview("conversation-one", draft({ text: "host", revision: "3" }), [content("sent")]);

    expect(store.get("conversation-one")).toMatchObject({ held: false, content: { text: "host" } });
    expect(store.retainedTexts("conversation-one")).toEqual(["sent"]);
  });

  it("retains copies and fails not-found when the host is absent, without recreating the draft", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const store = new DraftStore(persist, events, async () => undefined);
    store.adopt("conversation-one", draft({ text: "local" }));
    store.hold("conversation-one");

    const outcome = store.reconcileForReview("conversation-one", undefined, [content("stale send"), content("next")]);

    expect(outcome).toEqual({ ok: false, error: "Draft no longer exists.", key: "conversation-one" });
    expect(store.get("conversation-one")).toMatchObject({ id: "draft-one", held: false, error: { code: "not-found" } });
    expect(store.retainedTexts("conversation-one")).toEqual(["stale send", "next"]);
    expect(store.saveState("conversation-one")).toBe("failed");
    await expect(store.flush("conversation-one")).resolves.toEqual({ ok: false, error: "Draft no longer exists." });
    expect(persist).not.toHaveBeenCalled();
  });

  it("keeps the dirty destination's own edits and retains the source copy there when adoption would collide", async () => {
    const persist = vi.fn(async (input: DraftInput) => draft(input));
    const store = new DraftStore(persist, events);
    store.adopt("conversation-two", draft({ id: "destination", conversationId: "conversation-two", text: "destination" }));
    store.hold("conversation-two");
    await store.edit("conversation-two", { text: "destination dirty" });
    store.adopt("conversation-one", draft({ text: "source local" }));
    const writesBefore = persist.mock.calls.length;

    const outcome = store.reconcileForReview(
      "conversation-one",
      draft({ id: "destination", conversationId: "conversation-two", text: "host", revision: "2" }),
      [content("stale send")],
    );

    expect(outcome).toEqual({ ok: false, error: "Draft was moved to a conversation with unsaved edits. Your text is kept for copying.", key: "conversation-one" });
    expect(store.get("conversation-two")?.content.text).toBe("destination dirty");
    expect(store.retainedText("conversation-two")).toContain("source local");
    await expect(store.flush("conversation-one")).resolves.toMatchObject({ ok: false });
    expect(persist.mock.calls.length).toBe(writesBefore);
  });

  it("emits the affected key and changed event even when the key stays the same", () => {
    const changed = vi.fn();
    const notices: Array<[string, string]> = [];
    const store = new DraftStore(async input => draft(input), { changed, rekeyed: () => {}, notice: (key, event) => notices.push([key, event]) });
    store.adopt("conversation-one", draft({ text: "local" }));
    changed.mockClear();

    store.reconcileForReview("conversation-one", draft({ text: "host", revision: "2" }), []);

    expect(changed).toHaveBeenCalled();
    expect(notices).toEqual([["conversation-one", "remote-update"]]);
  });
});
