import { useEffect, useRef, type CSSProperties, type KeyboardEvent, type RefObject } from "react";

export type BookMenuItem = {
  action: string;
  label: string;
  /** Shown in the error colour. */
  destructive?: boolean;
  /** Set when the item cannot run now: it stays visible, is marked disabled, and its choice does nothing. */
  disabledReason?: string;
  onSelect(): void;
};

export type BookMenuEntry = BookMenuItem | "separator";

export type BookMenuPoint = { x: number; y: number };

const MENU_WIDTH = 220;
const MENU_ITEM_HEIGHT = 32;
const MENU_SEPARATOR_HEIGHT = 9;
const MENU_PADDING = 8;
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(value, max));
}

/** Keeps the menu's left edge inside the rail (clear of the app's icon rail) and inside the window. */
function clampMenuLeft(left: number, railLeft: number): number {
  return clamp(left, railLeft + MENU_GAP, window.innerWidth - MENU_WIDTH - MENU_GAP);
}

/** Opens under a ⋯ button, right-aligned to it. */
export function underButtonPoint(button: DOMRect, railLeft: number): BookMenuPoint {
  return { x: clampMenuLeft(button.right - MENU_WIDTH, railLeft), y: button.bottom + MENU_GAP };
}

/** Context-menu point: the pointer position, clamped horizontally like the ⋯ placement. */
export function pointerMenuPoint(clientX: number, clientY: number, railLeft: number): BookMenuPoint {
  return { x: clampMenuLeft(clientX, railLeft), y: clientY };
}

function bookMenuPosition(point: BookMenuPoint, entries: BookMenuEntry[]): CSSProperties {
  const height = entries.reduce((total, entry) => total + (entry === "separator" ? MENU_SEPARATOR_HEIGHT : MENU_ITEM_HEIGHT), MENU_PADDING * 2);
  return {
    position: "fixed",
    left: point.x,
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
  items: BookMenuEntry[];
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
      style={bookMenuPosition(point, items)}
      onKeyDown={handleKeyDown}
    >
      {items.map(item => item === "separator" ? (
        <div key="separator" role="separator" className="contacts-book-menu-separator" />
      ) : (
        <button
          key={item.action}
          type="button"
          role="menuitem"
          data-action={item.action}
          className={item.destructive ? "contacts-book-menu-item contacts-book-menu-item-destructive" : "contacts-book-menu-item"}
          aria-disabled={item.disabledReason ? true : undefined}
          title={item.disabledReason}
          onClick={() => {
            if (item.disabledReason) return;
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
