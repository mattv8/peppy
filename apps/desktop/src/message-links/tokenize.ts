export type MessageTextToken = MessageLinkToken | MessagePlainTextToken;

export type MessageLinkToken = {
  kind: "link";
  text: string;
  href: string;
};

export type MessagePlainTextToken = {
  kind: "text";
  text: string;
};

const MAX_CANDIDATE_LENGTH = 4096;
const MAX_PREFIX_LENGTH = 4096;
const MAX_LINKIFIED_TEXT_LENGTH = 64 * 1024;
const MAX_LINK_TOKENS = 256;
const MAX_TRACKED_SCHEME_LENGTH = 16;
const UNSAFE_CHARACTER = /[\p{Cc}\p{Cf}]/u;
const WORD_OR_ADDRESS_CHARACTER = /[\p{L}\p{N}_.%+@-]/u;
const UNSAFE_SCHEMES = new Set(["data", "file", "ftp", "javascript", "mailto", "vbscript"]);
const TRAILING_PUNCTUATION = new Set([".", ",", "!", "?", ";", ":", "…", "。", "、", "，", "！", "？", "：", "；"]);
const CANDIDATE_DELIMITERS = new Set(["<", ">", "\"", "'", "`", "“", "”", "‘", "’", "«", "»", "‹", "›", "「", "」", "『", "』", "。", "．", "、", "，", "！", "？", "：", "；"]);
const CLOSING_DELIMITERS = new Map([[")", "("], ["]", "["], ["}", "{"], ["）", "（"], ["］", "［"], ["】", "【"], ["〉", "〈"], ["》", "《"]]);
const DOT_LIKE_DELIMITERS = new Set(["。", "．"]);

type PrefixState = {
  runLength: number;
  hasAt: boolean;
  hasUnsafeScheme: boolean;
  hasSchemeSlashes: boolean;
  schemeName?: string;
  schemeNameTooLong: boolean;
  pendingSchemeSlashes: 0 | 1 | 2;
  previousWasSchemeCharacter: boolean;
  previousCharacter?: string;
};

type Candidate = {
  text: string;
  end: number;
  tooLong: boolean;
  invalidAuthority: boolean;
  truncated: boolean;
};

export function tokenizeMessageText(text: string): MessageTextToken[] {
  const tokens: MessageTextToken[] = [];
  const prefixState = createPrefixState();
  const linkifyEnd = boundedLinkifyEnd(text);
  let cursor = 0;
  let textStart = 0;
  let linkCount = 0;

  while (cursor < linkifyEnd && linkCount < MAX_LINK_TOKENS) {
    if (!startsLink(text, cursor) || !isStandaloneLink(prefixState) || isUnsafePrefix(prefixState)) {
      cursor = consumeCharacter(text, cursor, prefixState);
      continue;
    }

    const candidate = readCandidate(text, cursor, linkifyEnd, prefixState);
    const link = !candidate.tooLong
      && !candidate.invalidAuthority
      && !candidate.truncated
      && normalizeLink(trimTrailingPunctuation(candidate.text));

    if (link) {
      appendTextToken(tokens, text.slice(textStart, cursor));
      tokens.push(link);
      linkCount += 1;
      textStart = cursor + link.text.length;
    }

    cursor = candidate.end;
  }

  appendTextToken(tokens, text.slice(textStart));
  return tokens;
}

function createPrefixState(): PrefixState {
  return {
    runLength: 0,
    hasAt: false,
    hasUnsafeScheme: false,
    hasSchemeSlashes: false,
    schemeNameTooLong: false,
    pendingSchemeSlashes: 0,
    previousWasSchemeCharacter: false,
  };
}

function boundedLinkifyEnd(text: string): number {
  let end = Math.min(text.length, MAX_LINKIFIED_TEXT_LENGTH);
  if (end < text.length && end > 0 && isHighSurrogate(text.charCodeAt(end - 1)) && isLowSurrogate(text.charCodeAt(end))) {
    end -= 1;
  }
  return end;
}

function startsLink(text: string, start: number): boolean {
  const prefix = text.slice(start, start + 8).toLowerCase();
  return prefix.startsWith("http://") || prefix.startsWith("https://") || prefix.startsWith("www.");
}

