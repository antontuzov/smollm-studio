import { useEffect, useState } from "react";
import { ArrowDown, CirclePlay, Copy, Server, Square, Terminal } from "lucide-react";

import { startServer, stopServer } from "@/lib/actions";
import { describeError, formatTime, formatUptime } from "@/lib/format";
import { useStickToBottom } from "@/lib/use-stick-to-bottom";
import { useCatalog, useServerExamples, useServerStatus, useSettingsQuery } from "@/lib/queries";
import { levelTone } from "@/lib/presentation";
import { useServerLog } from "@/stores/server-log";
import { useEngine } from "@/stores/engine";
import { PageHeader } from "@/components/page-header";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, Stat } from "@/components/ui/card";
import { CodeBlock, CopyButton } from "@/components/ui/copy";
import { Input, NumberField, Select } from "@/components/ui/field";
import { EmptyState, Note, SkeletonList } from "@/components/ui/feedback";
import { Tabs } from "@/components/ui/tabs";

import type { ServerConfig } from "@/lib/types";

export function ServerPage() {
  const status = useServerStatus();
  const settings = useSettingsQuery();
  const catalog = useCatalog({ query: "", sort: "recommended", hidePlaceholders: true });
  const handle = useEngine((state) => state.handle);
  const lines = useServerLog((state) => state.lines);
  const clearLines = useServerLog((state) => state.clear);
  // The request log tails itself, unless you have scrolled up to read a line.
  const {
    ref: logRef,
    pinned: logPinned,
    trackScroll: trackLogScroll,
    scrollToBottom: scrollLogToBottom,
  } = useStickToBottom<HTMLUListElement>(lines[lines.length - 1]?.id ?? "empty");

  const [host, setHost] = useState("127.0.0.1");
  const [port, setPort] = useState(8080);
  const [modelId, setModelId] = useState("");
  const [exampleTab, setExampleTab] = useState<"curl" | "python">("curl");

  // Defaults come from the saved settings, and stop coming from Rust once the
  // user edits the field.
  useEffect(() => {
    if (!settings.data) {
      return;
    }
    setHost(settings.data.serverHost);
    setPort(settings.data.serverPort);
  }, [settings.data]);

  useEffect(() => {
    if (status.data && status.data.running) {
      setHost(status.data.host);
      setPort(status.data.port);
    }
  }, [status.data]);

  const servingModel = modelId || handle?.modelId || "";
  const examples = useServerExamples(servingModel);
  const running = status.data?.running ?? false;
  const config: ServerConfig = { host, port, defaultModelId: modelId.length > 0 ? modelId : null };

  const modelOptions = (catalog.data ?? [])
    .filter((model) => model.downloaded)
    .map((model) => ({ value: model.id, label: model.displayName }));
  if (handle && !modelOptions.some((option) => option.value === handle.modelId)) {
    modelOptions.unshift({ value: handle.modelId, label: `${handle.displayName} (loaded)` });
  }

  return (
    <>
      <PageHeader
        title="Local server"
        description="An OpenAI-compatible HTTP API on loopback, backed by the same engine the chat uses. Nothing is reachable from the network."
        icon={Server}
        actions={
          <>
            <StatusPill
              tone={running ? "success" : "neutral"}
              pulse={running}
              label={running ? `listening · ${status.data?.baseUrl ?? ""}` : "stopped"}
            />
            {running ? (
              <Button variant="destructive" onClick={() => void stopServer()}>
                <Square />
                Stop server
              </Button>
            ) : (
              <Button onClick={() => void startServer(config)}>
                <CirclePlay />
                Start server
              </Button>
            )}
          </>
        }
      />

      <div className="grid gap-5 p-6 xl:grid-cols-2">
        <div className="space-y-5">
          <Card>
            <CardHeader
              title="Endpoint"
              description="Only 127.0.0.1 and localhost are accepted, on purpose."
              actions={<Badge tone={running ? "success" : "neutral"}>{running ? "live" : "idle"}</Badge>}
            />
            <CardContent className="space-y-4">
              <div className="grid gap-4 sm:grid-cols-2">
                <div className="space-y-2">
                  <label htmlFor="server-host" className="text-xs font-medium text-muted-foreground">
                    Host
                  </label>
                  <Input
                    id="server-host"
                    value={host}
                    disabled={running}
                    onChange={(event) => setHost(event.target.value)}
                    placeholder="127.0.0.1"
                  />
                </div>
                <NumberField
                  label="Port"
                  value={port}
                  min={1}
                  max={65_535}
                  disabled={running}
                  onChange={setPort}
                />
              </div>

              <Select
                label="Default model"
                value={modelId}
                options={[
                  { value: "", label: handle ? handle.displayName : "Whatever is loaded" },
                  ...modelOptions,
                ]}
                disabled={running}
                onChange={setModelId}
                hint="Requests may still name a model explicitly; this is the fallback."
              />

              <div className="flex flex-wrap items-center gap-2">
                {running ? (
                  <Button variant="outline" onClick={() => void stopServer()}>
                    <Square />
                    Stop
                  </Button>
                ) : (
                  <Button onClick={() => void startServer(config)}>
                    <CirclePlay />
                    Start
                  </Button>
                )}
                {status.data?.baseUrl ? (
                  <span className="flex items-center gap-1 rounded-md border bg-background/60 px-2 py-1 font-mono text-xs">
                    {status.data.baseUrl}
                    <CopyButton text={status.data.baseUrl} label="Copy base URL" />
                  </span>
                ) : null}
              </div>

              {status.isError ? (
                <Note tone="danger">
                  <p>{describeError(status.error)}</p>
                </Note>
              ) : null}

              {status.data?.simulated ? (
                <Note tone="warning" icon={Terminal}>
                  <p>
                    The engine is the mock, so <span className="font-mono">/v1/chat/completions</span>{" "}
                    returns deterministic filler. The routes, streaming and error shapes are real.
                  </p>
                </Note>
              ) : null}
            </CardContent>
            <CardFooter>
              <span className="text-[11px] text-muted-foreground">
                API key checks are intentionally absent: this server only binds to loopback.
              </span>
            </CardFooter>
          </Card>

          <Card>
            <CardHeader title="Traffic" description="Since this server was started." />
            <CardContent>
              {status.isPending ? (
                <SkeletonList rows={1} />
              ) : (
                <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                  <Stat label="Requests" value={status.data?.requests ?? 0} />
                  <Stat
                    label="Uptime"
                    value={running ? formatUptime(status.data?.uptimeSeconds ?? 0) : "—"}
                  />
                  <Stat
                    label="Model"
                    value={
                      <span className="text-base leading-snug">
                        {status.data?.loadedModel ?? "none resident"}
                      </span>
                    }
                  />
                  <Stat label="Engine" value={<span className="text-base">{status.data?.engine ?? "—"}</span>} />
                </div>
              )}
            </CardContent>
          </Card>

          <Card className="relative">
            <CardHeader
              title="Request log"
              description="Live lines from the HTTP server."
              actions={
                <Button variant="ghost" size="sm" onClick={clearLines} disabled={lines.length === 0}>
                  Clear
                </Button>
              }
            />
            <CardContent>
              {lines.length === 0 ? (
                <EmptyState
                  icon={Terminal}
                  title="No requests yet"
                  description="Call the endpoint with one of the examples and the method, path, model and token count land here."
                />
              ) : (
                <ul
                  ref={logRef}
                  onScroll={trackLogScroll}
                  className="max-h-72 space-y-1 overflow-y-auto font-mono text-[11px]"
                >
                  {lines.map((line) => (
                    <li
                      key={line.id}
                      className="flex animate-rise-in items-start gap-2 rounded px-1 py-0.5 hover:bg-secondary/40"
                    >
                      <span className="shrink-0 text-muted-foreground">{formatTime(line.timestampMs)}</span>
                      <Badge tone={levelTone(line.level)} className="shrink-0 uppercase">
                        {line.level}
                      </Badge>
                      <span className="min-w-0 flex-1 break-words">{line.message}</span>
                    </li>
                  ))}
                </ul>
              )}
              {logPinned || lines.length === 0 ? null : (
                <Button
                  variant="secondary"
                  size="sm"
                  className="absolute bottom-3 left-1/2 -translate-x-1/2 animate-rise-in rounded-full shadow-panel"
                  onClick={() => scrollLogToBottom(true)}
                >
                  <ArrowDown />
                  Jump to latest
                </Button>
              )}
            </CardContent>
          </Card>
        </div>

        <div className="space-y-5">
          <Card>
            <CardHeader
              title="Client examples"
              description={
                examples.data
                  ? `Model in the snippet: ${examples.data.model}`
                  : "Generated by the backend so the port, model and path are always correct."
              }
              actions={
                examples.data ? (
                  <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                    <Copy className="size-3" />
                    ready to paste
                  </span>
                ) : null
              }
            />
            <CardContent className="space-y-4">
              {examples.isError ? (
                <ErrorPlaceholder message={describeError(examples.error)} />
              ) : examples.data ? (
                <>
                  <Tabs
                    value={exampleTab}
                    onChange={setExampleTab}
                    items={[
                      { id: "curl", label: "curl", icon: Terminal },
                      { id: "python", label: "Python", icon: Server },
                    ]}
                  />
                  {exampleTab === "curl" ? (
                    <CodeBlock language="bash" code={examples.data.curl} />
                  ) : (
                    <CodeBlock language="python" code={examples.data.python} />
                  )}
                </>
              ) : (
                <EmptyState
                  icon={Terminal}
                  title="Nothing to show yet"
                  description="Load a model (or start the server with one resident) and both snippets appear here, copied straight from the live endpoint."
                />
              )}
            </CardContent>
            <CardFooter>
              <span className="text-[11px] leading-relaxed text-muted-foreground">
                <span className="font-mono">/v1/chat/completions</span>,{" "}
                <span className="font-mono">/v1/completions</span>,{" "}
                <span className="font-mono">/v1/models</span> and{" "}
                <span className="font-mono">/v1/engine/metrics</span> are implemented;{" "}
                <span className="font-mono">stream: true</span>{" "}
                returns server-sent events.
              </span>
            </CardFooter>
          </Card>

          <Card>
            <CardHeader title="Notes" description="What is real here and what is not." />
            <CardContent className="space-y-3 text-xs leading-relaxed text-muted-foreground">
              <p>
                Requests are served by the model resident in the engine. If nothing is loaded, the
                server loads the default model on the first request, which is why the very first call
                can be slower.
              </p>
              <p>
                The server is started inside this app process. Stopping the app stops the API; there
                is no background daemon and no autostart.
              </p>
              <p>
                Bind addresses other than loopback are refused by the backend. If you need to reach
                the API from another machine, use an SSH tunnel rather than opening a port.
              </p>
            </CardContent>
          </Card>
        </div>
      </div>
    </>
  );
}

function ErrorPlaceholder({ message }: { message: string }) {
  return (
    <Note tone="danger">
      <p className="font-medium">The examples could not be generated</p>
      <p>{message}</p>
    </Note>
  );
}
