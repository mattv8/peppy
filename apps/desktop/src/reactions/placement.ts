export type ReactionRect = { top: number; bottom: number; left: number; right: number };

export type ReactionPlacementInput = {
  /** The reactable bubble (or media run article). */
  anchor: ReactionRect;
  /** Outgoing rows anchor the bar to the right edge. */
  outgoing: boolean;
  /** `#message-list`. */
  list: ReactionRect;
  /** Top of `#composer-area`, or the window height when there is none. */
  composerTop: number;
  /** Visible area (`visualViewport`, or the window). */
  viewport: { top: number; bottom: number };
  /** The sender label above a group row's first bubble. The bar starts to its right when they would overlap. */
  label?: ReactionRect;
  /** Measured bar size for the current route, pointer and width (see barMetrics). */
  width: number;
  height: number;
  /** How far the bar may overlap each edge without reaching text or a control. */
  reach: { top: number; bottom: number };
  /** Explicitly requested bars may cover the message when neither edge has room. */
  allowCover: boolean;
  /** Coarse pointers get a menu above the bubble (below when there is no room), never a straddle. */
  touch: boolean;
  /** Bottom of the chip row under the bubble, when the row has chips. A `below` bar starts under it. */
  chipsBottom?: number;
};

export type ReactionPlacement = { left: number; top: number; placement: "straddle" | "above" | "below" };

/** Maximum fine-pointer overlap at an anchor edge, also used by the covering fallback. */
export const BAR_OVERHANG = 16;
const SIDE_INSET = 8;
const EDGE = 4;
/** Covering fallback switches below when the bubble is this close to the list top. */
const PARTLY_SCROLLED = 8;

/** Returns null when the bubble is out of view or no safe edge fits for a hover bar. */
export function placeReactionBar(input: ReactionPlacementInput): ReactionPlacement | null {
  const { anchor, list, composerTop, viewport, width, height, outgoing } = input;
  if (anchor.bottom <= list.top || anchor.top >= viewport.bottom || anchor.top >= composerTop - EDGE) return null;
  const wanted = outgoing ? anchor.right - SIDE_INSET - width : horizontalStart(input);
  const left = clamp(wanted, list.left + SIDE_INSET, list.right - width - SIDE_INSET);
  return input.touch ? touchPlacement(input, left) : finePlacement(input, left);
}

/** Incoming bars start 8px inside the bubble, and clear the sender label when it sits above the bubble. */
function horizontalStart({ anchor, label }: ReactionPlacementInput): number {
  return Math.max(anchor.left + SIDE_INSET, label ? label.right + SIDE_INSET : Number.NEGATIVE_INFINITY);
}

function finePlacement(input: ReactionPlacementInput, left: number): ReactionPlacement | null {
  const { anchor, list, composerTop, height, reach, chipsBottom } = input;
  const fits = (top: number) => top >= list.top && top + height <= composerTop - EDGE;
  const atTop = anchor.top + overlapFor(reach.top) - height;
  if (fits(atTop)) return { left, top: atTop, placement: "straddle" };
  const underChips = chipsBottom === undefined ? Number.NEGATIVE_INFINITY : chipsBottom + EDGE;
  const atBottom = Math.max(anchor.bottom - overlapFor(reach.bottom), underChips);
  if (fits(atBottom)) return { left, top: atBottom, placement: "below" };
  return input.allowCover ? coveringPlacement(input, left) : null;
}

const overlapFor = (limit: number) => Math.max(0, Math.min(BAR_OVERHANG, limit));

function coveringPlacement(input: ReactionPlacementInput, left: number): ReactionPlacement | null {
  const { anchor, list, composerTop, height } = input;
  let top: number;
  let placement: ReactionPlacement["placement"] = "straddle";
  if (anchor.top - list.top < PARTLY_SCROLLED) {
    top = input.chipsBottom !== undefined ? input.chipsBottom + EDGE : anchor.bottom - overlapFor(input.reach.bottom);
    placement = "below";
    if (top < list.top) return null;
  } else {
    top = Math.max(anchor.top - BAR_OVERHANG, list.top);
  }
  if (top + height > composerTop - EDGE) top = composerTop - EDGE - height;
  return { left, top, placement };
}

function touchPlacement(input: ReactionPlacementInput, left: number): ReactionPlacement {
  const { anchor, list, height } = input;
  const above = anchor.top - height - EDGE;
  if (above >= list.top + EDGE) return { left, top: above, placement: "above" };
  return { left, top: anchor.bottom + EDGE, placement: "below" };
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(value, Math.max(min, max)));
}

export type BarRoute = "native" | "text";
export type BarMetricsInput = { route: BarRoute; coarse: boolean; listWidth: number; capture?: boolean };
export type BarMetrics = { width: number; height: number; quick: number };

const SEGMENT_WIDTH = 52;
const CAPTURE_WIDTH = 248;

/**
 * The bar's measured size. Buttons are 28px (fine) or 44px (coarse) with a 2px gap, 1px padding and 1px border.
 * The number of quick reactions shrinks with the list: 6, then 4 under 328px (340px coarse), then 3 under 248px coarse.
 */
export function barMetrics({ route, coarse, listWidth, capture = false }: BarMetricsInput): BarMetrics {
  if (capture) return { width: CAPTURE_WIDTH, height: coarse ? 104 : 88, quick: 0 };
  const quick = coarse
    ? (listWidth < 248 ? 3 : listWidth < 340 ? 4 : 6)
    : (listWidth < 328 ? 4 : 6);
  const button = coarse ? 44 : 28;
  const buttonRow = (quick + 1) * button + quick * 2 + 4;
  const segment = route === "text" ? SEGMENT_WIDTH : 0;
  return { width: buttonRow + segment, height: coarse ? 48 : 32, quick };
}
