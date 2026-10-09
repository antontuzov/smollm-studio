import { useId, useState } from "react";
import { Dices, Plus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input, Label, Select, SliderField } from "@/components/ui/field";
import { Note } from "@/components/ui/feedback";

import type { SamplingParams, SamplingPreset } from "@/lib/types";

/** The largest seed llama.cpp can be given, so the field never offers a value
 *  the engine would quietly throw away. */
const MAX_SEED = 4_294_967_295;

/** Mirrors `MAX_STOP_SEQUENCES` / `MAX_STOP_LENGTH` in smollm-core, which stays
 *  the authority: Rust answers an error rather than trusting these. */
const MAX_STOPS = 8;
const MAX_STOP_LENGTH = 128;

interface SamplingPanelProps {
  presets: SamplingPreset[];
  preset: string;
  params: SamplingParams;
  onPreset: (name: string, params: SamplingParams) => void;
  onParams: (params: SamplingParams) => void;
  stops: string[];
  onStops: (stops: string[]) => void;
  disabled?: boolean;
}

/** The sampling knobs the spec asks for, with the ranges Rust validates. */
export function SamplingPanel({
  presets,
  preset,
  params,
  onPreset,
  onParams,
  stops,
  onStops,
  disabled = false,
}: SamplingPanelProps) {
  const patch = (changes: Partial<SamplingParams>) => onParams({ ...params, ...changes });
  const active = presets.find((entry) => entry.name === preset);

  return (
    <div className="space-y-3.5">
      {presets.length === 0 ? (
        <Note tone="neutral">
          <p>Presets are still loading.</p>
        </Note>
      ) : (
        <Select
          label="Preset"
          value={preset}
          disabled={disabled}
          options={presets.map((entry) => ({ value: entry.name, label: entry.label }))}
          onChange={(name) => {
            const chosen = presets.find((entry) => entry.name === name);
            if (chosen) {
              onPreset(chosen.name, chosen.params);
            }
          }}
          hint={
            active
              ? presetDescription(active.name)
              : "Custom values; choose a preset to start again from a tuned set."
          }
        />
      )}

      <SliderField
        label="Temperature"
        value={params.temperature}
        min={0}
        max={2}
        step={0.05}
        disabled={disabled}
        onChange={(temperature) => patch({ temperature })}
        hint="Lower is repeatable, higher is surprising."
      />
      <SliderField
        label="Top-p"
        value={params.topP}
        min={0}
        max={1}
        step={0.01}
        disabled={disabled}
        onChange={(topP) => patch({ topP })}
      />
      <SliderField
        label="Top-k"
        value={params.topK}
        min={0}
        max={200}
        step={1}
        disabled={disabled}
        format={(value) => String(Math.round(value))}
        onChange={(topK) => patch({ topK })}
      />
      <SliderField
        label="Min-p"
        value={params.minP}
        min={0}
        max={1}
        step={0.01}
        disabled={disabled}
        onChange={(minP) => patch({ minP })}
      />
      <SliderField
        label="Max tokens"
        value={params.maxTokens}
        min={16}
        max={4096}
        step={16}
        disabled={disabled}
        format={(value) => String(Math.round(value))}
        onChange={(maxTokens) => patch({ maxTokens })}
        hint="Hard cap per answer, so a runaway generation costs you seconds, not minutes."
      />
      <SliderField
        label="Repeat penalty"
        value={params.repeatPenalty}
        min={0.8}
        max={2}
        step={0.01}
        disabled={disabled}
        onChange={(repeatPenalty) => patch({ repeatPenalty })}
      />
      <SliderField
        label="Presence penalty"
        value={params.presencePenalty}
        min={-2}
        max={2}
        step={0.05}
        disabled={disabled}
        onChange={(presencePenalty) => patch({ presencePenalty })}
      />

      <SeedField seed={params.seed} disabled={disabled} onChange={(seed) => patch({ seed })} />
      <StopField stops={stops} disabled={disabled} onChange={onStops} />
    </div>
  );
}

interface SeedFieldProps {
  seed: number | null;
  onChange: (seed: number | null) => void;
  disabled?: boolean;
}

/**
 * An empty field means "no seed" and the engine draws one per request, which is
 * a third state a number input cannot show on its own — hence the roll and the
 * clear beside it.
 */
