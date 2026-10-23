export type DateTimeInput = number | string | Date | null | undefined;

export type DateTimeFormatOptions = {
  locale?: string;
  timeZone?: string;
};

const FORMATTER_CACHE_LIMIT = 32;
const MINUTES_IN_HOUR = 60;
const MINUTES_IN_DAY = 1_440;
const MILLISECONDS_IN_MINUTE = 60_000;
const MAX_DATE_TIMESTAMP = 8.64e15;
const NUMERIC_LIKE = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/;
const TIME_ONLY = /^\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?\s*(?:[ap]m)?$/i;
const ISO_DATE = /^(\d{4})-(\d{2})-(\d{2})(?:T|$)/;

const dateTimeFormatters = new Map<string, Intl.DateTimeFormat>();
const relativeTimeFormatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

function validTimestamp(timestamp: number): number | undefined {
  return Number.isFinite(timestamp) && timestamp > 0 && timestamp <= MAX_DATE_TIMESTAMP
    ? timestamp
    : undefined;
}

function hasValidIsoDate(value: string): boolean {
  const match = value.match(ISO_DATE);
  if (!match) return true;
  const [, year, month, day] = match;
  const date = new Date(Date.UTC(Number(year), Number(month) - 1, Number(day)));
  return date.getUTCFullYear() === Number(year)
    && date.getUTCMonth() === Number(month) - 1
    && date.getUTCDate() === Number(day);
}

function parseString(value: string): number | undefined {
  const trimmed = value.trim();
  if (!trimmed || NUMERIC_LIKE.test(trimmed) || TIME_ONLY.test(trimmed) || !hasValidIsoDate(trimmed)) return undefined;
  return validTimestamp(Date.parse(trimmed));
}

function formatterKey({ locale, timeZone }: DateTimeFormatOptions): string {
  return `${locale ?? ""}\u0000${timeZone ?? ""}`;
}

function createDateTimeFormatter(options: DateTimeFormatOptions): Intl.DateTimeFormat {
  return new Intl.DateTimeFormat(options.locale, {
    dateStyle: "medium",
    timeStyle: "short",
    timeZone: options.timeZone,
  });
}

function dateTimeFormatter(options: DateTimeFormatOptions): Intl.DateTimeFormat {
  if (options.timeZone === undefined) return createDateTimeFormatter(options);

  const key = formatterKey(options);
  const cached = dateTimeFormatters.get(key);
  if (cached) return cached;

  if (dateTimeFormatters.size >= FORMATTER_CACHE_LIMIT) dateTimeFormatters.clear();
  const formatter = createDateTimeFormatter(options);
  dateTimeFormatters.set(key, formatter);
  return formatter;
}

export function toDate(value: DateTimeInput): Date | undefined {
  const timestamp = value instanceof Date
    ? validTimestamp(value.getTime())
    : typeof value === "number"
      ? validTimestamp(value)
      : typeof value === "string"
        ? /^\d+$/.test(value.trim())
          ? validTimestamp(Number(value.trim()))
          : parseString(value)
        : undefined;
  return timestamp === undefined ? undefined : new Date(timestamp);
}

export function formatDateTime(value: DateTimeInput, options: DateTimeFormatOptions = {}): string | undefined {
  const date = toDate(value);
  return date ? dateTimeFormatter(options).format(date) : undefined;
}

export function isoDateTime(value: DateTimeInput): string | undefined {
  return toDate(value)?.toISOString();
}

export function formatElapsedShort(value: DateTimeInput, now = Date.now()): string | undefined {
  const date = toDate(value);
  if (!date || !Number.isFinite(now)) return undefined;
  const minutes = Math.max(0, Math.round((now - date.getTime()) / MILLISECONDS_IN_MINUTE));
  if (minutes < 1) return "now";
  if (minutes < MINUTES_IN_HOUR) return `${minutes}m`;
  if (minutes < MINUTES_IN_DAY) return `${Math.round(minutes / MINUTES_IN_HOUR)}h`;
  return `${Math.round(minutes / MINUTES_IN_DAY)}d`;
}

export function formatRelativeTime(value: DateTimeInput, now = Date.now()): string | undefined {
  const date = toDate(value);
  if (!date || !Number.isFinite(now)) return undefined;
  return relativeTimeFormatter.format(
    Math.round((date.getTime() - now) / MILLISECONDS_IN_MINUTE),
    "minute",
  );
}
