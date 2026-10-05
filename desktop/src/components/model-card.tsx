import { Ban, Download, MessageSquare, Play } from "lucide-react";

import { formatMegabytes } from "@/lib/format";
import { capitalize, downloadStateLabel, ratingTone } from "@/lib/presentation";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Note, Progress } from "@/components/ui/feedback";
import { cn } from "@/lib/utils";

import type { CatalogEntry } from "@/lib/types";

interface ModelCardProps {
  model: CatalogEntry;
  onPull: (id: string) => void;
  onLoad: (id: string) => void;
  onChat: (id: string) => void;
  onCancel: (id: string) => void;
  busy?: boolean;
  className?: string;
}

export function ModelCard({
  model,
  onPull,
  onLoad,
  onChat,
  onCancel,
  busy = false,
  className,
}: ModelCardProps) {
  return (
    <Card
      className={cn(
        // Lift on hover and on keyboard focus, so the card reads as an object
        // you can act on either way.
        "group gap-0 p-4 transition-[border-color,box-shadow,transform] duration-200 ease-out",
        "hover:-translate-y-0.5 hover:border-primary/40 hover:shadow-panel",
        "focus-within:border-primary/40 focus-within:shadow-panel motion-reduce:hover:translate-y-0",
        className,
      )}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <h3 className="truncate text-sm font-semibold leading-snug tracking-tight">
            {model.displayName}
          </h3>
          <p className="truncate text-xs text-muted-foreground">
            {capitalize(model.family)} · {model.quantization} · {model.parametersB}B params
          </p>
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1.5">
          {model.status === "placeholder" ? (
            <Badge tone="warning" className="font-mono">
              unverified id
            </Badge>
          ) : null}
          <Badge tone={ratingTone(model.quality)}>{model.quality} quality</Badge>
        </div>
      </div>

      <p className="mt-3 line-clamp-3 text-xs leading-relaxed text-muted-foreground">
        {model.description}
      </p>

      <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs">
        <Fact label="Download" value={formatMegabytes(model.sizeMb)} />
        <Fact label="Context" value={`${model.contextLength} tokens`} />
        <Fact label="Needs RAM" value={`~${model.recommendedRamGb} GB`} />
        <Fact label="Speed" value={`${model.speed} · ${model.tags.slice(0, 2).join(", ")}`} />
      </dl>

      {!model.fitsMemory ? (
        <Note tone="warning" className="mt-3">
          This model is estimated to need about {model.recommendedRamGb} GB of RAM. Loading it may
          fail or push this machine into swap.
        </Note>
      ) : null}

      {model.downloading ? (
        <div className="mt-3 space-y-2 rounded-lg border bg-background/60 px-3 py-2.5">
          <div className="flex items-center justify-between gap-2 text-[11px]">
            <StatusPill tone="info" label={downloadStateLabel("running")} pulse />
            <span className="tabular-nums text-muted-foreground">{model.downloadPercent.toFixed(0)}%</span>
          </div>
          <Progress value={model.downloadPercent} tone="accent" />
          <Button variant="outline" size="sm" onClick={() => onCancel(model.id)}>
            <Ban />
            Cancel download
          </Button>
        </div>
      ) : (
        <div className="mt-4 flex flex-wrap items-center gap-2">
          {model.downloaded ? (
            <>
              <Button size="sm" onClick={() => onLoad(model.id)} disabled={busy}>
                <Play />
                Load
              </Button>
              <Button variant="outline" size="sm" onClick={() => onChat(model.id)}>
                <MessageSquare />
                Chat
              </Button>
              <Badge tone="success" className="ml-auto">
                in library
              </Badge>
            </>
          ) : (
            <Button size="sm" variant="secondary" onClick={() => onPull(model.id)} disabled={busy}>
              <Download />
              Download {formatMegabytes(model.sizeMb)}
            </Button>
          )}
        </div>
      )}
    </Card>
  );
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-2 border-b border-dashed py-1">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate text-right font-medium tabular-nums">{value}</dd>
    </div>
  );
}
