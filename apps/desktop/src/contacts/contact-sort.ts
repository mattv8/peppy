import type { ContactSort } from "../bridge/bridge";

const SORT_STORAGE_KEY = "peppy.contacts.sort.v1";
const DEFAULT_SORT: ContactSort = { key: "family", direction: "asc" };

export function readSortPreference(): ContactSort {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(SORT_STORAGE_KEY) ?? "null");
    return typeof value === "object" && value !== null && (value as ContactSort).key !== undefined && (value as ContactSort).direction !== undefined
      && ["given", "family"].includes((value as ContactSort).key) && ["asc", "desc"].includes((value as ContactSort).direction)
      ? { key: (value as ContactSort).key, direction: (value as ContactSort).direction } : DEFAULT_SORT;
  } catch { return DEFAULT_SORT; }
}

export function persistSortPreference(sort: ContactSort) {
  try { localStorage.setItem(SORT_STORAGE_KEY, JSON.stringify(sort)); } catch { /* Preferences are optional. */ }
}
