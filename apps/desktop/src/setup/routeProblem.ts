import type { GatewayAvailability, GatewayView } from "@peppy/desktop-ui/bridge-contracts";
import type { MmsPlan } from "../drafts/mmsPlan";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";
import { fileSize } from "../fileSize";
import { settingUpGatewayFor } from "./settingUpRoute";

export type RouteChoice =
  | { kind: "route"; gateway: GatewayView; implicit: boolean }
  | { kind: "stale"; gatewayId: string; simId: string }
  | { kind: "none" };

export const DEFAULT_MMS_LIMIT_BYTES = 300 * 1024;

export function gatewayAvailability(gateway: GatewayView): GatewayAvailability { return gateway.availability ?? "unknown"; }

export function availabilityProblem(gateway: GatewayView): string | undefined {
  switch (gatewayAvailability(gateway)) {
    case "setting_up": return formatCopy(peppyCopy.gateway_problem_setting_up, gateway.name);
    case "offline": return `${gateway.name} isn't responding. Sending is paused until it reconnects.`;
    case "never_connected": return `${gateway.name} has never connected. Finish setup on the phone to enable sending.`;
    case "no_sms_sim": return `No default SMS SIM is set on ${gateway.name}. Open the phone's SIM settings to fix this.`;
    case "sms_permission_needed": return `Peppy needs SMS permission on ${gateway.name}. Open Peppy on the phone to grant it.`;
    case "sms_unavailable": return `SMS is unavailable on ${gateway.name}. Check carrier settings on the phone.`;
    case "unsupported_platform": return "Can't send SMS through Peppy on this iPhone. Select an Android phone to send.";
    case "capabilities_unknown": return `${gateway.name}'s capabilities are unknown. Sending is paused until the phone reports in.`;
    default: return undefined;
  }
}

/** Names the blocked phone; says another phone can send only when one exists, so the user is pointed at it. */
function settingUpProblem(gateway: GatewayView, gateways: GatewayView[]): string {
  const another = gateways.some((item) => item.id !== gateway.id && item.supportsSms && availabilityProblem(item) === undefined);
  return formatCopy(another ? peppyCopy.gateway_problem_setting_up_other : peppyCopy.gateway_problem_setting_up, gateway.name);
}

/**
 * Why a phone whose import flag is set cannot send. The flag refuses the send whatever presence says;
 * the wording follows the phone's live presence: setup while live, then the same sentences a phone
 * that is not importing gets when it is offline or delayed.
 */
export function importingRefusal(gateway: GatewayView, gateways: GatewayView[]): string {
  const availability = gatewayAvailability(gateway);
  if (availability === "offline") return availabilityProblem(gateway) ?? settingUpProblem(gateway, gateways);
  if (availability === "delayed") {
    const seconds = gateway.lastSeenAgeSeconds;
    return seconds === undefined
      ? formatCopy(peppyCopy.gateway_problem_importing_delayed_unknown, gateway.name)
      : formatCopy(peppyCopy.gateway_problem_importing_delayed, gateway.name, ageText(seconds));
  }
  return settingUpProblem(gateway, gateways);
}

function ageText(seconds: number): string {
  return seconds < 60 ? "under a minute" : seconds < 3_600 ? `${Math.floor(seconds / 60)}m` : `${Math.floor(seconds / 3_600)}h`;
}

export function routeProblem(
  route: RouteChoice,
  gateways: GatewayView[],
  requiresMms: boolean,
  recipientCount: number,
  mmsPlan: MmsPlan,
  oversizedImageName?: string,
): string | undefined {
  if (route.kind === "none")
    return gateways.length
      ? "choose a phone and SIM"
      : "no phone is configured";
  const settingUp = settingUpGatewayFor(route, gateways);
  if (settingUp) return importingRefusal(settingUp, gateways);
  if (route.kind === "stale")
    return "the selected phone/SIM is no longer reported; choose another route";
  const availability = availabilityProblem(route.gateway);
  if (availability) return availability;
  if (!requiresMms && !route.gateway.supportsSms)
    return "this phone/SIM does not support SMS";
  if (requiresMms && !route.gateway.supportsMms)
    return "this phone/SIM does not support MMS attachments";
  const contentVersion = route.gateway.mmsContentVersion;
  if (requiresMms && (!Number.isSafeInteger(contentVersion) || (contentVersion ?? 0) < 2))
    return "this phone/SIM needs MMS content version 2";
  const recipientLimit = Math.min(route.gateway.mmsMaxRecipients ?? 20, 20);
  if (requiresMms && recipientCount > recipientLimit)
    return `this MMS route supports up to ${recipientLimit} recipients`;
  if (requiresMms && mmsPlan.kind === "error")
    return mmsPlan.imageId && oversizedImageName
      ? `${oversizedImageName} cannot be resized to fit the MMS limit`
      : `the MMS estimate exceeds the ${fileSize(route.gateway.mmsMaxBytes ?? DEFAULT_MMS_LIMIT_BYTES)} limit`;
  return undefined;
}
