import type { ConversationView, DesktopSnapshot } from "../bridge/bridge";

export type RcsRouteInput = {
  snapshot?: DesktopSnapshot;
  activeConversationId?: string;
  gatewayId?: string;
  nowMs: number;
  locked: boolean;
};

export type RcsRoute = { current: true; expiresAtMs: number } | { current: false };

export type RcsSendBlock =
  | { kind: "none" }
  | { kind: "unproven" }
  | { kind: "link"; help: "relink" | "phone" };

function expiry(value: string | undefined): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined;
}

function rcsRouteContext({ snapshot, activeConversationId, gatewayId, locked }: Omit<RcsRouteInput, "nowMs">) {
  if (!snapshot || locked || snapshot.encryption.state !== "unlocked" || (snapshot.mode !== "native" && snapshot.mode !== "browser") || !activeConversationId || !gatewayId) return undefined;
  const conversation = snapshot.conversations.find((item) => item.id === activeConversationId);
  const gateway = snapshot.gateways.find((item) => item.id === gatewayId);
  if (!conversation || conversation.draftOnly || conversation.recipientlessDraft || (conversation.participants?.length ?? 0) > 1 || !gateway || gateway.simulated) return undefined;
  const gatewayStatus = snapshot.rcsStatus?.gateways.find((item) => item.gatewayId === gatewayId);
  const eligibility = snapshot.rcsStatus?.eligibilities.find((item) => item.conversationId === activeConversationId && item.gatewayId === gatewayId);
  return { conversation, gatewayStatus, eligibility };
}

export function knownRcsChat(conversation: ConversationView): boolean {
  return conversation.rcsEncryption !== undefined || conversation.rcsHistoryGap === true || conversation.messages.some((message) => message.transport === "rcs");
}

export function currentRcsRoute({ snapshot, activeConversationId, gatewayId, nowMs, locked }: RcsRouteInput): RcsRoute {
  const context = rcsRouteContext({ snapshot, activeConversationId, gatewayId, locked });
  if (!context) return { current: false };
  const { gatewayStatus, eligibility } = context;
  const gatewayExpiresAtMs = expiry(gatewayStatus?.expiresAtMs);
  const eligibilityExpiresAtMs = expiry(eligibility?.expiresAtMs);
  if (!gatewayStatus || gatewayStatus.state !== "linked" || !gatewayStatus.current || !gatewayStatus.available || !gatewayStatus.healthy || !gatewayStatus.phoneResponsive || !eligibility || eligibility.eligibility !== "eligible" || gatewayExpiresAtMs === undefined || eligibilityExpiresAtMs === undefined) return { current: false };

  const expiresAtMs = Math.min(gatewayExpiresAtMs, eligibilityExpiresAtMs);
  return nowMs < expiresAtMs ? { current: true, expiresAtMs } : { current: false };
}

export function rcsSendBlock(input: RcsRouteInput): RcsSendBlock {
  if (currentRcsRoute(input).current) return { kind: "none" };
  const { nowMs } = input;
  const context = rcsRouteContext(input);
  if (!context || !knownRcsChat(context.conversation)) return { kind: "none" };
  const { gatewayStatus, eligibility } = context;
  if (!gatewayStatus || gatewayStatus.state === "unlinked") return { kind: "none" };
  if (gatewayStatus.state === "needs_relink" || gatewayStatus.state === "signed_out") return { kind: "link", help: "relink" };

  const gatewayExpiresAtMs = expiry(gatewayStatus.expiresAtMs);
  if (gatewayStatus.state === "unavailable" || !gatewayStatus.current || !gatewayStatus.available || !gatewayStatus.healthy || !gatewayStatus.phoneResponsive || gatewayExpiresAtMs === undefined || nowMs >= gatewayExpiresAtMs) return { kind: "link", help: "phone" };

  const eligibilityExpiresAtMs = expiry(eligibility?.expiresAtMs);
  return eligibility?.eligibility === "not_eligible" && eligibilityExpiresAtMs !== undefined && nowMs < eligibilityExpiresAtMs
    ? { kind: "none" }
    : { kind: "unproven" };
}

export function rcsBlockExpiry(input: RcsRouteInput): number | undefined {
  const context = rcsRouteContext(input);
  if (!context || !knownRcsChat(context.conversation) || context.gatewayStatus?.state !== "linked") return undefined;
  const { gatewayStatus: gateway, eligibility } = context;
  const values = [expiry(gateway?.expiresAtMs), expiry(eligibility?.expiresAtMs)].filter((value): value is number => value !== undefined);
  return values.length ? Math.min(...values) : undefined;
}
