import { execFileSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { posixQuote } from "./shellQuote";

describe("posixQuote", () => {
  it.each(["https://mcp.example/$(echo MARK)", "https://x/`echo MARK`", "it's"])("quotes and round-trips %s", value => {
    const quoted = posixQuote(value);
    expect(quoted.startsWith("'") && quoted.endsWith("'")).toBe(true);
    expect(execFileSync("sh", ["-c", `printf %s ${quoted}`], { encoding: "utf8" })).toBe(value);
  });
});
