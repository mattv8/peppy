const FALLBACK_ERROR =
  "The native operation failed. Your edits remain in this window.";

export const errorText = (error: unknown): string =>
  typeof error === "object" &&
  error !== null &&
  "message" in error &&
  typeof error.message === "string" &&
  error.message
    ? error.message
    : FALLBACK_ERROR;

export const errorCode = (error: unknown): string | undefined =>
  typeof error === "object" &&
  error !== null &&
  "code" in error &&
  typeof error.code === "string"
    ? error.code
    : undefined;
