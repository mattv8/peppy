import { useEffect, useLayoutEffect, useRef, useState, type ButtonHTMLAttributes, type CSSProperties, type KeyboardEvent, type ReactElement, type Ref } from "react";
import { Check, ChevronDown, Palette } from "lucide-react";

export const TONE_PRESETS = [
  { index: 61, label: "Rose" }, { index: 7, label: "Coral" }, { index: 60, label: "Red" }, { index: 15, label: "Orange" },
  { index: 19, label: "Amber" }, { index: 78, label: "Gold" }, { index: 9, label: "Mustard" }, { index: 31, label: "Lime" },
  { index: 12, label: "Green" }, { index: 32, label: "Emerald" }, { index: 37, label: "Aqua" }, { index: 27, label: "Teal" },
  { index: 4, label: "Sky" }, { index: 71, label: "Cyan" }, { index: 99, label: "Blue" }, { index: 43, label: "Periwinkle" },
  { index: 115, label: "Indigo" }, { index: 47, label: "Violet" }, { index: 55, label: "Purple" }, { index: 3, label: "Orchid" },
  { index: 111, label: "Magenta" }, { index: 26, label: "Pink" }, { index: 0, label: "Clay" }, { index: 89, label: "Slate" },
] as const;

const TONE_GRID_COLUMNS = 6;

type TonePickerProps = {
  id: string;
  seed: string;
  value?: number;
  automaticIndex: number;
  disabled?: boolean;
  pending?: boolean;
  error?: string;
  onSelect(index: number | null): void;
  variant?: "icon" | "field";
  describedBy?: string;
  renderTrigger?(props: ButtonHTMLAttributes<HTMLButtonElement> & { ref: Ref<HTMLButtonElement> }): ReactElement;
  disabledReason?: string;
};

const optionLabel = (index: number | undefined) => TONE_PRESETS.find((preset) => preset.index === index)?.label ?? (index === undefined ? "Automatic" : `Color ${index + 1}`);

