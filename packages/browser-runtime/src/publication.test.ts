import { describe, expect, it, vi } from "vitest";
import { publishAttachment } from "./publication.js";

const id = "123e4567-e89b-12d3-a456-426614174000";

describe("public attachment publication", () => {
  it("does not create or upload a derivative without explicit confirmation", async () => {
    const query = vi.fn();
    const image = vi.fn();
    const upload = vi.fn();
    await expect(publishAttachment({ query, publicImage: image }, { postPublicCopy: upload }, { id, confirmed: false })).rejects.toMatchObject({ code: "invalid-request" });
    expect(query).not.toHaveBeenCalled();
    expect(image).not.toHaveBeenCalled();
    expect(upload).not.toHaveBeenCalled();
  });

  it("uploads only the re-encoded derivative after resolving a private remote object", async () => {
    const derivative = new Uint8Array([4, 5, 6]);
    const query = vi.fn().mockResolvedValue({ remoteObjectId: "remote-object" });
    const image = vi.fn().mockResolvedValue({ bytes: derivative, name: "image.png", contentType: "image/png" });
    const upload = vi.fn().mockResolvedValue({ token: "share-token", safe_name: "image.png", expires_in_seconds: 300 });
    await expect(publishAttachment({ query, publicImage: image }, { postPublicCopy: upload }, { id, confirmed: true })).resolves.toEqual({ url: "/file/mms-usercontent/share-token/image.png", expiresInSeconds: 300 });
    expect(upload).toHaveBeenCalledWith("/v1/attachments/remote-object/public-copies", derivative, { fileName: "image.png" });
  });
});
