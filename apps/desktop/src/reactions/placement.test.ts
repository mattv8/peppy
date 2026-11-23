import { describe, expect, it } from "vitest";
import { barMetrics, placeReactionBar, type ReactionPlacementInput } from "./placement";

const base: ReactionPlacementInput = {
  anchor: { top: 200, bottom: 260, left: 100, right: 300 },
  outgoing: false,
  list: { top: 0, bottom: 800, left: 0, right: 400 },
  composerTop: 700,
  viewport: { top: 0, bottom: 768 },
  width: 212,
  height: 32,
  touch: false,
  reach: { top: 11, bottom: 11 },
  allowCover: false,
};

describe("placeReactionBar, fine pointer", () => {
  it("straddles the top edge of an incoming bubble, 8px inside its left edge", () => {
    expect(placeReactionBar(base)).toEqual({ left: 108, top: 179, placement: "straddle" });
  });

  it("anchors an outgoing bubble to its right edge", () => {
    expect(placeReactionBar({ ...base, outgoing: true })).toEqual({ left: 300 - 8 - 212, top: 179, placement: "straddle" });
  });

  it("starts incoming bars 8px from the bubble in group rows too, with no sender-gutter offset", () => {
    // The bubble rect already includes the avatar gutter, so no extra indent is added (designer item 6).
    expect(placeReactionBar({ ...base, anchor: { top: 200, bottom: 260, left: 140, right: 340 } })?.left).toBe(148);
  });

  it("starts clear of the sender label when the label sits above and to the left of the bar", () => {
    const placed = placeReactionBar({ ...base, list: { ...base.list, right: 600 }, label: { top: 180, bottom: 198, left: 100, right: 230 } });
    expect(placed?.left).toBe(238);
  });

  it("moves below the first row when its top edge cannot fit", () => {
    expect(placeReactionBar({ ...base, anchor: { top: 16, bottom: 76, left: 100, right: 300 } })).toEqual({ left: 108, top: 65, placement: "below" });
  });

  it("moves below the bubble when it is partly scrolled off the top, instead of covering its text", () => {
    expect(placeReactionBar({ ...base, anchor: { top: -10, bottom: 50, left: 100, right: 300 } })).toEqual({ left: 108, top: 39, placement: "below" });
  });

  it("starts a below bar under the chip row, not over its last chip", () => {
    const placed = placeReactionBar({ ...base, anchor: { top: -10, bottom: 50, left: 100, right: 300 }, chipsBottom: 70 });
    expect(placed).toEqual({ left: 108, top: 74, placement: "below" });
  });

  it("hides when the bubble is wholly above the list, or its top is under the composer", () => {
    expect(placeReactionBar({ ...base, anchor: { top: -90, bottom: -30, left: 100, right: 300 } })).toBeNull();
    expect(placeReactionBar({ ...base, anchor: { top: 702, bottom: 760, left: 100, right: 300 } })).toBeNull();
  });

  it("clamps above the composer top when the bar would overlap it", () => {
    expect(placeReactionBar({ ...base, anchor: { top: 690, bottom: 720, left: 100, right: 300 } })).toBeNull();
    expect(placeReactionBar({ ...base, allowCover: true, anchor: { top: 690, bottom: 720, left: 100, right: 300 } })?.top).toBe(664);
  });

  it("caps overlap at the edge and at controls", () => {
    expect(placeReactionBar({ ...base, reach: { top: 24, bottom: 11 } })?.top).toBe(184);
    expect(placeReactionBar({ ...base, reach: { top: 4, bottom: 11 } })?.top).toBe(172);
    expect(placeReactionBar({ ...base, reach: { top: 0, bottom: 0 } })?.top).toBe(168);
  });

  it("hides a hover bar for a tall bubble but allows explicitly requested covering", () => {
    const anchor = { top: -40, bottom: 720, left: 100, right: 300 };
    expect(placeReactionBar({ ...base, anchor })).toBeNull();
    expect(placeReactionBar({ ...base, anchor, allowCover: true })?.top).toBe(664);
  });

  it("keeps every visible hover bar outside text and within vertical bounds", () => {
    for (const height of [60, 300]) for (let top = -100; top <= 760; top += 10) {
      const anchor = { top, bottom: top + height, left: 100, right: 300 };
      const placed = placeReactionBar({ ...base, anchor });
      if (!placed) continue;
      expect(placed.top).toBeGreaterThanOrEqual(0);
      expect(placed.top + 32).toBeLessThanOrEqual(696);
      expect(placed.top + 32 <= anchor.top + 11 || placed.top >= anchor.bottom - 11).toBe(true);
    }
  });

  it("keeps the bar inside the list's horizontal bounds with an 8px inset", () => {
    expect(placeReactionBar({ ...base, anchor: { top: 200, bottom: 260, left: -40, right: 40 } })?.left).toBe(8);
    expect(placeReactionBar({ ...base, outgoing: true, anchor: { top: 200, bottom: 260, left: 300, right: 420 } })?.left).toBe(400 - 8 - 212);
  });
});