export function TonePicker({ id, seed, value, automaticIndex, disabled = false, pending = false, error, onSelect, variant = "icon", describedBy, renderTrigger, disabledReason = "Unlock Peppy to change colors." }: TonePickerProps) {
  const wrapper = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [focusIndex, setFocusIndex] = useState(0);
  const [position, setPosition] = useState<CSSProperties>();
  const [announcement, setAnnouncement] = useState("");
  const requested = useRef<number | null | undefined>(undefined);
  const wasPending = useRef(false);
  const previousSeed = useRef(seed);
  const options = [{ index: null, label: "Automatic" }, ...TONE_PRESETS, ...(value !== undefined && !TONE_PRESETS.some((preset) => preset.index === value) ? [{ index: value, label: optionLabel(value) }] : [])];
  const checkedIndex = Math.max(0, options.findIndex((option) => option.index === (value ?? null)));

  useEffect(() => {
    if (previousSeed.current === seed) return;
    previousSeed.current = seed;
    setOpen(false);
    setFocusIndex(0);
  }, [seed]);
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Node && !wrapper.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);
  useEffect(() => {
    if (!open) return;
    setFocusIndex(checkedIndex);
    requestAnimationFrame(() => wrapper.current?.querySelector<HTMLButtonElement>(`[data-tone-option-index="${checkedIndex}"]`)?.focus());
  }, [open, checkedIndex]);
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const rect = trigger.current?.getBoundingClientRect();
      if (!rect) return;
      const width = Math.min(window.innerWidth <= 600 ? 260 : 236, window.innerWidth - 16);
      const below = window.innerHeight - rect.bottom >= 280;
      setPosition({ position: "fixed", zIndex: 50, width, left: variant === "field" ? Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)) : Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)), top: below ? rect.bottom + 4 : undefined, bottom: below ? undefined : window.innerHeight - rect.top + 4 });
    };
    place(); window.addEventListener("resize", place); window.addEventListener("scroll", place, true);
    return () => { window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true); };
  }, [open, variant]);
  useEffect(() => {
    if (wasPending.current && !pending && !error && requested.current === (value ?? null)) {
      const active = document.activeElement;
      const restoreFocus = active instanceof Node && wrapper.current?.contains(active);
      setOpen(false);
      setAnnouncement(`Conversation color set to ${optionLabel(value)}`);
      if (restoreFocus) requestAnimationFrame(() => trigger.current?.focus());
      requested.current = undefined;
    }
    wasPending.current = pending;
  }, [error, pending, value]);

  const close = (restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) requestAnimationFrame(() => trigger.current?.focus());
  };
  const select = (index: number | null) => {
    if (disabled || pending) return;
    if (index === (value ?? null)) return close(true);
    requested.current = index;
    onSelect(index);
  };
  const onOptionKeyDown = (event: KeyboardEvent<HTMLButtonElement>, position: number) => {
    let next = position;
    if (event.key === "ArrowLeft") next = (position + options.length - 1) % options.length;
    else if (event.key === "ArrowRight") next = (position + 1) % options.length;
    else if (event.key === "ArrowUp") next = position === 0 ? options.length - 1 : position <= TONE_GRID_COLUMNS ? 0 : position - TONE_GRID_COLUMNS;
    else if (event.key === "ArrowDown") next = position === 0 ? 1 : Math.min(options.length - 1, position + TONE_GRID_COLUMNS);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = options.length - 1;
    else if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(options[position].index); return; }
    else if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(true); return; }
    else return;
    event.preventDefault();
    setFocusIndex(next);
    wrapper.current?.querySelector<HTMLButtonElement>(`[data-tone-option-index="${next}"]`)?.focus();
  };
  const selectedLabel = optionLabel(value);
  const effectiveIndex = value ?? automaticIndex;

  const triggerProps = { id: `${id}-trigger`, ref: trigger, type: "button" as const, "aria-haspopup": "dialog" as const, "aria-expanded": open, "aria-controls": `${id}-panel`, "aria-describedby": describedBy, "aria-label": variant === "field" ? `Conversation color: ${selectedLabel}` : undefined, "aria-disabled": disabled || pending || undefined, disabled: false, title: disabled ? disabledReason : undefined, className: `tone-picker-trigger tone-picker-${variant}`, onClick: () => { if (!disabled && !pending) setOpen((current) => !current); } };
  return <div id={id} ref={wrapper} data-tone-picker data-state={open ? "open" : "closed"} data-tone-pending={pending || undefined} data-tone-error={error ? true : undefined} onBlur={(event) => { if (open && event.relatedTarget instanceof Node && !event.currentTarget.contains(event.relatedTarget)) setOpen(false); }}>
    {renderTrigger ? renderTrigger(triggerProps) : <button {...triggerProps}>
      {variant === "icon" ? <><Palette size={16} aria-hidden /><span aria-hidden data-tone-picker-dot style={{ background: `var(--peppy-avatar-${effectiveIndex})` }} /></> : <><span aria-hidden data-tone-picker-swatch style={{ background: `var(--peppy-avatar-${effectiveIndex})` }} /> <span>{selectedLabel}</span><ChevronDown size={16} aria-hidden /></>}
      {variant === "icon" && <span className="sr-only">Change conversation color</span>}
    </button>}
    <span className="sr-only" role="status">{pending ? "Saving…" : announcement}</span>
    <div id={`${id}-panel`} style={position} data-tone-picker-panel role="dialog" aria-label="Conversation color" hidden={!open}>
      <div id={`${id}-group`} role="radiogroup" aria-label="Conversation color" aria-describedby={`${id}-status`} aria-busy={pending}>
        <span id={`${id}-heading`}>Conversation color</span>
        <ToneOption option={options[0]} automaticIndex={automaticIndex} position={0} selected={checkedIndex === 0} focused={focusIndex === 0} pending={pending} onClick={select} onKeyDown={onOptionKeyDown} />
        <div data-tone-picker-grid>{options.slice(1).map((option, offset) => <ToneOption key={option.index} option={option} automaticIndex={automaticIndex} position={offset + 1} selected={checkedIndex === offset + 1} focused={focusIndex === offset + 1} pending={pending} onClick={select} onKeyDown={onOptionKeyDown} />)}</div>
      </div>
      <p id={`${id}-status`}>{pending ? "Saving…" : ""}</p>
      {error && <p role="alert" data-tone-error>{error}</p>}
    </div>
  </div>;
}

function ToneOption({ option, automaticIndex, position, selected, focused, pending, onClick, onKeyDown }: { option: { index: number | null; label: string }; automaticIndex: number; position: number; selected: boolean; focused: boolean; pending: boolean; onClick(index: number | null): void; onKeyDown(event: KeyboardEvent<HTMLButtonElement>, position: number): void }) {
  const swatchIndex = option.index ?? automaticIndex;
  return <button type="button" role="radio" data-tone-option={option.index ?? "auto"} data-tone-option-index={position} aria-label={option.label} title={option.label} aria-checked={selected} aria-disabled={pending || undefined} tabIndex={focused ? 0 : -1} onClick={() => onClick(option.index)} onKeyDown={(event) => onKeyDown(event, position)} style={{ "--tone-swatch": `var(--peppy-avatar-${swatchIndex})` } as CSSProperties}>{option.index === null && <span>Automatic</span>}{selected && <Check size={14} aria-hidden />}</button>;
}

export type { TonePickerProps };
