import type { GatewayAvailability } from "@peppy/desktop-ui/bridge-contracts";
import { Check, ChevronDown, Clock, Radio, TriangleAlert } from "@peppy/desktop-ui";
import { createPortal } from "react-dom";
import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import "./GatewayPicker.css";

export type GatewayPickerOption = {
  value: string;
  name: string;
  simLabel?: string;
  statusLabel?: string;
  simulated?: boolean;
  availability?: GatewayAvailability;
  disabled?: boolean;
  gatewayId?: string;
  simId?: string;
};

export type GatewayPickerProps = {
  options: GatewayPickerOption[];
  value: string;
  placeholder: string;
  disabled?: boolean;
  onChange(value: string): void;
  describedBy?: string;
  invalid?: boolean;
};

type PopupPosition = Pick<CSSProperties, "top" | "left" | "width" | "maxHeight" | "transform">;

const TYPEAHEAD_RESET_MS = 500;
const POPUP_GAP_PX = 6;
const VIEWPORT_GUTTER_PX = 8;
const PREFERRED_POPUP_HEIGHT_PX = 280;

function selectedOption(options: GatewayPickerOption[], value: string) {
  return options.find((option) => option.value === value);
}

function firstEnabledOption(options: GatewayPickerOption[]) {
  return options.find((option) => !option.disabled);
}

function statusIcon(availability: GatewayAvailability | undefined): ReactNode {
  if (availability === "connected") return <Check aria-hidden size={12} />;
  if (availability === "delayed") return <Clock aria-hidden size={12} />;
  if (availability === "unknown" || availability === "capabilities_unknown") return <Radio aria-hidden size={12} />;
  if (availability && availability !== "offline") return <TriangleAlert aria-hidden size={12} />;
  return null;
}

function optionSearchText(option: GatewayPickerOption) {
  return [option.name, option.simLabel, option.statusLabel].filter(Boolean).join(" ").toLocaleLowerCase();
}

function nextEnabledOption(options: GatewayPickerOption[], activeValue: string | undefined, direction: 1 | -1) {
  const startIndex = Math.max(0, options.findIndex((option) => option.value === activeValue));
  for (let offset = 1; offset <= options.length; offset += 1) {
    const option = options[(startIndex + (offset * direction) + options.length) % options.length];
    if (!option.disabled) return option;
  }
  return undefined;
}

function optionId(listboxId: string, value: string) {
  return `${listboxId}-option-${encodeURIComponent(value)}`;
}

function routeBadges(option: GatewayPickerOption, id?: string) {
  const showStatus = option.statusLabel && option.statusLabel !== option.simLabel;
  return (
    <span id={id} data-gateway-badges>
      <span data-route-badge="device">{option.name}</span>
      {option.simLabel && <span data-route-badge="sim">{option.simLabel}</span>}
      {showStatus && <span data-route-badge="status" data-gateway-availability={option.availability}>{statusIcon(option.availability)}{option.statusLabel}</span>}
      {option.simulated && <span data-route-badge="simulated">Simulated</span>}
    </span>
  );
}

