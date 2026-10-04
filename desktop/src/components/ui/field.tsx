import { forwardRef, useId } from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";

import type { InputHTMLAttributes, LabelHTMLAttributes, ReactNode, TextareaHTMLAttributes } from "react";

const controlClass =
  "flex w-full rounded-md border border-input bg-background/60 px-3 py-2 text-sm shadow-soft transition-colors placeholder:text-muted-foreground/70 focus-visible:border-ring disabled:cursor-not-allowed disabled:opacity-50 read-only:opacity-70";

export function Label({ className, ...props }: LabelHTMLAttributes<HTMLLabelElement>) {
  return (
    <label
      className={cn("text-xs font-medium text-muted-foreground", className)}
      {...props}
    />
  );
}

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  ({ className, ...props }, ref) => (
    <input ref={ref} className={cn(controlClass, "h-9", className)} {...props} />
  ),
);

Input.displayName = "Input";

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(
  ({ className, ...props }, ref) => (
    <textarea
      ref={ref}
      className={cn(controlClass, "min-h-[80px] resize-y leading-relaxed", className)}
      {...props}
    />
  ),
);

Textarea.displayName = "Textarea";

interface FieldRowProps {
  label: ReactNode;
  hint?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
  className?: string;
}

/** A label, a control, and the explanation under it — the shape of most panels. */
export function FieldRow({ label, hint, htmlFor, children, className }: FieldRowProps) {
  return (
    <div className={cn("space-y-2", className)}>
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {hint ? <p className="text-xs leading-relaxed text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

interface NumberFieldProps {
  label: ReactNode;
  value: number;
  onChange: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  hint?: ReactNode;
  suffix?: ReactNode;
  disabled?: boolean;
}

/**
 * `onChange` receives a parsed, clamped number, so pages never carry a string
 * state that has to be coerced at submit time.
 */
export function NumberField({
  label,
  value,
  onChange,
  min = 0,
  max = Number.MAX_SAFE_INTEGER,
  step = 1,
  hint,
  suffix,
  disabled,
}: NumberFieldProps) {
  const id = useId();
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          type="number"
          inputMode="numeric"
          className="tabular-nums"
          value={Number.isFinite(value) ? value : ""}
          min={min}
          max={max}
          step={step}
          disabled={disabled}
          onChange={(event) => {
            const parsed = Number(event.target.value);
            if (Number.isFinite(parsed)) {
              onChange(Math.min(max, Math.max(min, parsed)));
            }
          }}
        />
        {suffix ? <span className="shrink-0 text-xs text-muted-foreground">{suffix}</span> : null}
      </div>
      {hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

interface SliderFieldProps {
  label: ReactNode;
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  step?: number;
  format?: (value: number) => string;
  hint?: ReactNode;
  disabled?: boolean;
}

export function SliderField({
  label,
  value,
  onChange,
  min,
  max,
  step = 0.01,
  format,
  hint,
  disabled,
}: SliderFieldProps) {
  const id = useId();
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-3">
        <Label htmlFor={id}>{label}</Label>
        <span className="font-mono text-xs tabular-nums text-foreground">
          {format ? format(value) : value}
        </span>
      </div>
      <input
        id={id}
        type="range"
        className="w-full cursor-pointer accent-primary disabled:cursor-not-allowed disabled:opacity-50"
        value={value}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        onChange={(event) => onChange(Number(event.target.value))}
      />
      {hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
}

export function Switch({ checked, onCheckedChange, label, disabled }: SwitchProps) {
  return (
    <div className="flex items-center gap-3">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={typeof label === "string" ? label : undefined}
        disabled={disabled}
        onClick={() => onCheckedChange(!checked)}
        className={cn(
          "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors disabled:opacity-50",
          checked ? "bg-primary" : "bg-secondary",
        )}
      >
        <span
          className={cn(
            "pointer-events-none inline-block size-4 transform rounded-full bg-white shadow transition-transform",
            checked ? "translate-x-4" : "translate-x-0.5",
          )}
        />
      </button>
      {label ? (
        <span className="text-sm leading-none text-foreground">{label}</span>
      ) : null}
    </div>
  );
}

export interface SelectOption {
  value: string;
  /** Native `<option>` labels must be text; keep it a string on purpose. */
  label: string;
  disabled?: boolean;
}

interface SelectProps {
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  label?: ReactNode;
  hint?: ReactNode;
  id?: string;
  disabled?: boolean;
  className?: string;
}

/**
 * A styled native select. Radix Select is not a dependency here, and the
 * browser's own popup behaves better inside a desktop window.
 */
export function Select({
  value,
  onChange,
  options,
  label,
  hint,
  id,
  disabled,
  className,
}: SelectProps) {
  const generatedId = useId();
  const selectId = id ?? generatedId;
  const select = (
    <div className="relative">
      <select
        id={selectId}
        value={value}
        disabled={disabled}
        className={cn(controlClass, "h-9 appearance-none pr-9", className)}
        onChange={(event) => onChange(event.target.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value} disabled={option.disabled}>
            {option.label}
          </option>
        ))}
      </select>
      <ChevronDown className="pointer-events-none absolute right-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
    </div>
  );

  if (!label && !hint) {
    return select;
  }
  return (
    <FieldRow label={label ?? ""} hint={hint} htmlFor={selectId}>
      {select}
    </FieldRow>
  );
}
