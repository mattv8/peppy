import type { GatewayAvailability, GatewayView } from "@peppy/desktop-ui/bridge-contracts";

/** A route choice as the composer and head panel see it (mirrors `RouteChoice` in setup/routeProblem.ts). */
export type PresentedRoute =
  | { kind: "route"; gateway: GatewayView; implicit: boolean }
  | { kind: "stale"; gatewayId: string; simId: string }
  | { kind: "none" };

const availabilityOf = (gateway: GatewayView): GatewayAvailability => gateway.availability ?? "unknown";

/**
 * The send refusal for a phone whose import is running. The setup flag is authoritative for refusal:
 * a setting-up phone never sends, whatever its presence says.
 */
export const isSettingUp = (gateway: GatewayView): boolean => gateway.settingUp === true || availabilityOf(gateway) === "setting_up";

/**
 * Live wording for a phone that is setting up. Presence decides it: an importing phone whose presence
 * is offline or delayed is presented as that presence, so "Setting up" never hides a stale phone.
 */
export const isLiveSettingUp = (gateway: GatewayView): boolean => availabilityOf(gateway) === "setting_up";

/**
 * The setting-up gateway a route refuses for, if any. A stored route to that phone (with its SIM id
 * vanished) refuses as that phone. Presentation only: the stored pair is never rewritten.
 */
export function settingUpGatewayFor(route: PresentedRoute, gateways: GatewayView[]): GatewayView | undefined {
  if (route.kind === "route") return isSettingUp(route.gateway) ? route.gateway : undefined;
  if (route.kind === "stale") return gateways.find(gateway => gateway.id === route.gatewayId && isSettingUp(gateway));
  return undefined;
}

/** The setting-up gateway whose live presence says it is setting up (the "Setting up" wording). */
export function liveSettingUpGatewayFor(route: PresentedRoute, gateways: GatewayView[]): GatewayView | undefined {
  const gateway = settingUpGatewayFor(route, gateways);
  return gateway && isLiveSettingUp(gateway) ? gateway : undefined;
}

/**
 * Presentation for a stored `(gatewayId, simId)` pair. A stored pair naming a setting-up phone
 * (whichever SIM it had) resolves to that phone, which is never written back.
 */
export function resolveStoredRoute(stored: { gatewayId: string; simId: string }, gateways: GatewayView[]): PresentedRoute {
  const exact = gateways.find(gateway => gateway.id === stored.gatewayId && gateway.simId === stored.simId);
  if (exact && !isSettingUp(exact)) return { kind: "route", gateway: exact, implicit: false };
  const settingUp = gateways.find(gateway => gateway.id === stored.gatewayId && isSettingUp(gateway));
  if (settingUp) return { kind: "route", gateway: settingUp, implicit: false };
  return { kind: "stale", gatewayId: stored.gatewayId, simId: stored.simId };
}
