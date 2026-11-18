import { describe, expect, it } from "vitest";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { registerResendRows, resendChains } from "./resendChains";

const message = (id: string, changes: Partial<MessageView> = {}): MessageView => ({
  id, revision: "1", sender: "self", body: "same", timestamp: "", attachments: [], ...changes,
});

describe("resendChains", () => {
  it("groups an explicit adjacent three-attempt chain and classifies risk", () => {
    const a = message("a", { status: "failed-confirmed", sentAgain: { messageId: "b" } });
    const b = message("b", { status: "sent", sentAgain: { messageId: "c" } });
    const c = message("c");
    const result = resendChains([a, b, c], {});
    expect([...result.hidden]).toEqual(["a", "b"]);
    expect(result.byLatest.get("c")).toEqual({ attempts: [a, b, c], earlierRisk: "sent" });
    expect(resendChains([a, message("b")], {}).byLatest.get("b")?.earlierRisk).toBe("none");
    expect(resendChains([message("a", { sentAgain: { messageId: "b" } }), b], {}).byLatest.size).toBe(0);
  });

  it("does not infer links from text, skip intervening messages, or hide missing or cyclic chains", () => {
    const a = message("a", { sentAgain: { messageId: "b" } });
    const b = message("b");
    expect(resendChains([message("a"), b], {}).hidden.size).toBe(0);
    expect(resendChains([a, message("other", { sender: "other" }), b], {}).hidden.size).toBe(0);
    expect(resendChains([a], {}).hidden.size).toBe(0);
    expect(resendChains([a, message("b", { sentAgain: { messageId: "a" } })], {}).hidden.size).toBe(0);
  });

  it("honours transient links, exclusions and possible risk", () => {
    const a = message("a");
    const b = message("b");
    const options = { transientLink: (id: string) => id === "a" ? "b" : undefined };
    expect(resendChains([a, b], options).byLatest.get("b")?.earlierRisk).toBe("possible");
    expect(resendChains([a, b], { ...options, keepVisible: (item) => item.id === "a" }).hidden.size).toBe(0);
    expect(resendChains([message("a", { sender: "other" }), b], options).hidden.size).toBe(0);
    expect(resendChains([message("a", { sentAgain: { messageId: "missing" } }), b], options).hidden.size).toBe(0);
  });

  it("keeps a whole chain visible when a middle attempt must stay visible", () => {
    const a = message("a", { sentAgain: { messageId: "b" } });
    const b = message("b", { sentAgain: { messageId: "c" } });
    expect(resendChains([a, b, message("c")], { keepVisible: (item) => item.id === "b" }).hidden.size).toBe(0);
  });

  it("keeps the chain visible when its latest link is nonadjacent", () => {
    const a = message("a", { sentAgain: { messageId: "b" } });
    const b = message("b", { sentAgain: { messageId: "c" } });
    expect(resendChains([a, b, message("reply", { sender: "other" }), message("c")], {}).hidden.size).toBe(0);
  });

  it("registers hidden attempt ids against the latest row and clears them on unmount", () => {
    const a = message("a", { sentAgain: { messageId: "b" } });
    const b = message("b");
    const chain = resendChains([a, b], {}).byLatest.get("b");
    const row = document.createElement("article");
    const rows = new Map<string, HTMLElement>();
    const register = (id: string, element: HTMLElement | null) => { if (element) rows.set(id, element); else rows.delete(id); };
    registerResendRows(chain, row, register);
    expect(rows.get("a")).toBe(row);
    registerResendRows(chain, null, register);
    expect(rows.has("a")).toBe(false);
  });
});
