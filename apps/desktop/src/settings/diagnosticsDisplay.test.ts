import { describe, expect, it } from "vitest";
import { diagnosticsUsagePercent, formatDiagnosticsSize, recordingText } from "./diagnosticsDisplay";

describe("recordingText", () => {
  it.each([
    [{ enabled: true, retainedBytes: 0 }, "Active"],
    [{ enabled: true, retainedBytes: 4096 }, "Active"],
    [{ enabled: false, retainedBytes: 4096 }, "Off. Retained logs are still available."],
    [{ enabled: false, retainedBytes: 0 }, "Off"],
  ])("describes %o as %s", (input, text) => {
    expect(recordingText(input)).toBe(text);
  });
});

describe("formatDiagnosticsSize", () => {
  it.each([
    [0, "0 B"],
    [1023, "1023 B"],
    [1024, "1 KiB"],
    [1536, "1 KiB"],
    [1048575, "1023 KiB"],
    [1048576, "1 MiB"],
    [1572864, "1.5 MiB"],
    [4194304, "4 MiB"],
  ])("formats %i bytes as %s", (bytes, text) => {
    expect(formatDiagnosticsSize(bytes)).toBe(text);
  });
});

describe("diagnosticsUsagePercent", () => {
  it("returns the share of the limit in use", () => {
    expect(diagnosticsUsagePercent(1572864, 4194304)).toBe(37.5);
  });

  it("clamps to the 0 through 100 range", () => {
    expect(diagnosticsUsagePercent(-1, 4194304)).toBe(0);
    expect(diagnosticsUsagePercent(5_000_000, 4194304)).toBe(100);
  });

  it("returns 0 when the limit is not positive", () => {
    expect(diagnosticsUsagePercent(1024, 0)).toBe(0);
  });
});
