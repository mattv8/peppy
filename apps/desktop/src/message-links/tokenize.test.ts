import { describe, expect, it } from "vitest";
import { tokenizeMessageText } from "./tokenize";

const text = (value: string) => tokenizeMessageText(value).map(token => token.text).join("");

describe("tokenizeMessageText", () => {
  it("preserves every character while recognizing standalone HTTP links", () => {
    const value = "Read https://example.com/a?b=1 and www.example.org.";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([
      { kind: "text", text: "Read " },
      { kind: "link", text: "https://example.com/a?b=1", href: "https://example.com/a?b=1" },
      { kind: "text", text: " and " },
      { kind: "link", text: "www.example.org", href: "https://www.example.org/" },
      { kind: "text", text: "." },
    ]);
  });

  it("keeps punctuation outside links except balanced parentheses", () => {
    const value = "See (https://example.com/a_(b)). Then https://example.com/a).";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com/a_(b)", href: "https://example.com/a_(b)" },
      { kind: "link", text: "https://example.com/a", href: "https://example.com/a" },
    ]);
  });

  it("does not link unsafe, malformed, credentialed, or email-embedded candidates", () => {
    const value = "javascript:alert(1) https://user:pass@example.com https://example bad@www.example.com https://exa\u0000mple.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([{ kind: "text", text: value }]);
  });

  it("preserves multiple lines and exposes an ASCII destination for Unicode domains", () => {
    const value = "First https://例え.テスト/path\nSecond HTTPS://EXAMPLE.COM";
    const links = tokenizeMessageText(value).filter(token => token.kind === "link");

    expect(text(value)).toBe(value);
    expect(links).toEqual([
      { kind: "link", text: "https://例え.テスト/path", href: "https://xn--r8jz45g.xn--zckzah/path" },
      { kind: "link", text: "HTTPS://EXAMPLE.COM", href: "https://example.com/" },
    ]);
  });

  it("keeps quotes and script-looking text as literal text", () => {
    const value = "'<script>https://example.com</script>' <img src=x>";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([
      { kind: "text", text: "'<script>" },
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "text", text: "</script>' <img src=x>" },
    ]);
  });

  it("treats curly quotes and fullwidth punctuation as delimiters instead of IDN authority characters", () => {
    const value = "“https://example.com” https://example。com https://example.com！";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
    ]);
  });

  it("rejects unsafe-scheme prefixes and invisible Unicode controls", () => {
    const value = "javascript://www.evil.com ftp://www.example.com data:text/html,https://evil.com javascript:https://evil.com mailto:x@y.com?body=https://evil.com https://exa\u202Emple.com https://exa\u200Bmple.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([{ kind: "text", text: value }]);
  });

  it("rejects unsafe schemes encountered earlier in the same non-whitespace token", () => {
    const value = "javascript:foo:https://evil.com data:text/html;x:https://evil.com custom://x:https://evil.com safe:https://example.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
    ]);
  });

  it("linkifies only the first 64 KiB and leaves the remainder in one plain token", () => {
    const prefix = `https://example.com ${"x".repeat(65_536)}`;
    const value = `${prefix} https://late.example.com`;
    const tokens = tokenizeMessageText(value);

    expect(text(value)).toBe(value);
    expect(tokens.filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
    ]);
    expect(tokens.at(-1)).toEqual({ kind: "text", text: value.slice("https://example.com".length) });
  });

  it("caps generated link tokens and keeps the remaining source in one plain token", () => {
    const value = "https://a.co ".repeat(1_000);
    const tokens = tokenizeMessageText(value);

    expect(text(value)).toBe(value);
    expect(tokens.filter(token => token.kind === "link")).toHaveLength(256);
    expect(tokens.length).toBeLessThanOrEqual(513);
    expect(tokens.at(-1)?.kind).toBe("text");
  });

  it("preserves lone surrogates around candidate boundaries", () => {
    const value = "\uD800https://example.com \uDC00www.example.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "www.example.com", href: "https://www.example.com/" },
    ]);
  });

  it("allows canonical explicitly clicked local and numeric authorities while rejecting ambiguous numeric forms", () => {
    const value = "http://localhost:3000 http://127.0.0.1 http://[::1] http://2130706433 http://0177.0.0.1 https://example..com https://@example.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "http://localhost:3000", href: "http://localhost:3000/" },
      { kind: "link", text: "http://127.0.0.1", href: "http://127.0.0.1/" },
      { kind: "link", text: "http://[::1]", href: "http://[::1]/" },
    ]);
  });

  it("leaves overlong candidates as text without repeatedly trimming brackets", () => {
    const value = `https://a.co/${")".repeat(4097)}`;

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([{ kind: "text", text: value }]);
  });

  it("keeps a long run with many link starts bounded and plain", () => {
    const value = '("www.'.repeat(20_000);

    expect(tokenizeMessageText(value)).toEqual([{ kind: "text", text: value }]);
  });

  it("keeps common CJK enclosures outside a valid destination", () => {
    const value = "【https://example.com】 https://example.com〉 https://example.com》 https://example.com。 https://example。com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
    ]);
  });

  it("rejects punctuation, backslashes, and numeric authorities rewritten by URL parsing", () => {
    const value = "http://0x7f.0.0.1 http://1.0x0.0.1 http://2130706433\\.x";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value)).toEqual([{ kind: "text", text: value }]);
  });

  it("links natural labels without allowing unsafe embedded schemes", () => {
    const value = "Link:https://example.com URL:www.example.com javascript:https://evil.com ftp://www.evil.com";

    expect(text(value)).toBe(value);
    expect(tokenizeMessageText(value).filter(token => token.kind === "link")).toEqual([
      { kind: "link", text: "https://example.com", href: "https://example.com/" },
      { kind: "link", text: "www.example.com", href: "https://www.example.com/" },
    ]);
  });
});