function isStandaloneLink(state: PrefixState): boolean {
  return state.previousCharacter === undefined || !WORD_OR_ADDRESS_CHARACTER.test(state.previousCharacter);
}

function isUnsafePrefix(state: PrefixState): boolean {
  return state.runLength > MAX_PREFIX_LENGTH || state.hasAt || state.hasUnsafeScheme || state.hasSchemeSlashes;
}

function readCandidate(text: string, start: number, limit: number, state: PrefixState): Candidate {
  let end = start;
  let characterCount = 0;
  let tooLong = false;

  while (end < limit) {
    const character = characterAt(text, end);
    if (isCandidateDelimiter(character)) break;

    advancePrefixState(state, character);
    end += character.length;
    characterCount += 1;
    if (characterCount > MAX_CANDIDATE_LENGTH) tooLong = true;
  }

  const candidate = text.slice(start, end);
  const delimiter = end < text.length ? characterAt(text, end) : undefined;
  return {
    text: candidate,
    end,
    tooLong,
    invalidAuthority: delimiter !== undefined && DOT_LIKE_DELIMITERS.has(delimiter) && isInAuthority(candidate) && isHostLabelCharacter(nextCharacter(text, end)),
    truncated: end === limit && limit < text.length && delimiter !== undefined && !isCandidateDelimiter(delimiter),
  };
}

function consumeCharacter(text: string, index: number, state: PrefixState): number {
  const character = characterAt(text, index);
  advancePrefixState(state, character);
  return index + character.length;
}

function advancePrefixState(state: PrefixState, character: string): void {
  if (/\s/u.test(character)) {
    resetPrefixState(state, character);
    return;
  }

  state.runLength += 1;
  state.previousCharacter = character;
  if (character === "@") state.hasAt = true;

  if (state.pendingSchemeSlashes === 1) {
    state.pendingSchemeSlashes = character === "/" ? 2 : 0;
    if (character === "/") return;
  } else if (state.pendingSchemeSlashes === 2) {
    state.pendingSchemeSlashes = 0;
    if (character === "/") {
      state.hasSchemeSlashes = true;
      return;
    }
  }

  if (isAsciiSchemeCharacter(character)) {
    if (state.schemeName !== undefined) {
      if (state.schemeName.length < MAX_TRACKED_SCHEME_LENGTH) state.schemeName += character.toLowerCase();
      else state.schemeNameTooLong = true;
    } else if (!state.previousWasSchemeCharacter && isAsciiLetter(character)) {
      state.schemeName = character.toLowerCase();
      state.schemeNameTooLong = false;
    }
    state.previousWasSchemeCharacter = true;
    return;
  }

  if (character === ":" && state.schemeName !== undefined) {
    if (!state.schemeNameTooLong && UNSAFE_SCHEMES.has(state.schemeName)) state.hasUnsafeScheme = true;
    state.pendingSchemeSlashes = 1;
  }

  state.schemeName = undefined;
  state.schemeNameTooLong = false;
  state.previousWasSchemeCharacter = false;
}

function resetPrefixState(state: PrefixState, whitespace: string): void {
  state.runLength = 0;
  state.hasAt = false;
  state.hasUnsafeScheme = false;
  state.hasSchemeSlashes = false;
  state.schemeName = undefined;
  state.schemeNameTooLong = false;
  state.pendingSchemeSlashes = 0;
  state.previousWasSchemeCharacter = false;
  state.previousCharacter = whitespace;
}

function isCandidateDelimiter(character: string): boolean {
  return /\s/u.test(character) || CANDIDATE_DELIMITERS.has(character);
}

function isAsciiLetter(character: string): boolean {
  return /^[A-Za-z]$/u.test(character);
}

function isAsciiSchemeCharacter(character: string): boolean {
  return /^[A-Za-z0-9+.-]$/u.test(character);
}

function isHighSurrogate(value: number): boolean {
  return value >= 0xD800 && value <= 0xDBFF;
}

function isLowSurrogate(value: number): boolean {
  return value >= 0xDC00 && value <= 0xDFFF;
}

function characterAt(text: string, index: number): string {
  return String.fromCodePoint(text.codePointAt(index)!);
}

