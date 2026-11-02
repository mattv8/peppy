import { describe, expect, it } from "vitest";
import { errorCode, errorText } from "./bridgeErrors";

describe("bridge errors", () => {
  it("uses native error details when they have the expected shape", () => {
    const error = { code: "stale-draft", message: "The draft changed elsewhere." };

    expect(errorText(error)).toBe("The draft changed elsewhere.");
    expect(errorCode(error)).toBe("stale-draft");
  });

  it("keeps the draft-safe fallback for malformed native errors", () => {
    expect(errorText({ message: "" })).toBe(
      "The native operation failed. Your edits remain in this window.",
    );
    expect(errorCode({ code: 42 })).toBeUndefined();
  });
});
