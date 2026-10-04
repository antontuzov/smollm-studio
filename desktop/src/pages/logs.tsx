import { useState } from "react";
import { Download, FileText, RefreshCw, ScrollText, Trash2 } from "lucide-react";

import { api } from "@/lib/api";
import { exportDiagnostics, openFolder } from "@/lib/actions";
import { useQuery } from "@tanstack/react-query";
import { describeError, formatTime } from "@/lib/format";
import { queryKeys } from "@/lib/queries";
import { capitalize, levelTone } from "@/lib/presentation";
import { toast } from "@/stores/ui";
import { PageHeader } from "@/components/page-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input, Select, Switch } from "@/components/ui/field";
import { EmptyState, ErrorState, SkeletonList } from "@/components/ui/feedback";
import { cn } from "@/lib/utils";

import type { LogFilter, LogStream } from "@/lib/types";

const streams: { value: string; label: string }[] = [
  { value: "", label: "All subsystems" },
  { value: "app", label: "App" },
  { value: "engine", label: "Engine" },
  { value: "download", label: "Downloads" },
  { value: "server", label: "Server" },
];

const levels = ["trace", "debug", "info", "warn", "error"];
const LOG_LIMIT = 400;

export function LogsPage() {
  const [stream, setStream] = useState("");
  const [contains, setContains] = useState("");
  const [selectedLevels, setSelectedLevels] = useState<string[]>([]);
  const [autoRefresh, setAutoRefresh] = useState(true);

  const filter: LogFilter = {
    levels: selectedLevels,
    stream: stream.length > 0 ? (stream as LogStream) : null,
    contains: contains.length > 0 ? contains : null,
    limit: LOG_LIMIT,
  };

  const logs = useQuery({
    queryKey: [...queryKeys.logs, filter] as const,
    queryFn: () => api.getLogs(filter),
    refetchInterval: autoRefresh ? 2000 : false,
  });

  const toggleLevel = (level: string) =>
    setSelectedLevels((current) =>
      current.includes(level) ? current.filter((entry) => entry !== level) : [...current, level],
    );

  const problemsOnly =
    selectedLevels.length > 0 &&
    selectedLevels.every((level) => level === "error" || level === "warn");

  const clear = async () => {
    try {
      const removed = await api.clearLogs();
      await logs.refetch();
      toast({ title: "Buffer cleared", description: `${removed} line(s) dropped.` });
    } catch (error) {
      toast({ title: "Could not clear the buffer", description: describeError(error), variant: "error" });
    }
  };

  // The buffer returns oldest-first; the page reads newest-first like a console.
  const entries = (logs.data ?? []).slice().reverse();

  return (
    <>
      <PageHeader
        title="Logs"
        description="The in-memory ring buffer behind the app: engine decisions, download state, server traffic and errors. Rotating files live in the logs folder."
        icon={ScrollText}
        actions={
          <>
            <Button variant="outline" onClick={() => void logs.refetch()}>
              <RefreshCw />
              Refresh
            </Button>
            <Button variant="outline" onClick={() => void openFolder("logs")}>
              <FileText />
              Log folder
            </Button>
            <Button variant="outline" onClick={() => void exportDiagnostics()}>
              <Download />
              Export diagnostics
            </Button>
            <Button variant="ghost" onClick={() => void clear()}>
              <Trash2 />
              Clear
            </Button>
          </>
        }
      />

      <div className="space-y-4 p-6">
        <Card>
          <CardContent className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
            <Select label="Subsystem" value={stream} options={streams} onChange={setStream} />
            <div className="space-y-2">
              <label htmlFor="log-search" className="text-xs font-medium text-muted-foreground">
                Contains
              </label>
              <Input
                id="log-search"
                value={contains}
                placeholder="qwen2.5, checksum…"
                onChange={(event) => setContains(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <span className="text-xs font-medium text-muted-foreground">Levels</span>
              <div className="flex flex-wrap gap-1.5">
                {levels.map((level) => {
                  const active = selectedLevels.includes(level);
                  return (
                    <button
                      key={level}
                      type="button"
                      onClick={() => toggleLevel(level)}
                      aria-pressed={active}
                      className={cn(
                        "rounded-md border px-2 py-1 text-[11px] transition-colors",
                        active
                          ? "border-primary/40 bg-primary/15 text-primary"
                          : "text-muted-foreground hover:text-foreground",
                      )}
                    >
                      {capitalize(level)}
                    </button>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-col justify-end gap-3">
              <Switch
                checked={autoRefresh}
                onCheckedChange={setAutoRefresh}
                label="Auto-refresh (2 s)"
              />
              <Switch
                checked={problemsOnly}
                onCheckedChange={(checked) =>
                  setSelectedLevels(checked ? ["error", "warn"] : [])
                }
                label="Problems only"
              />
            </div>
          </CardContent>
        </Card>

        {logs.isPending ? (
          <SkeletonList rows={8} />
        ) : logs.isError ? (
          <ErrorState
            message="The log buffer could not be read"
            detail={describeError(logs.error)}
            onRetry={() => void logs.refetch()}
          />
        ) : entries.length === 0 ? (
          <EmptyState
            icon={ScrollText}
            title="No log lines match"
            description="Loosen the filters, or do something in the app first: every command writes at least one line."
          />
        ) : (
          <Card>
            <CardContent className="p-0">
              <ul className="max-h-[60vh] divide-y overflow-y-auto">
                {entries.map((entry, index) => (
                  <li key={`${entry.timestampMs}-${index}`} className="flex gap-3 px-4 py-2 text-xs">
                    <span className="w-16 shrink-0 font-mono text-muted-foreground">
                      {formatTime(entry.timestampMs)}
                    </span>
                    <Badge tone={levelTone(entry.level)} className="w-16 shrink-0 justify-center uppercase">
                      {entry.level}
                    </Badge>
                    <Badge tone="neutral" className="w-20 shrink-0 justify-center">
                      {entry.stream}
                    </Badge>
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-mono text-[11px] text-muted-foreground">
                        {entry.target}
                      </span>
                      <span className="break-words">{entry.message}</span>
                    </span>
                  </li>
                ))}
              </ul>
            </CardContent>
          </Card>
        )}

        <p className="text-[11px] text-muted-foreground">
          {entries.length} line(s) shown, newest first. The buffer holds the last few thousand lines
          for this session; older lines are in the rotated files.
        </p>
      </div>
    </>
  );
}
