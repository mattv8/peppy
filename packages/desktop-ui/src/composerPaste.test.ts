import { describe, expect, it } from "vitest";
import { extractComposerPaste } from "./composerPaste";

function pasteData(files: File[], text = ""): DataTransfer {
  return { files, getData: (type: string) => type === "text/plain" ? text : "" } as unknown as DataTransfer;
}

function image(name: string, type = "image/png", size = 1): File {
  return new File([new Uint8Array(size)], name, { type });
}

describe("extractComposerPaste", () => {
  it("accepts supported images through the remaining slot limit", () => {
    const result = extractComposerPaste(pasteData([image("one.png"), image("two.webp", "image/webp")]), 1);
    expect(result.images.map((file) => file.name)).toEqual(["one.png"]);
    expect(result.rejected).toEqual([{ name: "two.webp", reason: "limit" }]);
  });

  it("rejects unsupported types and images over 32 MiB", () => {
    const result = extractComposerPaste(pasteData([image("one.bmp", "image/bmp"), image("two.png", "image/png", 33_554_433)]), 2);
    expect(result.rejected).toEqual([{ name: "one.bmp", reason: "type" }, { name: "two.png", reason: "size" }]);
  });

  it("keeps caption text once unless it duplicates an image filename or is a URL", () => {
    expect(extractComposerPaste(pasteData([image("one.png")], "caption"), 1).text).toBe("caption");
    expect(extractComposerPaste(pasteData([image("one.png")], "one.png"), 1).text).toBeNull();
    expect(extractComposerPaste(pasteData([image("one.png")], "https://peppy.example/image"), 1).text).toBeNull();
    expect(extractComposerPaste(pasteData([image("one.png")], "https://example.com"), 1).text).toBeNull();
    expect(extractComposerPaste(pasteData([image("one.png")], "https://example.com\n"), 1).text).toBeNull();
    expect(extractComposerPaste(pasteData([image("one.png")], "Note: lunch"), 1).text).toBe("Note: lunch");
    expect(extractComposerPaste(pasteData([image("one.png")], "Re: lunch"), 1).text).toBe("Re: lunch");
  });
});
