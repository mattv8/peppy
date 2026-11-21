// Shared Contacts test fixtures. Every test file must declare `vi.mock("../bridge/bridge", { spy: true });` itself (vi.mock is hoisted per file) and call `resetContactBridgeMocks()` from its beforeEach.
import { cleanup } from "@testing-library/react";
import { vi } from "vitest";
import { bridge, type ContactBookView, type ContactView } from "../bridge/bridge";

export const mockContactBook: ContactBookView = {
  id: "book-pixel8",
  deviceName: "Pixel 8",
  state: "active",
  capabilities: {
    canWrite: true,
    canDelete: true,
    supportsNotes: true,
    supportsPhoto: true,
    supportsBirthday: true,
  },
  contactCount: 2,
  defaultAccountLabel: "Google",
  lastSyncAt: "Just now",
  pendingEditCount: 0,
};

export const mockContactAurora: ContactView = {
  id: "contact-aurora",
  bookId: "book-pixel8",
  revision: "1",
  displayName: "Aurora Chen",
  givenName: "Aurora",
  familyName: "Chen",
  phones: [{ label: "mobile", number: "+12025550123" }],
  emails: [{ label: "work", address: "aurora@example.com" }],
  addresses: [],
  organization: "Northstar",
  title: "Director",
};

export const mockContactRiver: ContactView = {
  id: "contact-river",
  bookId: "book-pixel8",
  revision: "1",
  displayName: "River Park",
  givenName: "River",
  familyName: "Park",
  phones: [{ label: "mobile", number: "+12025550124" }],
  emails: [],
  addresses: [],
};

export const originalScrollIntoView = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
export const storedPreferences = new Map<string, string>();
export const preferenceStorage = {
  getItem: (key: string) => storedPreferences.get(key) ?? null,
  setItem: (key: string, value: string) => storedPreferences.set(key, value),
  removeItem: (key: string) => storedPreferences.delete(key),
};

export const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(resolvePromise => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
};

export class DeferredCropImage {
  static instances: DeferredCropImage[] = [];
  public onload: (() => void) | null = null;
  public onerror: (() => void) | null = null;
  public set src(_value: string) { DeferredCropImage.instances.push(this); }
}

/** Body of a test file's beforeEach: clears mocks, stubs storage and scrollIntoView, and sets default bridge responses. */
export function resetContactBridgeMocks() {
  vi.clearAllMocks();
  vi.stubGlobal("localStorage", preferenceStorage);
  storedPreferences.clear();
  localStorage.removeItem("peppy.contacts.sort.v1");
  localStorage.removeItem("peppy.layout.v1");
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    value: vi.fn(),
  });
  vi.mocked(bridge.list_contact_books).mockResolvedValue([mockContactBook]);
  vi.mocked(bridge.list_contacts).mockResolvedValue([
    mockContactAurora,
    mockContactRiver,
  ]);
  vi.mocked(bridge.forget_contact_book).mockResolvedValue(undefined);
  // clearAllMocks keeps implementations, so a scan status one test sets would leak into the next.
  vi.mocked(bridge.get_contact_sync_request).mockResolvedValue(null);
  vi.mocked(bridge.list_restorable_contacts).mockResolvedValue([]);
}

/** Body of a test file's afterEach: unmounts, restores globals and scrollIntoView. */
export function restoreContactDomGlobals() {
  cleanup();
  vi.unstubAllGlobals();
  if (originalScrollIntoView) {
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", originalScrollIntoView);
  } else {
    delete (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  }
}
