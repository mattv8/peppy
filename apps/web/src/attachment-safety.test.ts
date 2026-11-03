import { describe, expect, it } from "vitest";
import { canInlineMedia, inlineMediaType, safeSuggestedFilename } from "./attachment-safety";

describe("browser attachment safety", () => {
  it("allows only desktop-supported raster and media types inline", () => {
    ["image/jpeg", "image/png", "image/gif", "image/webp", "video/mp4", "video/3gpp", "audio/mpeg", "audio/amr"].forEach(mediaType => {
      expect(canInlineMedia(mediaType)).toBe(true);
      expect(inlineMediaType(mediaType.toUpperCase())).toBe(mediaType);
    });
    expect(canInlineMedia("image/svg+xml")).toBe(false);
    expect(canInlineMedia("text/html")).toBe(false);
    expect(canInlineMedia("application/xml")).toBe(false);
    expect(canInlineMedia("text/javascript")).toBe(false);
  });

  it("uses a desktop-equivalent safe download name without removing emoji", () => {
    expect(safeSuggestedFilename("../invoice\u202efdp.exe")).toBe("..invoicefdp.exe");
    expect(safeSuggestedFilename("my.photo.jpg")).toBe("my.photo.jpg");
    expect(safeSuggestedFilename("notes.md")).toBe("notes.md");
    expect(safeSuggestedFilename("Meeting 10:30.txt")).toBe("Meeting 10:30.txt");
    expect(safeSuggestedFilename(`${"a".repeat(200)}.pdf`)).toBe(`${"a".repeat(124)}.pdf`);
    expect(safeSuggestedFilename("📎 report.txt")).toBe("📎 report.txt");
  });
});
