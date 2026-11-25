import { vi } from "vitest";
import { conversationWireDefaults } from "./conversationWireDefaults";
import { bridge, type ContactBookView, type ConversationView, type DesktopSnapshot, type Draft, type DraftInput, type GatewayView, type SendDraftInput } from "../bridge/bridge";

export type HostError = { code: string; message: string };

export const SMS_ONLY: GatewayView = {
  id: "gw-phone",
  name: "Phone",
  simId: "sim-1",
  online: false,
  simulated: false,
  supportsSms: true,
  supportsMms: false,
  availability: "unknown",
  settingUp: false,
  wakeRoute: false,
};
export const MMS_SIM: GatewayView = { ...SMS_ONLY, simId: "sim-2", supportsMms: true, mmsContentVersion: 2 };

/**
 * In-memory stand-in for the native host contract (apps/desktop/src-tauri/src/session.rs):
 * an empty draft ID asks the host to create the draft (and, with an empty conversation ID, a new
 * conversation); saves are CAS on `expectedRevision`; sends use the STORED draft and clear its
 * message fields while retaining its identity, route, and recipients.
 */
export function createHost() {
  let created = 0;
  const host = {
    gateways: [SMS_ONLY, MMS_SIM] as GatewayView[],
    connection: {
      state: "connected",
      origin: "https://example.test",
    } as DesktopSnapshot["connection"],
    encryption: { state: "unlocked" } as DesktopSnapshot["encryption"],
    desktop: { trayAvailable: true, startAtLogin: false, startupSupported: true, background: false } as NonNullable<DesktopSnapshot["desktop"]>,
    head: { enabled: false, capability: "unsupported" } as DesktopSnapshot["head"],
    conversations: [
      {
        id: "aurora",
        name: "Aurora",
        preview: "Hello",
        unread: 1,
        messages: [
          {
            id: "m-aurora",
            revision: "1",
            sender: "other",
            body: "Hello from Aurora",
            timestamp: "now",
            attachments: [
              {
                id: "photo-1",
                name: "photo.png",
                mediaType: "image/png",
                byteSize: 10,
                state: "ready",
                transfer: "download",
                retryable: true,
              },
            ],
          },
        ],
      },
      {
        id: "river",
        name: "River",
        preview: "Hi",
        unread: 0,
        messages: [
          {
            id: "m-river",
            revision: "1",
            sender: "other",
            body: "Hello from River",
            timestamp: "now",
            attachments: [],
          },
        ],
      },
    ] as ConversationView[],
    drafts: new Map<string, Draft>(),
    failSaves: undefined as HostError | undefined,
    contactResolution: undefined as DesktopSnapshot["contactResolution"],
    contactBooks: [] as ContactBookView[],
    notifications: [] as DesktopSnapshot["notifications"],
    appFilters: [] as DesktopSnapshot["appFilters"],
    sent: [] as Draft[],
    load(conversationId?: string): DesktopSnapshot {
      const known = [
        ...host.conversations.map((c) => c.id),
        ...host.drafts.keys(),
      ];
      const active =
        conversationId && known.includes(conversationId)
          ? conversationId
          : known[0];
      const listed = host.conversations.map((c) => ({
        ...c,
        messages: c.id === active ? c.messages : [],
      }));
      const draftOnly = [...host.drafts.values()]
        .filter(
          (d) => !host.conversations.some((c) => c.id === d.conversationId),
        )
        .map((d) => ({
          ...conversationWireDefaults,
          id: d.conversationId,
          name: d.recipientIds.join(", ") || "New message",
          preview: "Draft",
          unread: 0,
          messages: [],
          draftOnly: true,
          recipientlessDraft: d.recipientIds.length === 0,
        }));
      return {
        writesPaused: false, version: "1",
        mode: "native",
        connection: host.connection,
        encryption: host.encryption,
        gateways: host.gateways,
        conversations: [...listed, ...draftOnly],
        activeConversationId: active,
        draft: active ? host.drafts.get(active) : undefined,
        desktop: host.desktop,
        head: host.head,
        pendingCount: 0,
        quarantineCount: 0,
        notifications: host.notifications,
        contactResolution: host.contactResolution,
        contactBooks: host.contactBooks,
        appFilters: host.appFilters,
        notificationPreferences: {
          messageBanners: true,
          mirroredBanners: true,
          preview: "full",
        },
      };
    },
    save(input: DraftInput): Draft {
      if (host.failSaves) throw host.failSaves;
      let current: Draft | undefined;
      if (input.id === "") {
        const conversationId = input.conversationId || `conv-new-${++created}`;
        current = host.drafts.get(conversationId) ?? {
          id: `draft-${conversationId}`,
          conversationId,
          text: "",
          recipientIds: [],
          attachmentIds: [],
          revision: "0",
        };
      } else {
        current = [...host.drafts.values()].find((d) => d.id === input.id);
        if (!current)
          throw {
            code: "not-found",
            message: "The requested stored item was not found.",
          } satisfies HostError;
      }
      if (
        input.conversationId &&
        input.conversationId !== current.conversationId
      )
        throw {
          code: "invalid-draft",
          message: "The draft does not belong to this conversation.",
        } satisfies HostError;
      if (current.revision !== input.expectedRevision)
        throw {
          code: "stale-draft",
          message: `The draft changed elsewhere (current revision ${current.revision}); both versions were kept.`,
        } satisfies HostError;
      const saved: Draft = {
        ...current,
        text: input.text,
        recipientIds: input.recipientIds,
        attachmentIds: input.attachmentIds,
        gatewayId: input.gatewayId ?? current.gatewayId,
        simId: input.simId ?? current.simId,
        revision: String(Number(current.revision) + 1),
      };
      host.drafts.set(saved.conversationId, saved);
      return saved;
    },
    send(input: SendDraftInput) {
      const stored = [...host.drafts.values()].find((d) => d.id === input.id);
      if (!stored)
        throw {
          code: "not-found",
          message: "The requested stored item was not found.",
        } satisfies HostError;
      if (stored.revision !== input.expectedRevision)
        throw {
          code: "stale-draft",
          message: "The draft changed elsewhere; both versions were kept.",
        } satisfies HostError;
      host.sent.push({
        ...stored,
        gatewayId: input.gatewayId,
        simId: input.simId,
      });
      const cleared: Draft = {
        ...stored,
        text: "",
        attachmentIds: [],
        revision: String(Number(stored.revision) + 1),
      };
      host.drafts.set(cleared.conversationId, cleared);
      return {
        accepted: true,
        status: "queued-local" as const,
        revision: cleared.revision,
      };
    },
    discard(input: { id: string; conversationId: string; expectedRevision: string }) {
      const draft = host.drafts.get(input.conversationId);
      if (!draft || draft.id !== input.id || draft.revision !== input.expectedRevision) throw { code: "stale-draft", message: "The draft changed elsewhere." } satisfies HostError;
      if (draft.recipientIds.length) throw { code: "draft-not-discardable", message: "Only recipientless drafts can be deleted." } satisfies HostError;
      host.drafts.delete(input.conversationId);
    },
  };
  return host;
}

