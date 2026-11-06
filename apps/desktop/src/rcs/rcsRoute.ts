import type { DesktopSnapshot } from "../bridge/bridge";

export type RcsRouteInput = {
  snapshot?: DesktopSnapshot;
  activeConversationId?: string;
  gatewayId?: string;
  nowMs: number;
  locked: boolean;
};

export type RcsRoute = { current: true; expiresAtMs: number } | { current: false };

function expiry(value: string | undefined): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined;
}

export function currentRcsRoute({ snapshot, activeConversationId, gatewayId, nowMs, locked }: RcsRouteInput): RcsRoute {
  if (!snapshot || locked || snapshot.encryption.state !== "unlocked" || (snapshot.mode !== "native" && snapshot.mode !== "browser") || !activeConversationId || !gatewayId) return { current: false };

  const conversation = snapshot.conversations.find((item) => item.id === activeConversationId);
  if (!conversation || conversation.draftOnly || conversation.recipientlessDraft || (conversation.participants?.length ?? 0) > 1) return { current: false };

  const gateway = snapshot.gateways.find((item) => item.id === gatewayId);
  const gatewayStatus = snapshot.rcsStatus?.gateways.find((item) => item.gatewayId === gatewayId);
  const eligibility = snapshot.rcsStatus?.eligibilities.find((item) => item.conversationId === activeConversationId && item.gatewayId === gatewayId);
  const gatewayExpiresAtMs = expiry(gatewayStatus?.expiresAtMs);
  const eligibilityExpiresAtMs = expiry(eligibility?.expiresAtMs);
  if (!gateway || gateway.simulated || !gatewayStatus || gatewayStatus.state !== "linked" || !gatewayStatus.current || !gatewayStatus.available || !gatewayStatus.healthy || !gatewayStatus.phoneResponsive || !eligibility || eligibility.eligibility !== "eligible" || gatewayExpiresAtMs === undefined || eligibilityExpiresAtMs === undefined) return { current: false };

  const expiresAtMs = Math.min(gatewayExpiresAtMs, eligibilityExpiresAtMs);
  return nowMs < expiresAtMs ? { current: true, expiresAtMs } : { current: false };
}
