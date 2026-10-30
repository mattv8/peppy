const pixels = (value: string | undefined): number => {
  const parsed = Number.parseFloat(value ?? "");
  return Number.isFinite(parsed) ? parsed : 0;
};

const nonNegative = (value: number): number =>
  Number.isFinite(value) && value > 0 ? value : 0;

const isFlexContainer = (style: CSSStyleDeclaration): boolean =>
  style.display === "flex" || style.display === "inline-flex";

const isRowFlexContainer = (style: CSSStyleDeclaration): boolean =>
  isFlexContainer(style) && (style.flexDirection ?? "").startsWith("row");

const takesVerticalSpace = (element: HTMLElement): boolean => {
  if (element.hidden) return false;
  const style = getComputedStyle(element);
  return (
    style.display !== "none" &&
    style.position !== "absolute" &&
    style.position !== "fixed"
  );
};

const blockEdges = (element: HTMLElement): number => {
  const style = getComputedStyle(element);
  return (
    pixels(style.paddingTop) +
    pixels(style.paddingBottom) +
    pixels(style.borderTopWidth) +
    pixels(style.borderBottomWidth)
  );
};

/** Vertical space a container adds around `current`, excluding `current` itself. */
const stackedChrome = (container: HTMLElement, current: HTMLElement): number => {
  const style = getComputedStyle(container);
  if (isRowFlexContainer(style)) return 0;
  let siblingHeights = 0;
  let stacked = 0;
  for (const child of Array.from(container.children)) {
    if (!(child instanceof HTMLElement) || !takesVerticalSpace(child)) continue;
    stacked += 1;
    if (child === current) continue;
    const childStyle = getComputedStyle(child);
    siblingHeights +=
      child.offsetHeight + pixels(childStyle.marginTop) + pixels(childStyle.marginBottom);
  }
  const gaps = isFlexContainer(style)
    ? Math.max(0, stacked - 1) * pixels(style.rowGap)
    : 0;
  return siblingHeights + gaps;
};

const requiredChrome = (area: HTMLElement, textarea: HTMLElement): number => {
  let chrome = 0;
  let current: HTMLElement = textarea;
  let container = textarea.parentElement;
  while (container) {
    chrome += blockEdges(container) + stackedChrome(container, current);
    if (container === area) return chrome;
    current = container;
    container = container.parentElement;
  }
  return 0;
};

/**
 * Non-textarea vertical chrome of the composer, from laid-out children and
 * ancestor edges rather than the clipped outer height. The textarea's own
 * height and padding are excluded; callers add one textarea line separately.
 */
export function measureComposerChrome(area: HTMLElement, textarea: HTMLElement): number {
  const outerMinusText = nonNegative(area.offsetHeight - textarea.offsetHeight);
  return nonNegative(Math.max(outerMinusText, requiredChrome(area, textarea)));
}
