import { useMemo } from "react";
import {
  Boxes,
  Gauge,
  HardDrive,
  House,
  MessageSquare,
  ScrollText,
  Server,
  Settings2,
} from "lucide-react";

import logoUrl from "@/assets/logo.png";
import { formatBytes, formatPercent } from "@/lib/format";
import { useAppInfo } from "@/lib/queries";
import { selectActiveTasks, useDownloads } from "@/stores/engine";
import { PAGES, useUi } from "@/stores/ui";
import { Progress } from "@/components/ui/feedback";
import { cn } from "@/lib/utils";

import type { ComponentType } from "react";
import type { PageId } from "@/stores/ui";

const pageIcons: Record<PageId, ComponentType<{ className?: string }>> = {
  home: House,
  models: Boxes,
  library: HardDrive,
  chat: MessageSquare,
  server: Server,
  benchmarks: Gauge,
  logs: ScrollText,
  settings: Settings2,
};

export function Sidebar() {
  const page = useUi((state) => state.page);
  const setPage = useUi((state) => state.setPage);
  const appInfo = useAppInfo();
  const taskMap = useDownloads((state) => state.tasks);
  const live = useDownloads((state) => state.live);
  const active = useMemo(() => selectActiveTasks(taskMap), [taskMap]);
  const current = active[0];
  const progress = current ? live[current.id] : undefined;

  return (
    <nav className="flex w-56 shrink-0 flex-col border-r bg-card/40">
      <div className="flex items-center gap-3 px-4 py-4">
        <img
          src={logoUrl}
          alt=""
          width={36}
          height={36}
          className="size-9 shrink-0 rounded-lg shadow-soft"
        />
        <div className="min-w-0">
          <p className="truncate text-sm font-semibold leading-tight">
            {appInfo.data?.name ?? "SmolLLM Studio"}
          </p>
          <p className="truncate text-[11px] text-muted-foreground">
            v{appInfo.data?.version ?? "0.1.0"} · {appInfo.data?.tagline ?? "small models, local"}
          </p>
        </div>
      </div>

      <div className="flex-1 space-y-1 overflow-y-auto px-2 pb-2">
        {PAGES.map((entry) => {
          const Icon = pageIcons[entry.id];
          const selected = entry.id === page;
          return (
            <button
              key={entry.id}
              type="button"
              onClick={() => setPage(entry.id)}
              aria-current={selected ? "page" : undefined}
              className={cn(
                "flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors",
                selected
                  ? "bg-primary/15 font-medium text-primary"
                  : "text-muted-foreground hover:bg-secondary/60 hover:text-foreground",
              )}
            >
              <Icon className={cn("size-4 shrink-0", selected && "text-primary")} />
              <span className="flex-1 truncate text-left">{entry.label}</span>
              <span className="font-mono text-[10px] text-muted-foreground/70">
                {entry.shortcut}
              </span>
            </button>
          );
        })}
      </div>

      {current ? (
        <div className="mx-2 mb-2 space-y-2 rounded-lg border bg-background/60 px-3 py-2.5">
          <div className="flex items-center justify-between gap-2 text-[11px]">
            <span className="truncate font-medium">
              {active.length === 1 ? current.displayName : `${active.length} downloads`}
            </span>
            <span className="tabular-nums text-muted-foreground">
              {formatPercent(current.percent)}
            </span>
          </div>
          <Progress value={current.percent} tone="accent" />
          <p className="truncate text-[11px] text-muted-foreground">
            {progress
              ? `${formatBytes(progress.downloadedBytes)} · ${formatBytes(progress.bytesPerSecond)}/s`
              : `${current.fileName} · ${current.state}`}
          </p>
        </div>
      ) : (
        <p className="px-4 pb-4 text-[11px] leading-relaxed text-muted-foreground">
          Everything stays on this machine. No accounts, no telemetry.
        </p>
      )}
    </nav>
  );
}
