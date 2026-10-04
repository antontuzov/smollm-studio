import { Select, SliderField } from "@/components/ui/field";
import { Note } from "@/components/ui/feedback";

import type { SamplingParams, SamplingPreset } from "@/lib/types";

interface SamplingPanelProps {
  presets: SamplingPreset[];
  preset: string;
  params: SamplingParams;
  onPreset: (name: string, params: SamplingParams) => void;
  onParams: (params: SamplingParams) => void;
  disabled?: boolean;
}

/** The five knobs the spec asks for, with the ranges Rust validates. */
export function SamplingPanel({
  presets,
  preset,
  params,
  onPreset,
  onParams,
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
    </div>
  );
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
