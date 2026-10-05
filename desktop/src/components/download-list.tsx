import { useMemo } from "react";
import { Ban, RotateCw } from "lucide-react";

import { cancelDownload, retryDownload } from "@/lib/actions";
import { formatBytes, formatDuration, formatPercent } from "@/lib/format";
import { downloadStateLabel, downloadStateTone } from "@/lib/presentation";
import { isMoving, isRetryable, selectActiveTasks, useDownloads } from "@/stores/engine";
import { StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState, Progress } from "@/components/ui/feedback";
import { cn } from "@/lib/utils";

import type { DownloadProgress, DownloadTask } from "@/lib/types";

const MAX_FINISHED = 6;

/** Transfers, live first. Used on the Library and Home pages. */
export function DownloadList({ className }: { className?: string }) {
  const tasks = useDownloads((state) => state.tasks);
  const live = useDownloads((state) => state.live);

  const rows = useMemo(() => {
    const active = selectActiveTasks(tasks);
    const finished = Object.values(tasks)
      .filter((task) => !isMoving(task.state))
      .sort((left, right) => (right.finishedMs ?? 0) - (left.finishedMs ?? 0))
      .slice(0, MAX_FINISHED);
    return [...active, ...finished];
  }, [tasks]);

  if (rows.length === 0) {
    return (
      <EmptyState
        title="No transfers in this session"
        description="Downloads you start on the Models page appear here with live speed and progress."
        className={className}
      />
    );
  }

  return (
    <div className={cn("space-y-2", className)}>
      {rows.map((task) => (
        <TransferRow key={task.id} task={task} progress={live[task.id]} />
      ))}
    </div>
  );
}

function TransferRow({ task, progress }: { task: DownloadTask; progress?: DownloadProgress }) {
  const percent = progress?.percent ?? task.percent;
  const downloaded = progress?.downloadedBytes ?? task.downloadedBytes;
  const rate = progress?.bytesPerSecond ?? task.bytesPerSecond;
  const total = task.totalBytes ?? progress?.totalBytes ?? null;
  const moving = isMoving(task.state);
  const elapsed =
    task.finishedMs !== null && task.finishedMs > task.startedMs
      ? task.finishedMs - task.startedMs
      : null;

  return (
    <div className="space-y-2.5 rounded-lg border bg-card/60 px-4 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">{task.displayName}</p>
          <p className="truncate font-mono text-[11px] text-muted-foreground">{task.fileName}</p>
        </div>
        <StatusPill
          tone={downloadStateTone(task.state)}
          label={downloadStateLabel(task.state)}
          pulse={moving}
        />
      </div>

      <Progress value={percent} tone={moving ? "accent" : "primary"} />

      <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
        <span className="tabular-nums">
          {formatBytes(downloaded)}
          {total ? ` / ${formatBytes(total)}` : ""} · {formatPercent(percent)}
          {rate > 0 ? ` · ${formatBytes(rate)}/s` : ""}
          {task.resumed ? " · resumed" : ""}
          {elapsed !== null ? ` · ${formatDuration(elapsed)}` : ""}
        </span>
        <span className="flex items-center gap-2">
          {isRetryable(task.state) ? (
            <Button variant="outline" size="sm" onClick={() => void retryDownload(task.id)}>
              <RotateCw />
              Retry
            </Button>
          ) : null}
          {moving ? (
            <Button variant="ghost" size="sm" onClick={() => void cancelDownload(task.id)}>
              <Ban />
              Cancel
            </Button>
          ) : null}
        </span>
      </div>

      {task.error ? (
        // An automatic retry is not a failure, so it must not read like one.
        <p
          className={cn(
            "rounded-md border px-2.5 py-1.5 text-[11px] leading-relaxed",
            task.state === "retrying"
              ? "border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300"
              : "border-destructive/30 bg-destructive/10 text-destructive",
          )}
        >
          {task.error}
        </p>
      ) : null}
    </div>
  );
}