function normalizeLink(text: string): MessageLinkToken | undefined {
  if (!text || text.includes("\\") || UNSAFE_CHARACTER.test(text)) return undefined;

  const source = text.toLowerCase().startsWith("www.") ? `https://${text}` : text;
  const rawAuthority = authorityFrom(source);
  if (!rawAuthority || !isSafeRawAuthority(rawAuthority)) return undefined;

  try {
    const url = new URL(source);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    if (url.username || url.password || !url.hostname) return undefined;
    if (!isCanonicalNumericAuthority(rawAuthority.host, url.hostname)) return undefined;

    return { kind: "link", text, href: url.href };
  } catch {
    return undefined;
  }
}

function authorityFrom(source: string): { host: string; port?: string } | undefined {
  const schemeEnd = source.indexOf("://");
  if (schemeEnd < 0) return undefined;

  const authorityEnd = source.slice(schemeEnd + 3).search(/[/?#]/u);
  const authority = source.slice(schemeEnd + 3, authorityEnd < 0 ? undefined : schemeEnd + 3 + authorityEnd);
  if (!authority || authority.includes("@")) return undefined;

  if (authority.startsWith("[")) {
    const closingBracket = authority.indexOf("]");
    const host = authority.slice(0, closingBracket + 1);
    const port = authority.slice(closingBracket + 1);
    if (closingBracket < 0 || (port && !/^:\d+$/u.test(port))) return undefined;
    return { host, port: port.slice(1) || undefined };
  }

  const colon = authority.lastIndexOf(":");
  const host = colon < 0 ? authority : authority.slice(0, colon);
  const port = colon < 0 ? undefined : authority.slice(colon + 1);
  if (colon >= 0 && (!port || !/^\d+$/u.test(port) || authority.indexOf(":") !== colon)) return undefined;
  return { host, port };
}

function isSafeRawAuthority(authority: { host: string }): boolean {
  if (!authority.host || UNSAFE_CHARACTER.test(authority.host)) return false;
  if (authority.host.startsWith("[")) return /^\[[\dA-Fa-f:.]+\]$/u.test(authority.host);

  if (authority.host === "localhost") return true;
  if (!authority.host.includes(".")) return false;

  return authority.host.split(".").every(isSafeHostLabel);
}

function isCanonicalNumericAuthority(rawHost: string, normalizedHost: string): boolean {
  if (!isCanonicalIpv4(normalizedHost)) return true;
  return rawHost === normalizedHost;
}

function isCanonicalIpv4(host: string): boolean {
  const labels = host.split(".");
  return labels.length === 4 && labels.every(label => /^(0|[1-9]\d{0,2})$/u.test(label) && Number(label) <= 255);
}

function isSafeHostLabel(label: string): boolean {
  return /^[\p{L}\p{M}\p{N}-]+$/u.test(label) && !label.startsWith("-") && !label.endsWith("-");
}

function trimTrailingPunctuation(candidate: string): string {
  const characters = [...candidate];
  const delimiterCounts = new Map<string, number>();

  for (const character of characters) {
    if (CLOSING_DELIMITERS.has(character) || [...CLOSING_DELIMITERS.values()].includes(character)) {
      delimiterCounts.set(character, (delimiterCounts.get(character) ?? 0) + 1);
    }
  }

  while (characters.length > 0) {
    const character = characters.at(-1)!;
    const openingDelimiter = CLOSING_DELIMITERS.get(character);
    if (TRAILING_PUNCTUATION.has(character)) {
      characters.pop();
      continue;
    }
    if (!openingDelimiter || (delimiterCounts.get(character) ?? 0) <= (delimiterCounts.get(openingDelimiter) ?? 0)) break;

    delimiterCounts.set(character, (delimiterCounts.get(character) ?? 1) - 1);
    characters.pop();
  }

  return characters.join("");
}

function isInAuthority(candidate: string): boolean {
  const schemeEnd = candidate.indexOf("://");
  return schemeEnd >= 0 && !/[/?#]/u.test(candidate.slice(schemeEnd + 3));
}

function isHostLabelCharacter(character: string | undefined): boolean {
  return character !== undefined && /^[\p{L}\p{M}\p{N}-]$/u.test(character);
}

function nextCharacter(text: string, index: number): string | undefined {
  return index < text.length ? String.fromCodePoint(text.codePointAt(index)!) : undefined;
}

function appendTextToken(tokens: MessageTextToken[], text: string): void {
  if (text) tokens.push({ kind: "text", text });
}