export function GatewayPicker({ options, value, placeholder, disabled = false, onChange, describedBy, invalid = false }: GatewayPickerProps): React.JSX.Element {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popupRef = useRef<HTMLDivElement>(null);
  const typeaheadTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const typeaheadRef = useRef("");
  const listboxId = useId();
  const [open, setOpen] = useState(false);
  const [activeValue, setActiveValue] = useState<string>();
  const [popupPosition, setPopupPosition] = useState<PopupPosition>();
  const popupVisible = open && Boolean(popupPosition);
  const selected = selectedOption(options, value);
  const canOpen = !disabled && options.length > 0;
  const activeCandidate = selectedOption(options, activeValue ?? "");
  const active = activeCandidate && !activeCandidate.disabled
    ? activeCandidate
    : (selected && !selected.disabled ? selected : firstEnabledOption(options));
  const activeOptionId = active ? optionId(listboxId, active.value) : undefined;
  const summaryId = `${listboxId}-summary`;
  const describedByIds = [summaryId, describedBy].filter(Boolean).join(" ");

  const close = () => {
    setOpen(false);
    typeaheadRef.current = "";
    if (typeaheadTimerRef.current) clearTimeout(typeaheadTimerRef.current);
  };

  const openWith = (option: GatewayPickerOption | undefined) => {
    if (!canOpen) return;
    setActiveValue(option?.value ?? firstEnabledOption(options)?.value);
    setOpen(true);
  };

  const commit = (option: GatewayPickerOption | undefined) => {
    if (!option || option.disabled) return;
    onChange(option.value);
    close();
    triggerRef.current?.focus();
  };

  useEffect(() => () => {
    if (typeaheadTimerRef.current) clearTimeout(typeaheadTimerRef.current);
  }, []);

  useEffect(() => {
    if (!canOpen) {
      close();
      return;
    }
    if (open && (!activeCandidate || activeCandidate.disabled)) setActiveValue(firstEnabledOption(options)?.value);
  }, [activeValue, canOpen, open, options]);

  useLayoutEffect(() => {
    if (!open) return;
    const positionPopup = () => {
      const rect = triggerRef.current?.getBoundingClientRect();
      if (!rect) return;
      const viewportWidth = window.innerWidth;
      const viewportHeight = window.innerHeight;
      const below = Math.max(0, viewportHeight - rect.bottom - VIEWPORT_GUTTER_PX - POPUP_GAP_PX);
      const above = Math.max(0, rect.top - VIEWPORT_GUTTER_PX - POPUP_GAP_PX);
      const placeAbove = below < Math.min(PREFERRED_POPUP_HEIGHT_PX, above) && above > below;
      const maxHeight = placeAbove ? above : below;
      const width = Math.min(Math.max(rect.width, 220), viewportWidth - (VIEWPORT_GUTTER_PX * 2));
      setPopupPosition({
        top: placeAbove ? rect.top - POPUP_GAP_PX : rect.bottom + POPUP_GAP_PX,
        left: Math.max(VIEWPORT_GUTTER_PX, Math.min(rect.left, viewportWidth - width - VIEWPORT_GUTTER_PX)),
        width,
        maxHeight,
        transform: placeAbove ? "translateY(-100%)" : undefined,
      });
    };
    positionPopup();
    window.addEventListener("resize", positionPopup);
    document.addEventListener("scroll", positionPopup, true);
    const resizeObserver = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(positionPopup);
    if (triggerRef.current) resizeObserver?.observe(triggerRef.current);
    return () => {
      window.removeEventListener("resize", positionPopup);
      document.removeEventListener("scroll", positionPopup, true);
      resizeObserver?.disconnect();
    };
  }, [open]);

  useLayoutEffect(() => {
    if (!popupVisible || !activeOptionId) return;
    const activeOption = document.getElementById(activeOptionId) as (HTMLElement & { scrollIntoView?: (options: ScrollIntoViewOptions) => void }) | null;
    activeOption?.scrollIntoView?.({ block: "nearest" });
  }, [activeOptionId, popupVisible]);

  useEffect(() => {
    if (!open) return;
    const closeOutside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!triggerRef.current?.contains(target) && !popupRef.current?.contains(target)) close();
    };
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [open]);

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    const first = firstEnabledOption(options);
    const last = [...options].reverse().find((option) => !option.disabled);
    if (!open && ["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      openWith(event.key === "ArrowUp" || event.key === "End" ? last : first);
      return;
    }
    if (!open) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setActiveValue(nextEnabledOption(options, active?.value, event.key === "ArrowDown" ? 1 : -1)?.value);
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      setActiveValue((event.key === "Home" ? first : last)?.value);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      commit(active);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
      return;
    }
    if (event.key === "Tab") {
      close();
      return;
    }
    if (event.key.length !== 1 || event.altKey || event.ctrlKey || event.metaKey) return;
    typeaheadRef.current += event.key.toLocaleLowerCase();
    if (typeaheadTimerRef.current) clearTimeout(typeaheadTimerRef.current);
    typeaheadTimerRef.current = setTimeout(() => { typeaheadRef.current = ""; }, TYPEAHEAD_RESET_MS);
    const match = options.find((option) => !option.disabled && optionSearchText(option).startsWith(typeaheadRef.current));
    if (match) setActiveValue(match.value);
  };

  const popupHost = triggerRef.current?.closest<HTMLElement>(".theme-light, .theme-dark, .theme-system") ?? document.body;
  const popup = open && popupHost && popupPosition && (
    <div ref={popupRef} id={listboxId} role="listbox" aria-label="Phone routes" data-gateway-picker-popup style={popupPosition}>
      {options.map((option) => (
        <div
          key={option.value}
          id={optionId(listboxId, option.value)}
          role="option"
          aria-selected={option.value === value}
          aria-disabled={option.disabled || undefined}
          data-gateway-id={option.gatewayId}
          data-sim-id={option.simId}
          data-gateway-availability={option.availability}
          data-gateway-choice={option.value}
          data-active={option.value === active?.value || undefined}
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => commit(option)}
        >
          {routeBadges(option)}
          {option.value === value && <Check aria-hidden size={14} />}
        </div>
      ))}
    </div>
  );

  return (
    <span data-gateway-picker>
      <button
        ref={triggerRef}
        type="button"
        role="combobox"
        aria-label="Phone"
        aria-expanded={open}
        aria-haspopup="listbox"
        aria-controls={open ? listboxId : undefined}
        aria-activedescendant={open ? activeOptionId : undefined}
        aria-describedby={describedByIds}
        aria-invalid={invalid || undefined}
        disabled={!canOpen}
        data-gateway-picker-trigger
        onClick={() => open ? close() : openWith(selected && !selected.disabled ? selected : undefined)}
        onKeyDown={handleKeyDown}
        onBlur={(event) => {
          const next = event.relatedTarget as Node | null;
          if (next && (triggerRef.current?.contains(next) || popupRef.current?.contains(next))) return;
          close();
        }}
      >
        {selected ? routeBadges(selected, summaryId) : <span id={summaryId} data-gateway-picker-placeholder>{placeholder}</span>}
        <ChevronDown aria-hidden size={14} />
      </button>
      {popup && createPortal(popup, popupHost)}
    </span>
  );
}
