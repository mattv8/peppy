import { describe, expect, it, vi } from "vitest";
import { publishAttachment } from "./publication.js";

const id = "123e4567-e89b-12d3-a456-426614174000";

describe("public attachment publication", () => {
  it("does not create or upload a derivative without explicit confirmation", async () => {
    const query = vi.fn();
    const image = vi.fn();
    const upload = vi.fn();
    await expect(publishAttachment({ query, mutate: vi.fn(), publicImage: image }, { postPublicCopy: upload }, { id, confirmed: false })).rejects.toMatchObject({ code: "invalid-request" });
    expect(query).not.toHaveBeenCalled();
    expect(image).not.toHaveBeenCalled();
    expect(upload).not.toHaveBeenCalled();
  });

  it("uploads only the re-encoded derivative after resolving a private remote object", async () => {
    const derivative = new Uint8Array([4, 5, 6]);
    const query = vi.fn().mockResolvedValue({ remoteObjectId: "remote-object" });
    const image = vi.fn().mockResolvedValue({ bytes: derivative, name: "image.png", contentType: "image/png" });
    const upload = vi.fn().mockResolvedValue({ token: "share-token", safe_name: "image.png", expires_in_seconds: 300 });
    await expect(publishAttachment({ query, mutate: vi.fn(), publicImage: image }, { postPublicCopy: upload }, { id, confirmed: true })).resolves.toEqual({ url: "/file/mms-usercontent/share-token/image.png", expiresInSeconds: 300 });
    expect(upload).toHaveBeenCalledWith("/v1/attachments/remote-object/public-copies", derivative, { fileName: "image.png" });
  });

  it.each([
    undefined,
    null,
    {},
    { token: null, safe_name: "image.png" },
    { token: false, safe_name: "image.png" },
    { token: 1, safe_name: "image.png" },
    { token: "share-token", safe_name: null },
    { token: "share-token", safe_name: false },
    { token: "share-token", safe_name: 1 },
    { token: "", safe_name: "image.png" },
    { token: "share-token", safe_name: "" },
  ])("rejects malformed successful publication responses: %j", async response => {
    const session = { query: vi.fn(async () => ({ remoteObjectId: "remote-object" })), mutate: vi.fn(), publicImage: vi.fn(async () => ({ bytes: new Uint8Array([1]), name: "image.png" })) };
    const upload = { postPublicCopy: async <T>() => response as T };

    await expect(publishAttachment(session, upload, { id, confirmed: true })).rejects.toMatchObject({ kind: "invalid" });
  });

  it("checkpoints exact policy removal from publication but leaves generic 404 and 410 retryable", async () => {
    const mutate = vi.fn(async () => ({}));
    const session = { query: vi.fn(async () => ({ remoteObjectId: "remote-object" })), mutate, publicImage: vi.fn(async () => ({ bytes: new Uint8Array([1]), name: "image.png" })) };
    const exact = vi.fn(async () => { throw new (await import("./transport.js")).TransportError("status", 410, "attachment_removed_by_storage_policy"); });
    await expect(publishAttachment(session, { postPublicCopy: exact }, { id, confirmed: true })).rejects.toMatchObject({ code: "public-copy-unavailable" });
    expect(mutate).toHaveBeenCalledWith("_worker_attachment_retention_report_removed", { attachmentId: id });

    for (const error of [new (await import("./transport.js")).TransportError("status", 404), new (await import("./transport.js")).TransportError("status", 410, "gone")]) {
      mutate.mockClear();
      await expect(publishAttachment(session, { postPublicCopy: async () => { throw error; } }, { id, confirmed: true })).rejects.toBe(error);
      expect(mutate).not.toHaveBeenCalled();
    }
  });

});
