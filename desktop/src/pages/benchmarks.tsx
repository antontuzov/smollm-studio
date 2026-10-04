import { useEffect, useState } from "react";
import { Activity, Gauge, Play, X } from "lucide-react";

import { api } from "@/lib/api";
import { toast } from "@/stores/ui";
import { describeError, formatDuration, formatRate } from "@/lib/format";
import { useCatalog, useHardware, useSettingsQuery } from "@/lib/queries";
import { useBenchmark } from "@/stores/benchmark";
import { useEngine } from "@/stores/engine";
import { PageHeader } from "@/components/page-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, Stat } from "@/components/ui/card";
import { CopyButton } from "@/components/ui/copy";
import { NumberField, Select } from "@/components/ui/field";
import { EmptyState, ErrorState, Note, Progress, SkeletonList } from "@/components/ui/feedback";

import type { Backend, BenchmarkConfig, BenchmarkResult } from "@/lib/types";

export function BenchmarksPage() {
  const catalog = useCatalog({ query: "", sort: "smallest", hidePlaceholders: true });
  const hardware = useHardware();
  const settings = useSettingsQuery();
  const handle = useEngine((state) => state.handle);
  const progress = useBenchmark((state) => state.progress);
  const running = useBenchmark((state) => state.running);
  const setRunning = useBenchmark((state) => state.setRunning);
  const setProgress = useBenchmark((state) => state.setProgress);

  const [config, setConfig] = useState<BenchmarkConfig>({
    modelId: "",
    promptTokens: 128,
    maxTokens: 128,
    contextLength: 4096,
    gpuLayers: -1,
    backend: "cpu",
    runs: 3,
  });
  const [result, setResult] = useState<BenchmarkResult | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [history, setHistory] = useState<BenchmarkResult[]>([]);

  const downloaded = (catalog.data ?? []).filter((model) => model.downloaded);

  useEffect(() => {
    if (config.modelId || downloaded.length === 0) {
      return;
    }
    setConfig((current) => ({ ...current, modelId: handle?.modelId ?? downloaded[0].id }));
  }, [config.modelId, downloaded, handle]);

  useEffect(() => {
    if (!settings.data || !hardware.data) {
      return;
    }
    const backend: Backend = hardware.data.metalAvailable
      ? "metal"
      : hardware.data.vulkanAvailable
        ? "vulkan"
        : settings.data.defaultBackend;
    setConfig((current) => ({
      ...current,
      contextLength: settings.data?.defaultContextLength ?? current.contextLength,
      gpuLayers: settings.data?.defaultGpuLayers ?? current.gpuLayers,
      backend: current.backend === "cpu" ? backend : current.backend,
    }));
  }, [settings.data, hardware.data]);

  const backendOptions: { value: Backend; label: string }[] = [
    { value: "cpu", label: "CPU" },
    { value: "mock", label: "Mock engine (simulated numbers)" },
  ];
  if (hardware.data?.metalAvailable) {
    backendOptions.splice(1, 0, { value: "metal", label: "Apple Metal / GPU" });
  }
  if (hardware.data?.nvidiaGpu) {
    backendOptions.splice(1, 0, { value: "cuda", label: `CUDA · ${hardware.data.nvidiaGpu}` });
  }
  if (hardware.data?.vulkanAvailable) {
    backendOptions.push({ value: "vulkan", label: "Vulkan" });
  }

  const run = async () => {
    if (config.modelId.length === 0) {
      toast({ title: "Pick a model first", variant: "warning" });
      return;
    }
    setRunning(true);
    setFailure(null);
    setProgress(null);
    try {
      const outcome = await api.runBenchmark(config);
      setResult(outcome);
      setHistory((current) => [outcome, ...current].slice(0, 8));
    } catch (error) {
      setFailure(describeError(error));
    } finally {
      setRunning(false);
      setProgress(null);
    }
  };

  return (
    <>
      <PageHeader
        title="Benchmarks"
        description="One cold load, one prompt, one generation: load time, prompt and generation throughput, time to first token and peak resident memory. The benchmark uses its own engine instance so it never competes with the chat."
        icon={Gauge}
        actions={
          <Button onClick={() => void run()} disabled={running}>
            <Play />
            {running ? "Running…" : "Run benchmark"}
          </Button>
        }
      />

      <div className="grid gap-5 p-6 xl:grid-cols-[360px_minmax(0,1fr)]">
        <Card>
          <CardHeader title="Setup" description="Anything you change applies to the next run." />
          <CardContent className="space-y-4">
            {catalog.isPending ? (
              <SkeletonList rows={3} />
            ) : downloaded.length === 0 ? (
              <EmptyState
                icon={Gauge}
                title="Nothing to measure yet"
                description="Download a model first: a benchmark of the mock engine tells you about the harness, not about your hardware."
              />
            ) : (
              <Select
                label="Model"
                value={config.modelId}
                disabled={running}
                options={downloaded.map((model) => ({ value: model.id, label: model.displayName }))}
                onChange={(modelId) => setConfig({ ...config, modelId })}
              />
            )}

            <NumberField
              label="Prompt length"
              value={config.promptTokens}
              min={8}
              max={8192}
              step={8}
              suffix="tokens"
              disabled={running}
              onChange={(promptTokens) => setConfig({ ...config, promptTokens })}
              hint="Filled with generated filler text, so prefill has something real to read."
            />
            <NumberField
              label="Generation length"
              value={config.maxTokens}
              min={8}
              max={2048}
              step={8}
              suffix="tokens"
              disabled={running}
              onChange={(maxTokens) => setConfig({ ...config, maxTokens })}
            />
            <NumberField
              label="Context window"
              value={config.contextLength}
              min={512}
              max={32_768}
              step={512}
              suffix="tokens"
              disabled={running}
              onChange={(contextLength) => setConfig({ ...config, contextLength })}
            />
            <NumberField
              label="GPU layers"
              value={config.gpuLayers}
              min={-1}
              max={99}
              disabled={running}
              onChange={(gpuLayers) => setConfig({ ...config, gpuLayers })}
              hint="-1 offloads every layer the backend can."
            />
            <Select
              label="Backend"
              value={config.backend}
              disabled={running}
              options={backendOptions.map((option) => ({ value: option.value, label: option.label }))}
              onChange={(backend) => setConfig({ ...config, backend: backend as Backend })}
            />
            <NumberField
              label="Repetitions"
              value={config.runs}
              min={1}
              max={10}
              suffix="runs"
              disabled={running}
              onChange={(runs) => setConfig({ ...config, runs })}
              hint="Reported values are the median across runs."
            />
          </CardContent>
          <CardFooter>
            <span className="text-[11px] text-muted-foreground">
              Prompt and generation speed are medians, not a best-of.
            </span>
          </CardFooter>
        </Card>

        <div className="space-y-5">
          {running ? (
            <Card>
              <CardHeader
                title="Running"
                description={progress?.message ?? "Preparing the engine…"}
                actions={<Badge tone="info">{progress?.stage ?? "starting"}</Badge>}
              />
              <CardContent className="space-y-2">
                <Progress value={progress?.percent} tone="accent" />
                <p className="text-[11px] text-muted-foreground">
                  A cold load is part of the measurement, so the first stage can take a few seconds.
                </p>
              </CardContent>
            </Card>
          ) : null}

          {failure ? (
            <ErrorState message="The benchmark did not finish" detail={failure} />
          ) : null}

          {result ? (
            <Card>
              <CardHeader
                title="Results"
                description={`${result.modelId} · ${result.engine} · ${result.backend}`}
                actions={
                  <div className="flex items-center gap-1">
                    {result.simulated ? <Badge tone="warning">simulated</Badge> : null}
                    <CopyButton text={toMarkdown(result)} label="Copy as Markdown" />
                  </div>
                }
              />
              <CardContent className="space-y-4">
                {result.simulated ? (
                  <Note tone="warning" icon={Activity}>
                    <p>
                      These numbers come from the mock engine. They are useful for checking the
                      harness and the UI, and meaningless as a statement about this machine.
                    </p>
                  </Note>
                ) : null}

                <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
                  <Stat label="Load time" value={formatDuration(result.loadMs)} />
                  <Stat
                    label="Prompt speed"
                    value={`${formatRate(result.promptTokensPerSecond)} tok/s`}
                    tone="accent"
                  />
                  <Stat
                    label="Generation"
                    value={`${formatRate(result.generationTokensPerSecond)} tok/s`}
                    tone="accent"
                  />
                  <Stat
                    label="First token"
                    value={formatDuration(result.timeToFirstTokenMs)}
                    tone={result.timeToFirstTokenMs > 2000 ? "warning" : "default"}
                  />
                </div>

                <table className="w-full text-sm">
                  <caption className="sr-only">Benchmark metrics</caption>
                  <thead>
                    <tr className="border-b text-left text-xs text-muted-foreground">
                      <th className="py-2 font-medium">Metric</th>
                      <th className="py-2 text-right font-medium">Value</th>
                    </tr>
                  </thead>
                  <tbody className="font-mono text-xs">
                    {rows(result).map(([label, value]) => (
                      <tr key={label} className="border-b last:border-0">
                        <td className="py-1.5 font-sans text-muted-foreground">{label}</td>
                        <td className="py-1.5 text-right tabular-nums">{value}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>

                {result.warnings.length > 0 ? (
                  <Note tone="warning">
                    <ul className="list-disc space-y-1 pl-4">
                      {result.warnings.map((warning) => (
                        <li key={warning}>{warning}</li>
                      ))}
                    </ul>
                  </Note>
                ) : null}
              </CardContent>
            </Card>
          ) : running ? null : (
            <EmptyState
              icon={Gauge}
              title="No run yet"
              description="Choose a model and press Run benchmark. Results stay in this window only; nothing is uploaded."
            />
          )}

          {history.length > 1 ? (
            <Card>
              <CardHeader
                title="This session"
                description={`${history.length} runs, newest first.`}
              />
              <CardContent>
                <ul className="space-y-1.5">
                  {history.map((entry, index) => (
                    <li
                      key={`${entry.modelId}-${index}`}
                      className="flex items-center justify-between gap-3 rounded-md border bg-background/50 px-3 py-2 text-xs"
                    >
                      <span className="min-w-0 truncate">{entry.modelId}</span>
                      <span className="flex shrink-0 items-center gap-2 tabular-nums text-muted-foreground">
                        <Badge tone={entry.simulated ? "warning" : "success"}>
                          {formatRate(entry.generationTokensPerSecond)} tok/s
                        </Badge>
                        <span>{formatDuration(entry.loadMs)} load</span>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="Show this run"
                          onClick={() => setResult(entry)}
                        >
                          <Activity />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="Forget this run"
                          onClick={() => setHistory((current) => current.filter((_, i) => i !== index))}
                        >
                          <X />
                        </Button>
                      </span>
                    </li>
                  ))}
                </ul>
              </CardContent>
            </Card>
          ) : null}
        </div>
      </div>
    </>
  );
}

function rows(result: BenchmarkResult): [string, string][] {
  return [
    ["Model", result.modelId],
    ["Engine", result.engine],
    ["Backend", result.backend],
    ["Load time", formatDuration(result.loadMs)],
    ["Prompt processing", `${formatRate(result.promptTokensPerSecond)} tok/s`],
    ["Generation", `${formatRate(result.generationTokensPerSecond)} tok/s`],
    ["Time to first token", formatDuration(result.timeToFirstTokenMs)],
    [
      "Peak RSS",
      result.peakRssMb === null ? "not measurable" : `${result.peakRssMb.toFixed(0)} MB`,
    ],
    ["Prompt tokens", String(result.promptTokens)],
    ["Generated tokens", String(result.generatedTokens)],
    ["Repetitions", String(result.runs)],
    ["Context length", `${result.contextLength} tokens`],
  ];
}

/** The same two-column table the Rust `BenchmarkResult::markdown_table` emits. */
function toMarkdown(result: BenchmarkResult): string {
  const lines = ["| Metric | Value |", "| --- | --- |", ...rows(result).map(([label, value]) => `| ${label} | ${value} |`)].join("\n");
  return result.simulated ? `${lines}\n\n_Simulated by the mock engine._` : lines;
}
