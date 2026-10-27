import type { DesktopBridge } from "@peppy/desktop-ui/bridge-contracts";

export type {
  AttachmentView,
  ConversationView,
  DesktopSnapshot,
  Draft,
  DraftInput,
  GatewayView,
  MessageStatus,
  AppFilter,
  NotificationPreferences,
  PublicCopy,
  ContactResolutionMap,
  MediaCursor,
  MediaMetadataView,
  MediaPageItem,
  MediaPage,
  MediaOpen,
  LocalSearchRequest,
  LocalSearchPage,
  SearchIndexProgress,
  MessageWindowRequest,
  MessageWindow,
  DesktopBridge,
} from "@peppy/desktop-ui/bridge-contracts";

export let bridge: DesktopBridge;

export function installBrowserBridge(browserBridge: DesktopBridge): void {
  bridge = browserBridge;
}

export function renewHostedSession(): Promise<void> {
  return Promise.reject(new Error("Hosted session renewal is unavailable in the browser."));
}
