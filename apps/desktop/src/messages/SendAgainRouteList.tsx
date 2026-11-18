import type { ReactElement } from "react";
import { formatComposerReason } from "@peppy/desktop-ui";
import type { GatewayView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import type { MmsPlan } from "../drafts/mmsPlan";
import { peppyCopy } from "../generated/peppyCopy";
import { routeBadges, type GatewayPickerOption } from "../setup/GatewayPicker";
import { gatewayAvailability, routeProblem } from "../setup/routeProblem";

/** The send_again call enforces MMS size limits, so the popover only needs a plan that reports no error. */
const NO_MMS_PLAN_PROBLEM: MmsPlan = { kind: "single", parts: 1 };

export type SendAgainRouteLabel = { simLabel?: string; statusLabel: string };
export type SendAgainRouteLabeler = (gateway: GatewayView) => SendAgainRouteLabel;

export type SendAgainRoute = { key: string; gateway: GatewayView; reason?: string };

export const sendAgainRouteKey = (gateway: GatewayView) => `${encodeURIComponent(gateway.id)}:${encodeURIComponent(gateway.simId)}`;
const domIdOf = (key: string) => key.replace(/[^A-Za-z0-9_-]/g, "_");
const recipientCountOf = (message: MessageView) => message.participants?.length ?? 1;
const requiresMmsFor = (message: MessageView) => message.transport === "mms" || message.attachments.length > 0 || recipientCountOf(message) > 1;

/**
 * Every phone the roster lists, with the composer's own refusal reason (undefined when it can send this message).
 * Each reason is judged against the phone alone, so the plain wording is used, never the "choose another phone"
 * variant. A route the call rejected with route-unavailable stays unselectable for this popover session.
 */
export function sendAgainRoutes(gateways: GatewayView[], message: MessageView, rejected: ReadonlySet<string>): SendAgainRoute[] {
  const requiresMms = requiresMmsFor(message);
  const recipients = recipientCountOf(message);
  return gateways.map((gateway) => {
    const key = sendAgainRouteKey(gateway);
    const reason = rejected.has(key)
      ? peppyCopy.message_send_again_route_gone
      : routeProblem({ kind: "route", gateway, implicit: false }, [gateway], requiresMms, recipients, NO_MMS_PLAN_PROBLEM);
    return { key, gateway, reason };
  });
}

function optionFor(route: SendAgainRoute, routeLabel: SendAgainRouteLabeler): GatewayPickerOption {
  const label = routeLabel(route.gateway);
  return { value: route.key, name: route.gateway.name, simLabel: label.simLabel, statusLabel: label.statusLabel, availability: gatewayAvailability(route.gateway), simulated: route.gateway.simulated };
}

export function reasonId(messageId: string, route: SendAgainRoute): string {
  return `send-again-reason-${messageId}-${domIdOf(route.key)}`;
}

export type RouteListProps = {
  messageId: string;
  routes: SendAgainRoute[];
  selectedKey?: string;
  /** The one route shown as a read-only row because the popover opened with exactly one eligible route. */
  onlyKey?: string;
  pending: boolean;
  routeLabel: SendAgainRouteLabeler;
  onChoose(key: string): void;
  hintId?: string;
};

/** The route fieldset: the read-only single row, radios for several eligible routes, disabled radios with the reason as description. */
export function RouteList({ messageId, routes, selectedKey, onlyKey, pending, routeLabel, onChoose, hintId }: RouteListProps): ReactElement {
  return <fieldset id={`send-again-routes-${messageId}`} data-send-again-routes aria-describedby={hintId}>
    <legend>{peppyCopy.message_send_again_route_legend}</legend>
    {routes.map((route) => {
      const badges = routeBadges(optionFor(route, routeLabel));
      if (onlyKey === route.key) return <div key={route.key} data-send-again-route={route.key} data-send-again-route-state="only">{badges}</div>;
      if (route.reason !== undefined) {
        const id = reasonId(messageId, route);
        return <label key={route.key} data-send-again-route={route.key} data-send-again-route-state="unavailable">
          <input type="radio" name={`send-again-route-${messageId}`} value={route.key} disabled aria-describedby={id} />
          {badges}
          <span id={id}>{formatComposerReason(route.reason)}</span>
        </label>;
      }
      return <label key={route.key} data-send-again-route={route.key} data-send-again-route-state="eligible">
        <input type="radio" name={`send-again-route-${messageId}`} value={route.key} checked={selectedKey === route.key} disabled={pending} onChange={() => onChoose(route.key)} />
        {badges}
      </label>;
    })}
  </fieldset>;
}

/** The no-route state: each phone's reason with its badges, no choice. */
export function RouteReasons({ routes, routeLabel }: { routes: SendAgainRoute[]; routeLabel: SendAgainRouteLabeler }): ReactElement {
  return <>
    {routes.map((route) => <div key={route.key} data-send-again-reason={route.key}>
      {routeBadges(optionFor(route, routeLabel))}
      <span>{route.reason === undefined ? "" : formatComposerReason(route.reason)}</span>
    </div>)}
  </>;
}
