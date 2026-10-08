import {
  Boxes,
  Cpu,
  Gauge,
  HardDrive,
  MemoryStick,
  MessageSquare,
  Play,
  Server,
  Zap,
} from "lucide-react";

import { loadModel, pullModel } from "@/lib/actions";
import { formatGigabytes, formatMegabytes, formatRate } from "@/lib/format";
import { pickEntries, useCatalog, useDoctor, useHardware } from "@/lib/queries";
import { backendLabel, backendTone } from "@/lib/presentation";
import { selectActiveTasks, useDownloads, useEngine } from "@/stores/engine";
import { useChat } from "@/stores/chat";
import { useUi } from "@/stores/ui";
import { DownloadList } from "@/components/download-list";
import { PageHeader } from "@/components/page-header";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, Stat } from "@/components/ui/card";
import { Busy, ErrorState, Note, Skeleton, SkeletonList } from "@/components/ui/feedback";

import type { ComponentType } from "react";

const suggestions = [
  "Explain what a GGUF quantisation level means in three sentences.",
  "Write a haiku about running models on a laptop.",
  "Summarise the tradeoff between speed and quality in small models.",
];

export function HomePage() {
  const setPage = useUi((state) => state.setPage);
  const setDraft = useChat((state) => state.setDraft);
  const hardware = useHardware();
  const doctor = useDoctor();
  const metrics = useEngine((state) => state.metrics);
  const handle = useEngine((state) => state.handle);
  const busy = useEngine((state) => state.busy);
  const tasks = useDownloads((state) => state.tasks);
  const activeCount = selectActiveTasks(tasks).length;

  const recommendations = useCatalog({
    query: "",
    sort: "recommended",
    hidePlaceholders: true,
  });

  return (
    <>
      <PageHeader
        title="Home"
        description="Your machine, the engine, and the three steps that matter: pick a model, load it, talk to it."
        icon={Boxes}
        actions={
          <>
            <Button variant="outline" onClick={() => setPage("models")}>
              <Boxes />
              Browse models
            </Button>
            <Button onClick={() => setPage("chat")}>
              <MessageSquare />
              Open chat
            </Button>
          </>
        }
      />

      <div className="space-y-5 p-6">
        {metrics?.simulated ? (
          <Note tone="warning" icon={Gauge}>
            <p className="font-medium">
              The engine is reporting from <span className="font-mono">{metrics.engine}</span>, which
              generates deterministic text instead of running weights.
            </p>
            <p>
              Real GGUF inference comes from the <span className="font-mono">llama-cpp</span> engine,
              which this binary does not contain: it is built with{" "}
              <span className="font-mono">--features llama-cpp</span> and needs cmake plus a C/C++
              toolchain. Everything else — downloads, the library, the local server — behaves the
              same, so this is the right place to learn the interface.
            </p>
          </Note>
        ) : null}

        <div className="grid gap-5 xl:grid-cols-2">
          <Card>
            <CardHeader
              title="This machine"
              description="Measured once at launch with sysinfo."
              actions={
                <Button variant="ghost" size="sm" onClick={() => void hardware.refetch()}>
                  Re-measure
                </Button>
              }
            />
            <CardContent>
              {hardware.isPending ? (
                <div className="space-y-3">
                  <Skeleton className="h-4 w-1/2" />
                  <SkeletonList rows={2} />
                </div>
              ) : hardware.isError ? (
                <ErrorState
                  message="Hardware detection failed"
                  detail={hardware.error?.message}
                  onRetry={() => void hardware.refetch()}
                />
              ) : (
                <div className="space-y-4">
                  <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
                    <Stat label="CPU" value={hardware.data?.logicalCores ?? 0} hint="logical cores" />
                    <Stat
                      label="Memory"
                      value={`${Math.round(hardware.data?.totalRamGb ?? 0)} GB`}
                      hint={`${formatGigabytes(hardware.data?.availableRamGb ?? 0)} free`}
                    />
                    <Stat
                      label="Disk"
                      value={`${Math.round(hardware.data?.diskFreeGb ?? 0)} GB`}
                      hint="free for models"
                    />
                  </div>
                  <dl className="space-y-2 text-xs">
                    <Line icon={Cpu} label="Processor" value={hardware.data?.cpuBrand || hardware.data?.arch || "unknown"} />
                    <Line
                      icon={MemoryStick}
                      label="Graphics"
                      value={hardware.data?.gpuName || "integrated / none detected"}
                    />
                    <Line
                      icon={Zap}
                      label="Offload"
                      value={
                        hardware.data?.accelerator
                          ? `${hardware.data.accelerator.description} on ${hardware.data.accelerator.backend} · ${formatGigabytes(hardware.data.accelerator.usableMemoryGb)} budget`
                          : "no engine in this build names a device"
                      }
                    />
                    <Line
                      icon={HardDrive}
                      label="Model volume"
                      value={`${formatGigabytes(hardware.data?.modelVolumeFreeGb ?? 0)} free`}
                    />
                  </dl>
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge tone={hardware.data?.metalAvailable ? "success" : "neutral"}>
                      {hardware.data?.metalAvailable ? "Metal available" : "no Metal"}
                    </Badge>
                    <Badge tone={hardware.data?.appleSilicon ? "success" : "neutral"}>
                      {hardware.data?.appleSilicon ? "Apple Silicon" : hardware.data?.platform ?? "unknown"}
                    </Badge>
                    <Badge tone={hardware.data?.vulkanAvailable ? "info" : "neutral"}>
                      {hardware.data?.vulkanAvailable ? "Vulkan" : "no Vulkan"}
                    </Badge>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader
              title="Quick start"
              description={doctor.data?.headline ?? "A recommendation for this machine."}
            />
            <CardContent className="space-y-4">
              {doctor.isPending ? (
                <SkeletonList rows={3} />
              ) : doctor.isError ? (
                <ErrorState
                  message="The doctor report failed"
                  detail={doctor.error?.message}
                  onRetry={() => void doctor.refetch()}
                />
              ) : (
                <>
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span className="text-muted-foreground">Recommended backend</span>
                    <Badge tone={backendTone(doctor.data?.backendRecommendation ?? "mock")}>
                      {backendLabel(doctor.data?.backendRecommendation ?? "mock")}
                    </Badge>
                    <StatusPill
                      tone={metrics ? "success" : "neutral"}
                      label={metrics ? `${metrics.requests} requests this session` : "no requests yet"}
                    />
                  </div>

                  {doctor.data?.warnings.length ? (
                    <Note tone="warning">
                      <ul className="list-disc space-y-1 pl-4">
                        {doctor.data.warnings.map((warning) => (
                          <li key={warning}>{warning}</li>
                        ))}
                      </ul>
                    </Note>
                  ) : null}

                  <div className="space-y-2">
                    <p className="text-xs font-medium text-muted-foreground">
                      Models that fit this machine
                    </p>
                    {recommendations.isPending ? (
                      <Busy label="Reading the catalog" />
                    ) : recommendations.isError ? (
                      <ErrorState
                        message="The catalog is unavailable"
                        detail={recommendations.error?.message}
                        onRetry={() => void recommendations.refetch()}
                      />
                    ) : (
                      <ul className="space-y-2">
                        {pickEntries(recommendations.data ?? [], doctor.data?.recommendedModels ?? []).map(
                          (model) => (
                            <li
                              key={model.id}
                              className="flex items-center justify-between gap-3 rounded-lg border bg-background/50 px-3 py-2"
                            >
                              <div className="min-w-0">
                                <p className="truncate text-xs font-medium">{model.displayName}</p>
                                <p className="truncate text-[11px] text-muted-foreground">
                                  {model.parametersB}B · {model.quantization} ·{" "}
                                  {formatMegabytes(model.sizeMb)} ·{" "}
                                  ~{formatGigabytes(model.estimatedRamGb)} RAM
                                </p>
                              </div>
                              {model.downloaded ? (
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  disabled={busy}
                                  onClick={() => void loadModel(model.id)}
                                >
                                  <Play />
                                  Load
                                </Button>
                              ) : (
                                <Button
                                  size="sm"
                                  variant="outline"
                                  disabled={model.downloading}
                                  onClick={() => void pullModel(model.id)}
                                >
                                  {model.downloading ? `${model.downloadPercent.toFixed(0)}%` : "Download"}
                                </Button>
                              )}
                            </li>
                          ),
                        )}
                      </ul>
                    )}
                  </div>
                </>
              )}
            </CardContent>
            <CardFooter>
              <span className="text-[11px] text-muted-foreground">
                Nothing leaves this machine. Model files come from Hugging Face only when you press
                Download.
              </span>
              <Button variant="ghost" size="sm" onClick={() => setPage("models")}>
                See all models
              </Button>
            </CardFooter>
          </Card>

          <Card>
            <CardHeader
              title="Engine"
              description={handle ? handle.path : "No model is resident yet."}
              actions={
                <Button variant="ghost" size="sm" onClick={() => setPage("benchmarks")}>
                  <Gauge />
                  Benchmarks
                </Button>
              }
            />
            <CardContent>
              <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                <Stat label="Engine" value={metrics?.engine ?? "—"} hint={metrics ? `backend: ${metrics.backend}` : undefined} />
                <Stat label="Requests" value={metrics?.requests ?? 0} />
                <Stat label="Tokens" value={metrics?.tokensGenerated ?? 0} />
                <Stat
                  label="Avg speed"
                  value={metrics && metrics.tokensPerSecond > 0 ? `${formatRate(metrics.tokensPerSecond)} tok/s` : "—"}
                  tone="accent"
                />
              </div>
            </CardContent>
            <CardFooter>
              <Button variant="outline" size="sm" onClick={() => setPage("settings")}>
                Defaults
              </Button>
              <span className="flex items-center gap-2 text-[11px] text-muted-foreground">
                <Server className="size-3.5" />
                {activeCount > 0 ? `${activeCount} transfer(s) running` : "no transfers running"}
              </span>
            </CardFooter>
          </Card>

          <Card>
            <CardHeader title="Transfers" description="Live download state for this session." />
            <CardContent>
              <DownloadList />
            </CardContent>
            <CardFooter>
              <span className="text-[11px] text-muted-foreground">
                Interrupted transfers resume from the .part file.
              </span>
              <Button variant="ghost" size="sm" onClick={() => setPage("library")}>
                Library
              </Button>
            </CardFooter>
          </Card>

          <Card className="xl:col-span-2">
            <CardHeader
              title="Try a prompt"
              description="These work with the mock engine too, so you can see the streaming path immediately."
            />
            <CardContent>
              <ul className="grid gap-2 sm:grid-cols-3">
                {suggestions.map((prompt) => (
                  <li key={prompt}>
                    <button
                      type="button"
                      onClick={() => {
                        setDraft(prompt);
                        setPage("chat");
                      }}
                      className="h-full w-full rounded-lg border bg-background/50 px-3 py-2.5 text-left text-xs leading-relaxed text-muted-foreground transition-colors hover:border-primary/40 hover:text-foreground"
                    >
                      {prompt}
                    </button>
                  </li>
                ))}
              </ul>
            </CardContent>
          </Card>
        </div>
      </div>
    </>
  );
}

function Line({
  icon: Icon,
  label,
  value,
}: {
  icon: ComponentType<{ className?: string }>;
  label: string;
  value: string | undefined;
}) {
  return (
    <div className="flex items-center gap-2">
      <Icon className="size-3.5 shrink-0 text-muted-foreground" />
      <dt className="w-24 shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 flex-1 truncate">{value ?? "—"}</dd>
    </div>
  );
}