export function installAppHost(
  host: ReturnType<typeof createHost>,
  listeners: {
    lifecycle: Parameters<typeof bridge.subscribe_lifecycle>[0];
    lifecycleFinished: Parameters<typeof bridge.subscribe_lifecycle_finished>[0];
    subscribe: () => void;
    drafts: (draft: Draft) => void;
    lifecycleDispose: () => void;
    lifecycleFinishedDispose: () => void;
    draftDispose: () => void;
    hintListeners: Set<() => void>;
  },
) {
  vi.spyOn(bridge, "load_state").mockImplementation(async (id) =>
    host.load(id),
  );
  vi.spyOn(bridge, "save_draft").mockImplementation(async (input) =>
    host.save(input),
  );
  vi.spyOn(bridge, "send_draft").mockImplementation(async (input) =>
    host.send(input),
  );
  vi.spyOn(bridge, "discard_draft").mockImplementation(async (input) => host.discard(input));
  vi.spyOn(bridge, "pick_attachments").mockResolvedValue([
    {
      id: "file-1",
      name: "file.png",
      mediaType: "image/png",
      byteSize: 2,
      state: "ready",
    },
  ]);
  vi.spyOn(bridge, "retry_attachment").mockResolvedValue();
  vi.spyOn(bridge, "save_attachment").mockResolvedValue(true);
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "publish_attachment").mockResolvedValue(null);
  vi.spyOn(bridge, "open_composer").mockResolvedValue();
  vi.spyOn(bridge, "close_composer").mockResolvedValue();
  vi.spyOn(bridge, "close_head_panel").mockResolvedValue();
  vi.spyOn(bridge, "popout_conversation").mockResolvedValue({ headCreated: true });
  vi.spyOn(bridge, "set_start_at_login").mockResolvedValue();
  vi.spyOn(bridge, "acknowledge_lifecycle").mockResolvedValue();
  vi.spyOn(bridge, "subscribe_lifecycle").mockImplementation(listener => {
    listeners.lifecycle = listener;
    return listeners.lifecycleDispose;
  });
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockImplementation(listener => {
    listeners.lifecycleFinished = listener;
    return listeners.lifecycleFinishedDispose;
  });
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    listeners.hintListeners.add(listener);
    listeners.subscribe();
    return () => { listeners.hintListeners.delete(listener); };
  });
  const draftBridge = bridge as typeof bridge & { subscribe_drafts(listener: (draft: Draft) => void): () => void };
  vi.spyOn(draftBridge, "subscribe_drafts").mockImplementation(listener => {
    listeners.drafts = listener;
    return listeners.draftDispose;
  });
}
