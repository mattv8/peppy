import { useRef, type KeyboardEvent } from "react";
import type { WebhookFilterMode } from "@peppy/desktop-ui/bridge-contracts";

const FILTER_MODE_OPTIONS: { value: WebhookFilterMode; label: string }[] = [
  { value: "all", label: "Anyone" },
  { value: "only", label: "Only these contacts" },
  { value: "except", label: "Everyone except" },
];

const STEP_BY_KEY: Record<string, number | undefined> = {
  ArrowRight: 1,
  ArrowDown: 1,
  ArrowLeft: -1,
  ArrowUp: -1,
};

type WebhookFilterModeControlProps = {
  labelledBy: string;
  value: WebhookFilterMode;
  onChange(mode: WebhookFilterMode): void;
};

/** A radiogroup with roving focus: arrow keys select and move focus to the next option. */
export function WebhookFilterModeControl({ labelledBy, value, onChange }: WebhookFilterModeControlProps) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);

  const select = (index: number) => {
    onChange(FILTER_MODE_OPTIONS[index].value);
    buttons.current[index]?.focus();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const count = FILTER_MODE_OPTIONS.length;
    if (event.key === "Home") {
      event.preventDefault();
      select(0);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      select(count - 1);
      return;
    }
    const step = STEP_BY_KEY[event.key];
    if (step === undefined) return;
    event.preventDefault();
    select((index + step + count) % count);
  };

  return (
    <div id="webhook-filter-mode" role="radiogroup" aria-labelledby={labelledBy} className="webhook-segmented">
      {FILTER_MODE_OPTIONS.map((option, index) => {
        const checked = option.value === value;
        return (
          <button
            key={option.value}
            ref={(element) => {
              buttons.current[index] = element;
            }}
            id={`webhook-filter-mode-${option.value}`}
            type="button"
            role="radio"
            aria-checked={checked}
            tabIndex={checked ? 0 : -1}
            className="webhook-segment"
            onClick={() => onChange(option.value)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
