import vectors from "../../../../crates/desktop-api/tests/fixtures/mms_plan_vectors.json";
import { describe, expect, it } from "vitest";
import { planMms } from "./mmsPlan";

describe("planMms", () => {
  it.each(vectors.vectors)("matches the shared Rust vector: $name", (vector) => {
    const result = planMms({
      limitBytes: vector.limit,
      gatewayVersion: vector.version,
      text: vector.text,
      images: vector.images.map((image, index) => ({ id: `image-${index}`, bytes: image.bytes, mediaType: image.type })),
    });

    expect(result.kind).toBe(vector.expect.kind);
    expect(result.parts).toBe(vector.expect.parts ?? 0);
    if (result.kind === "split" && vector.expect.kind === "split") {
      expect(result.caption).toBe(vector.expect.caption);
      expect(result.groups.map((group) => group.length)).toEqual(vector.expect.groups);
    }
  });
});