function SeedField({ seed, onChange, disabled }: SeedFieldProps) {
  const id = useId();
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>Seed</Label>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          type="number"
          inputMode="numeric"
          className="tabular-nums"
          min={0}
          max={MAX_SEED}
          step={1}
          value={seed ?? ""}
          placeholder="random per request"
          disabled={disabled}
          onChange={(event) => {
            const typed = event.target.value.trim();
            if (typed.length === 0) {
              onChange(null);
              return;
            }
            const parsed = Number(typed);
            if (Number.isFinite(parsed)) {
              onChange(Math.min(MAX_SEED, Math.max(0, Math.trunc(parsed))));
            }
          }}
        />
        <Button
          variant="outline"
          size="sm"
          title="Draw a seed at random"
          disabled={disabled}
          onClick={() => onChange(Math.floor(Math.random() * (MAX_SEED + 1)))}
        >
          <Dices />
          Random
        </Button>
        {seed === null ? null : (
          <Button variant="ghost" size="sm" onClick={() => onChange(null)} disabled={disabled}>
            <X />
            Clear
          </Button>
        )}
      </div>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {seed === null
          ? "Nothing is pinned, so the same question can answer differently each time."
          : `Pinned to ${seed}: while the other values hold, this seed reproduces the answer.`}
      </p>
    </div>
  );
}

interface StopFieldProps {
  stops: string[];
  onChange: (stops: string[]) => void;
  disabled?: boolean;
}

/**
 * A single-line input cannot type a newline, so `\n` and `\t` written as escapes
 * are turned into the real control characters — a template's end marker is
 * usually exactly one of those.
 */
function StopField({ stops, onChange, disabled }: StopFieldProps) {
  const id = useId();
  const [typed, setTyped] = useState("");
  const full = stops.length >= MAX_STOPS;

  const add = () => {
    const marker = unescapeMarker(typed);
    if (marker.length === 0 || stops.includes(marker) || full) {
      return;
    }
    onChange([...stops, marker]);
    setTyped("");
  };

  return (
    <div className="space-y-2">
      <Label htmlFor={id}>Stop sequences</Label>
      {stops.length === 0 ? null : (
        <ul className="flex flex-wrap gap-1.5">
          {stops.map((stop, index) => (
            <li
              key={`${stop}-${index}`}
              className="flex max-w-full items-center gap-1 rounded-md border bg-secondary/50 py-0.5 pl-2 pr-1"
            >
              <code className="truncate text-[11px]">{showMarker(stop)}</code>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove ${showMarker(stop)}`}
                disabled={disabled}
                onClick={() => onChange(stops.filter((_, position) => position !== index))}
              >
                <X />
              </Button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-center gap-2">
        <Input
          id={id}
          value={typed}
          maxLength={MAX_STOP_LENGTH}
          placeholder="<|end|>"
          className="font-mono text-xs"
          disabled={disabled || full}
          onChange={(event) => setTyped(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              add();
            }
          }}
        />
        <Button variant="outline" size="sm" onClick={add} disabled={disabled || full || typed.trim().length === 0}>
          <Plus />
          Add
        </Button>
      </div>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {`Up to ${MAX_STOPS} markers. An answer ends before the first marker it contains, and the
        marker is never shown. Write \\n for a newline, which is how most chat templates end.`}
      </p>
    </div>
  );
}

function unescapeMarker(typed: string): string {
  return typed
    .trim()
    .replace(/\\n/g, "\n")
    .replace(/\\t/g, "\t");
}

/** Control characters have to be visible to be removable. */
function showMarker(stop: string): string {
  return stop.replace(/\n/g, "\\n").replace(/\t/g, "\\t");
}

function presetDescription(name: string): string {
  switch (name) {
    case "fast":
      return "Low temperature, short answers. Best for lookups and summarising.";
    case "balanced":
      return "The default. Reasonable variety without rambling.";
    case "creative":
      return "Warm sampling for stories, naming and brainstorming.";
    case "coding":
      return "Cool and consistent, with room for a long file.";
    case "precise":
      return "Near-greedy for extractions, formatting and maths.";
    default:
      return "Tuned sampling values.";
  }
}
