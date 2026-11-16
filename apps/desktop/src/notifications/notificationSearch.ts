import type { MirroredNotification } from "../bridge/bridge";

export type TextSegment = { text: string; match: boolean };

export type NotificationLabels = {
  appName(n: MirroredNotification): string;
  sourceName(n: MirroredNotification): string;
  title(n: MirroredNotification): string;
};

const MAX_QUERY_CODE_POINTS = 128;

/** Trims, caps the input at 128 code points, lowercases, and splits on whitespace. */
export function notificationSearchTerms(query: string): string[] {
  const capped = Array.from(query.trim()).slice(0, MAX_QUERY_CODE_POINTS).join("");
  return capped.toLowerCase().split(/\s+/).filter(Boolean);
}

/** Keeps notifications where every term appears in the app, phone, title, or body. */
export function searchNotifications(
  list: MirroredNotification[],
  query: string,
  labels: NotificationLabels,
): MirroredNotification[] {
  const terms = notificationSearchTerms(query);
  if (terms.length === 0) return list;
  // Search and highlighting share one matcher so every result shows a highlight.
  const patterns = terms.map(term => termPattern(term, "iu"));
  return list.filter(notification => {
    const fields = [
      labels.appName(notification),
      labels.sourceName(notification),
      labels.title(notification),
      notification.text,
    ];
    return patterns.every(pattern => fields.some(field => pattern.test(field)));
  });
}

type Range = { start: number; end: number };

function termPattern(term: string, flags: string): RegExp {
  return new RegExp(escapeRegExp(term), flags);
}

function termRanges(text: string, terms: string[]): Range[] {
  return terms.flatMap(term => {
    const pattern = termPattern(term, "giu");
    return [...text.matchAll(pattern)].map(match => ({
      start: match.index,
      end: match.index + match[0].length,
    }));
  });
}

function mergeRanges(ranges: Range[]): Range[] {
  const sorted = [...ranges].sort((a, b) => a.start - b.start);
  return sorted.reduce<Range[]>((merged, range) => {
    const last = merged.at(-1);
    if (last && range.start <= last.end) {
      last.end = Math.max(last.end, range.end);
    } else {
      merged.push({ ...range });
    }
    return merged;
  }, []);
}

/** Splits text into matched and unmatched segments; overlapping or adjacent matches merge. */
export function highlightSegments(text: string, terms: string[]): TextSegment[] {
  if (!text) return [];
  const usable = terms.filter(Boolean);
  if (usable.length === 0) return [{ text, match: false }];
  const segments: TextSegment[] = [];
  let cursor = 0;
  for (const range of mergeRanges(termRanges(text, usable))) {
    if (range.start > cursor) segments.push({ text: text.slice(cursor, range.start), match: false });
    segments.push({ text: text.slice(range.start, range.end), match: true });
    cursor = range.end;
  }
  if (cursor < text.length) segments.push({ text: text.slice(cursor), match: false });
  return segments;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
