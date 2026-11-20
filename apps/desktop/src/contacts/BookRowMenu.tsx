import { useEffect, useRef, type CSSProperties, type KeyboardEvent, type RefObject } from "react";

export type BookMenuItem = {
  action: string;
  label: string;
  onSelect(): void;
};

export type BookMenuPoint = { x: number; y: number };

const MENU_WIDTH = 220;
const MENU_ITEM_HEIGHT = 32;
const MENU_PADDING = 8;
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;

/** Opens under a ⋯ button, right-aligned to it. */
export function underButtonPoint(rect: DOMRect): BookMenuPoint {
  return { x: rect.right - MENU_WIDTH, y: rect.bottom + MENU_GAP };
}

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(value, max));
}

function bookMenuPosition(point: BookMenuPoint, itemCount: number): CSSProperties {
  const height = itemCount * MENU_ITEM_HEIGHT + MENU_PADDING * 2;
  return {
    position: "fixed",
    left: clamp(point.x, VIEWPORT_MARGIN, window.innerWidth - MENU_WIDTH - VIEWPORT_MARGIN),
    top: clamp(point.y, VIEWPORT_MARGIN, window.innerHeight - height - VIEWPORT_MARGIN),
  };
}

/** Index of the item a navigation key moves to, or undefined when the key is not a menu key. */
function nextMenuIndex(key: string, current: number, count: number): number | undefined {
  if (key === "ArrowDown") return (current + 1) % count;
  if (key === "ArrowUp") return (current - 1 + count) % count;
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  return undefined;
}

/** Row actions for a removed contact book. Closes on Escape, Tab, outside press, or an item choice. */
export function BookRowMenu({ label, items, point, anchorRef, onClose }: {
  label: string;
  items: BookMenuItem[];
  point: BookMenuPoint;
  anchorRef: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const frame = requestAnimationFrame(() => menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus());
    return () => cancelAnimationFrame(frame);
  }, []);

  useEffect(() => {
    const dismissOnOutsidePress = (event: MouseEvent) => {
      const target = event.target as Node;
      if (menuRef.current?.contains(target) || anchorRef.current?.contains(target)) return;
      onClose();
    };
    document.addEventListener("mousedown", dismissOnOutsidePress);
    return () => document.removeEventListener("mousedown", dismissOnOutsidePress);
  }, [anchorRef, onClose]);

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      anchorRef.current?.focus();
      onClose();
      return;
    }
    if (event.key === "Tab") {
      anchorRef.current?.focus();
      onClose();
      return;
    }
    const buttons = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = nextMenuIndex(event.key, current, buttons.length);
    if (next === undefined) return;
    event.preventDefault();
    buttons[next]?.focus();
  };

  return (
    <div
      id="contacts-book-menu"
      ref={menuRef}
      role="menu"
      aria-label={label}
      style={bookMenuPosition(point, items.length)}
      onKeyDown={handleKeyDown}
    >
      {items.map(item => (
        <button
          key={item.action}
          type="button"
          role="menuitem"
          data-action={item.action}
          className="contacts-book-menu-item"
          onClick={() => {
            onClose();
            item.onSelect();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
