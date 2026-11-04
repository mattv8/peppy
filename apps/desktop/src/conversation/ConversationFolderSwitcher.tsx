import { useEffect, useRef, useState, type FocusEvent, type JSX, type KeyboardEvent, type MouseEvent } from "react";
import { ChevronDown } from "@peppy/desktop-ui";
import type { ConversationFolder } from "@peppy/desktop-ui/bridge-contracts";

type Props = { value: ConversationFolder; counts: Record<ConversationFolder, number>; unread: Record<ConversationFolder, number>; onChange(folder: ConversationFolder): void; disabled?: boolean };

const FOLDERS: ConversationFolder[] = ["inbox", "archived", "spam"];
const LABELS: Record<ConversationFolder, string> = { inbox: "Inbox", archived: "Archived", spam: "Spam" };

export function ConversationFolderSwitcher({ value, counts, unread, onChange, disabled = false }: Props): JSX.Element {
  const [open, setOpen] = useState(false);
  const [focusIndex, setFocusIndex] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const selectedIndex = FOLDERS.indexOf(value);

  const close = (restoreFocus = false) => {
    setOpen(false);
    if (restoreFocus) requestAnimationFrame(() => triggerRef.current?.focus());
  };
  const openMenu = (index: number) => { if (!disabled) { setFocusIndex(index); setOpen(true); } };
  useEffect(() => { if (open) itemRefs.current[focusIndex]?.focus(); }, [focusIndex, open]);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => { if (!rootRef.current?.contains(event.target as Node)) close(); };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);

  const onRootBlur = (event: FocusEvent<HTMLDivElement>) => {
    const nextFocus = event.relatedTarget;
    if (nextFocus instanceof Node && rootRef.current?.contains(nextFocus)) return;
    close();
  };
  const keepFocusOnMenuPress = (event: MouseEvent<HTMLDivElement>) => event.preventDefault();
  const onTriggerKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Enter" || event.key === " " || event.key === "ArrowDown") { event.preventDefault(); openMenu(selectedIndex); }
    if (event.key === "ArrowUp") { event.preventDefault(); openMenu(FOLDERS.length - 1); }
  };
  const onMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(true); return; }
    if (event.key === "Tab") { close(); return; }
    if (event.key === "Home") { event.preventDefault(); setFocusIndex(0); return; }
    if (event.key === "End") { event.preventDefault(); setFocusIndex(FOLDERS.length - 1); return; }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setFocusIndex(current => (current + (event.key === "ArrowDown" ? 1 : -1) + FOLDERS.length) % FOLDERS.length);
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const index = itemRefs.current.findIndex(item => item === document.activeElement);
      if (index >= 0) select(FOLDERS[index]);
    }
  };
  const select = (folder: ConversationFolder) => { onChange(folder); close(true); };
  const label = LABELS[value];

  return <div ref={rootRef} id="conversation-folder-switcher" onBlur={onRootBlur}><button ref={triggerRef} id="conversation-folder-trigger" data-folder={value} type="button" aria-haspopup="menu" aria-expanded={open} {...(open ? { "aria-controls": "conversation-folder-menu" } : {})} aria-disabled={disabled || undefined} disabled={disabled} onClick={() => open ? close() : openMenu(selectedIndex)} onKeyDown={onTriggerKeyDown}><span className="visually-hidden">Folder: </span><span className="conversation-folder-trigger-label">{label}</span>{counts[value] > 0 && <span className="conversation-folder-trigger-count">{counts[value]}</span>}<ChevronDown size={14} aria-hidden /></button>{open && <div id="conversation-folder-menu" role="menu" aria-label="Folder" onKeyDown={onMenuKeyDown} onMouseDown={keepFocusOnMenuPress}>{FOLDERS.map((folder, index) => <button key={folder} ref={element => { itemRefs.current[index] = element; }} type="button" role="menuitemradio" aria-checked={folder === value} tabIndex={index === focusIndex ? 0 : -1} onClick={() => select(folder)}><span>{LABELS[folder]}</span><span className="conversation-folder-menu-count">{folder !== "inbox" && unread[folder] > 0 && <><span className="conversation-folder-unread-dot" aria-hidden /><span className="visually-hidden">has unread messages</span></>}{counts[folder]}</span></button>)}</div>}</div>;
}