describe("placeReactionBar, touch", () => {
  const touch = { ...base, list: { ...base.list, right: 600 }, touch: true, height: 48, width: 324 };

  it("sits above the bubble as a menu, never straddling it", () => {
    expect(placeReactionBar(touch)).toEqual({ left: 108, top: 200 - 48 - 4, placement: "above" });
  });

  it("flips below when there is no room above, at a 4px gap", () => {
    expect(placeReactionBar({ ...touch, anchor: { top: 10, bottom: 60, left: 100, right: 300 } })).toEqual({ left: 108, top: 64, placement: "below" });
  });

  it("clamps an outgoing bar wider than the bubble to the list's right edge, not off the screen", () => {
    // Pixel 7 case from the designer: list 0..348, bubble right 340. The 324px bar must end at 348 - 8.
    const placed = placeReactionBar({ ...touch, outgoing: true, list: { top: 0, bottom: 800, left: 0, right: 348 }, anchor: { top: 200, bottom: 260, left: 200, right: 340 } });
    expect(placed?.left).toBe(8);
    expect((placed?.left ?? 0) + 324).toBeLessThanOrEqual(348 - 8);
  });

  it("clamps the left edge when a narrow list cannot fit the bar on the bubble's side", () => {
    expect(placeReactionBar({ ...touch, outgoing: true, list: { top: 0, bottom: 800, left: 0, right: 348 }, anchor: { top: 200, bottom: 260, left: 0, right: 320 } })?.left).toBe(8);
  });
});

describe("barMetrics", () => {
  it("measures the fine-pointer bar at 212px with six quick reactions, and 152px compact", () => {
    expect(barMetrics({ route: "native", coarse: false, listWidth: 771 })).toEqual({ width: 212, height: 32, quick: 6 });
    expect(barMetrics({ route: "native", coarse: false, listWidth: 300 })).toEqual({ width: 152, height: 32, quick: 4 });
  });

  it("adds the 52px Text segment to the text route", () => {
    expect(barMetrics({ route: "text", coarse: false, listWidth: 771 }).width).toBe(264);
  });

  it("measures the coarse bar at 324px (7 x 44px) and 232px compact under 340px", () => {
    expect(barMetrics({ route: "native", coarse: true, listWidth: 412 })).toEqual({ width: 324, height: 48, quick: 6 });
    expect(barMetrics({ route: "native", coarse: true, listWidth: 330 })).toEqual({ width: 232, height: 48, quick: 4 });
  });

  it("drops to three quick reactions on coarse pointers under 248px", () => {
    expect(barMetrics({ route: "native", coarse: true, listWidth: 240 })).toEqual({ width: 186, height: 48, quick: 3 });
  });

  it("sizes the capture panel the same for both pointers, with a taller coarse panel", () => {
    expect(barMetrics({ route: "native", coarse: false, listWidth: 771, capture: true }).width).toBe(248);
    expect(barMetrics({ route: "native", coarse: true, listWidth: 771, capture: true }).height).toBe(104);
  });
});
