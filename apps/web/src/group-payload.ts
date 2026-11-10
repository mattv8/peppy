import type { AttachmentView, ConversationView, DesktopSnapshot, GroupNoticeView, GroupView, MessageStatus, MessageView, MessageWindow, SendDraftInput, SendResultView, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

/**
 * Boundary checks for the group fields the browser worker sends back. Each parser returns a value
 * the UI can rely on, or undefined (or throws, for send results) when the payload is malformed.
 * Unknown fields are dropped, never passed through.
 */

const GROUP_NAME_MAX_LENGTH = 100;
const GROUP_STATES: readonly GroupView["state"][] = ["unlinked", "proving", "live", "paused", "withdrawn"];
const SEND_REASONS: readonly NonNullable<SendResultView["reason"]>[] = ["members_changed", "update_required", "withdrawn", "create_unconfirmed", "create_not_rcs"];
const SEND_STATUSES: readonly MessageStatus[] = ["queued-local", "server-accepted", "gateway-persisted", "preparing", "submitted", "sent", "delivery-confirmed", "failed-before-submit", "failed-confirmed", "unknown", "read"];
const DECIMAL = /^(0|[1-9]\d*)$/;
const CANONICAL_E164 = /^\+[1-9]\d{6,14}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isAddress = (value: unknown): value is string => typeof value === "string" && CANONICAL_E164.test(value);
const isDecimal = (value: unknown): value is string => typeof value === "string" && DECIMAL.test(value);
const NOTICE_KINDS: readonly GroupNoticeView["kind"][] = ["renamed", "added", "removed"];

function isGroupState(value: unknown): value is GroupView["state"] {
  return GROUP_STATES.some((state) => state === value);
}

function isSendReason(value: unknown): value is NonNullable<SendResultView["reason"]> {
  return SEND_REASONS.some((reason) => reason === value);
}

function isSendStatus(value: unknown): value is MessageStatus {
  return SEND_STATUSES.some((status) => status === value);
}

/** Parses a conversation's group projection. Returns undefined when any required field is malformed. */
export function parseGroupView(value: unknown): GroupView | undefined {
  if (!isRecord(value)) return undefined;
  const { name, members, state, memberRevision, canCreate, createNeedsConfirmation, notices } = value;
  if (name !== undefined && typeof name !== "string") return undefined;
  if (!Array.isArray(members) || !members.every(isAddress)) return undefined;
  if (!isGroupState(state) || !isDecimal(memberRevision)) return undefined;
  if (typeof canCreate !== "boolean" || typeof createNeedsConfirmation !== "boolean") return undefined;
  return {
    ...(name === undefined ? {} : { name }),
    members: [...members],
    state,
    memberRevision,
    canCreate,
    createNeedsConfirmation,
    ...(Array.isArray(notices) ? { notices: notices.flatMap(parseGroupNotice) } : {}),
  };
}

/** A notice the core could not describe is dropped alone; the others still show. */
function parseGroupNotice(value: unknown): GroupNoticeView[] {
  if (!isRecord(value) || !NOTICE_KINDS.some((kind) => kind === value.kind) || !isDecimal(value.atMs)) return [];
  if (value.name !== undefined && typeof value.name !== "string") return [];
  if (value.member !== undefined && !isAddress(value.member)) return [];
  if (value.kind !== "renamed" && value.member === undefined) return [];
  return [{
    kind: value.kind as GroupNoticeView["kind"],
    atMs: value.atMs,
    ...(value.name === undefined ? {} : { name: value.name }),
    ...(value.member === undefined ? {} : { member: value.member }),
  } as GroupNoticeView];
}

const RCS_STATES: readonly NonNullable<AttachmentView["rcsState"]>[] = ["downloading", "stored", "too_large", "quota_full", "failed", "expired"];
const RCS_SEND_STATUSES: readonly NonNullable<AttachmentView["rcsSendStatus"]>[] = ["sending", "sent", "not_sent", "outcome_unknown", "members_changed"];

/** Keeps only the RCS fields the UI understands. A malformed field is dropped alone, the attachment stays. */
export function parseRcsAttachmentFields(value: Record<string, unknown>): Pick<AttachmentView, "rcsState" | "rcsSendStatus" | "index"> {
  const rcsState = RCS_STATES.find((state) => state === value.rcsState);
  const rcsSendStatus = RCS_SEND_STATUSES.find((status) => status === value.rcsSendStatus);
  const index = typeof value.index === "number" && Number.isSafeInteger(value.index) && value.index >= 0 ? value.index : undefined;
  return {
    ...(rcsState === undefined ? {} : { rcsState }),
    ...(rcsSendStatus === undefined ? {} : { rcsSendStatus }),
    ...(index === undefined ? {} : { index }),
  };
}

function normalizeAttachment(value: AttachmentView): AttachmentView {
  const { rcsState: _state, rcsSendStatus: _status, index: _index, ...rest } = value;
  return { ...rest, ...parseRcsAttachmentFields(value) };
}

function normalizeRcsMessage(message: MessageView): MessageView {
  return Array.isArray(message.attachments)
    ? { ...message, attachments: message.attachments.map(normalizeAttachment) }
    : message;
}

function normalizeRcsMessages(snapshot: DesktopSnapshot): DesktopSnapshot {
  if (!Array.isArray(snapshot.conversations) || !snapshot.conversations.some((conversation) => Array.isArray(conversation.messages) && conversation.messages.some((message) => Array.isArray(message.attachments) && message.attachments.length > 0))) return snapshot;
  return {
    ...snapshot,
    conversations: snapshot.conversations.map((conversation) => Array.isArray(conversation.messages)
      ? { ...conversation, messages: conversation.messages.map(normalizeRcsMessage) }
      : conversation),
  };
}

/** Normalizes RCS attachment fields from a paged message response as well as snapshots. */
export function normalizeMessageWindow(window: MessageWindow): MessageWindow {
  return { ...window, messages: window.messages.map(normalizeRcsMessage) };
}

/** Keeps every conversation; drops only a malformed `group` so one bad projection cannot blank the list. Also checks RCS attachment fields. */
export function normalizeGroupSnapshot(snapshot: DesktopSnapshot): DesktopSnapshot {
  const checked = normalizeRcsMessages(snapshot);
  if (!Array.isArray(checked.conversations) || !checked.conversations.some((conversation) => "group" in conversation)) return checked;
  return { ...checked, conversations: checked.conversations.map(normalizeConversationGroup) };
}

function normalizeConversationGroup(conversation: ConversationView): ConversationView {
  if (!("group" in conversation)) return conversation;
  const group = parseGroupView(conversation.group);
  const { group: _dropped, ...rest } = conversation;
  return group ? { ...rest, group } : rest;
}

/**
 * Parses a send result. Throws on a malformed envelope, because the composer cannot tell whether
 * the message went out. Group refusal fields are checked and dropped when they are malformed.
 */
export function parseSendResult(value: unknown): SendResultView {
  if (!isRecord(value) || typeof value.accepted !== "boolean" || !isSendStatus(value.status)) throw new Error("Invalid send result.");
  const reason = isSendReason(value.reason) ? value.reason : undefined;
  const group = parseRefusedGroup(value.group);
  const refusedAttachmentIds = Array.isArray(value.refusedAttachmentIds) && value.refusedAttachmentIds.every((id) => typeof id === "string") ? [...value.refusedAttachmentIds] : undefined;
  return {
    accepted: value.accepted,
    status: value.status,
    ...(reason === undefined ? {} : { reason }),
    ...(refusedAttachmentIds === undefined ? {} : { refusedAttachmentIds }),
    ...(typeof value.revision === "string" ? { revision: value.revision } : {}),
    ...(typeof value.parts === "number" && Number.isSafeInteger(value.parts) ? { parts: value.parts } : {}),
    ...(group === undefined ? {} : { group }),
  };
}

function parseRefusedGroup(value: unknown): SendResultView["group"] | undefined {
  if (!isRecord(value) || !Array.isArray(value.members) || !value.members.every(isAddress) || !isDecimal(value.memberRevision)) return undefined;
  return { members: [...value.members], memberRevision: value.memberRevision };
}

/** Parses a typing signal; `participant` is required to attribute a group signal and must be canonical. */
export function parseTypingSignal(value: unknown): TypingSignal | undefined {
  if (!isRecord(value) || typeof value.conversationId !== "string") return undefined;
  if (value.state !== "started" && value.state !== "stopped") return undefined;
  if (value.expiresAtMs !== undefined && !isDecimal(value.expiresAtMs)) return undefined;
  if (value.participant !== undefined && !isAddress(value.participant)) return undefined;
  return {
    conversationId: value.conversationId,
    state: value.state,
    ...(value.expiresAtMs === undefined ? {} : { expiresAtMs: value.expiresAtMs }),
    ...(value.participant === undefined ? {} : { participant: value.participant }),
  };
}

type GroupDraftFields = Pick<SendDraftInput, "groupName" | "expectedMemberRevision" | "groupCreateConfirmDuplicate">;

/** Validates the outgoing group fields before they reach the worker. Throws on an invalid value. */
export function groupDraftFields(input: GroupDraftFields): GroupDraftFields {
  if (input.groupName !== undefined && (typeof input.groupName !== "string" || Array.from(input.groupName).length > GROUP_NAME_MAX_LENGTH)) throw new Error("Invalid group name.");
  if (input.expectedMemberRevision !== undefined && !isDecimal(input.expectedMemberRevision)) throw new Error("Invalid member revision.");
  if (input.groupCreateConfirmDuplicate !== undefined && typeof input.groupCreateConfirmDuplicate !== "boolean") throw new Error("Invalid group create confirmation.");
  return {
    ...(input.groupName === undefined ? {} : { groupName: input.groupName }),
    ...(input.expectedMemberRevision === undefined ? {} : { expectedMemberRevision: input.expectedMemberRevision }),
    ...(input.groupCreateConfirmDuplicate === undefined ? {} : { groupCreateConfirmDuplicate: input.groupCreateConfirmDuplicate }),
  };
}
