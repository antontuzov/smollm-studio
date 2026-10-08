import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  Bot,
  Eraser,
  Gauge,
  MessageSquare,
  RotateCw,
  Send,
  Square,
  UserRound,
  X,
} from "lucide-react";

import { api } from "@/lib/api";
import { loadModel } from "@/lib/actions";
import { describeError, formatDuration, formatRate } from "@/lib/format";
import { useStickToBottom } from "@/lib/use-stick-to-bottom";
import { useCatalog, usePresets, useSettingsQuery } from "@/lib/queries";
import { cn, nextId } from "@/lib/utils";
import { useChat } from "@/stores/chat";
import { useEngine } from "@/stores/engine";
import { toast } from "@/stores/ui";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { CopyButton } from "@/components/ui/copy";
import { Select, Textarea } from "@/components/ui/field";
import { EmptyState, Note, Progress, SkeletonList } from "@/components/ui/feedback";
import { Markdown } from "@/components/ui/markdown";
import { SamplingPanel } from "@/components/sampling-panel";

import type { ChatMessage, ChatRequest, SamplingParams } from "@/lib/types";

const fallbackParams: SamplingParams = {
  temperature: 0.7,
  topP: 0.9,
  topK: 40,
  minP: 0.05,
  maxTokens: 512,
  repeatPenalty: 1.1,
  presencePenalty: 0,
  seed: 42,
};

export function ChatPage() {
  const turns = useChat((state) => state.turns);
  const draft = useChat((state) => state.draft);
  const setDraft = useChat((state) => state.setDraft);
  const systemPrompt = useChat((state) => state.systemPrompt);
  const setSystemPrompt = useChat((state) => state.setSystemPrompt);
  const preset = useChat((state) => state.preset);
  const setPreset = useChat((state) => state.setPreset);
  const params = useChat((state) => state.params);
  const setParams = useChat((state) => state.setParams);
  const streaming = useChat((state) => state.streaming);
  const requestId = useChat((state) => state.requestId);
  const stats = useChat((state) => state.stats);
  const lastError = useChat((state) => state.lastError);
  const beginStream = useChat((state) => state.beginStream);
  const failStream = useChat((state) => state.failStream);
  const stopStream = useChat((state) => state.stopStream);
  const clear = useChat((state) => state.clear);
  const removeTurn = useChat((state) => state.removeTurn);
  const retryLast = useChat((state) => state.retryLast);

  const handle = useEngine((state) => state.handle);
  const busy = useEngine((state) => state.busy);
  const presets = usePresets();
  const settings = useSettingsQuery();
  const catalog = useCatalog({ query: "", sort: "recommended", hidePlaceholders: true });
  const [modelId, setModelId] = useState(handle?.modelId ?? "");
  const [showParams, setShowParams] = useState(false);

  const input = useRef<HTMLTextAreaElement>(null);
  // Character count, not turn count: streamed tokens change the length of the
  // last turn, and that is what should pull the transcript down.
  const contentSize = turns.reduce((total, turn) => total + turn.content.length, 0);
  // The transcript follows new tokens only while the reader is at the bottom;
  // otherwise their scroll position is theirs.
  const {
    ref: transcriptRef,
    pinned,
    trackScroll,
    scrollToBottom,
  } = useStickToBottom<HTMLDivElement>(contentSize);

  const downloaded = useMemo(
    () => (catalog.data ?? []).filter((model) => model.downloaded),
    [catalog.data],
  );

  // Only the newest answer can be regenerated; older ones are history.
  const lastAssistantId = useMemo(() => {
    for (let index = turns.length - 1; index >= 0; index -= 1) {
      if (turns[index].role === "assistant") {
        return turns[index].id;
      }
    }
    return null;
  }, [turns]);

  const modelOptions = useMemo(() => {
    const options = downloaded.map((model) => ({ value: model.id, label: model.displayName }));
    if (handle && !options.some((option) => option.value === handle.modelId)) {
      options.unshift({ value: handle.modelId, label: `${handle.displayName} (loaded)` });
    }
    return options;
  }, [downloaded, handle]);

  useEffect(() => {
    if (handle && modelId.length === 0) {
      setModelId(handle.modelId);
    }
  }, [handle, modelId]);

  // Seed sampling from the saved chat preset once the presets arrive.
  useEffect(() => {
    if (params || !presets.data || presets.data.length === 0) {
      return;
    }
    const wanted = presets.data.find((entry) => entry.name === settings.data?.chatPreset)
      ?? presets.data.find((entry) => entry.name === "balanced")
      ?? presets.data[0];
    setPreset(wanted.name, wanted.params);
  }, [params, presets.data, settings.data, setPreset]);

  const send = useCallback(
    async (text: string, history?: ChatMessage[]) => {
      const trimmed = text.trim();
      if (trimmed.length === 0 || streaming) {
        return;
      }
      if (modelId.length === 0) {
        toast({
          title: "No model selected",
          description: "Download a model on the Models page, or pick one above.",
          variant: "warning",
        });
        return;
      }
      const id = nextId("chat");
      const assistantId = nextId("turn");
      const previous: ChatMessage[] = (
        history ??
        turns
          .filter((turn) => turn.content.length > 0)
          .map((turn) => ({ role: turn.role, content: turn.content }))
      );
      const request: ChatRequest = {
        requestId: id,
        modelId,
        messages: [...previous, { role: "user", content: trimmed }],
        systemPrompt,
        params: params ?? fallbackParams,
        stop: [],
      };
      beginStream(trimmed, id, assistantId);
      setDraft("");
      scrollToBottom();
      try {
        await api.startChatStream(request);
      } catch (error) {
        failStream(describeError(error));
      }
    },
    [beginStream, failStream, modelId, params, setDraft, scrollToBottom, streaming, systemPrompt, turns],
  );

  const stop = useCallback(async () => {
    if (!requestId) {
      stopStream();
      return;
    }
    const id = requestId;
    try {
      await api.stopGeneration(id);
      // The engine's own `chat-done` event closes the turn; nothing to do here.
    } catch (error) {
      // The stream may already have finished between the click and the command.
      stopStream();
      toast({ title: "Nothing to stop", description: describeError(error), variant: "warning" });
    }
  }, [requestId, stopStream]);

  const retry = useCallback(() => {
    const history = retryLast();
    if (!history || history.length === 0) {
      return;
    }
    const prompt = history[history.length - 1];
    void send(prompt.content, history.slice(0, -1));
  }, [retryLast, send]);

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 p-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
            <MessageSquare className="size-4" />
            Chat
          </h1>
          <p className="text-xs text-muted-foreground">
            {handle
              ? `${handle.displayName} · ${handle.engine} · ${handle.contextLength} token context`
              : "Nothing is resident: the first message loads the model you choose."}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <StatusPill
            tone={streaming ? "info" : handle ? "success" : "neutral"}
            pulse={streaming}
            label={streaming ? "generating" : handle ? "ready" : "no model loaded"}
          />
          {stats ? (
            <Badge tone={stats.simulated ? "warning" : "primary"}>
              {formatRate(stats.tokensPerSecond)} tok/s · {formatDuration(stats.elapsedMs)}
            </Badge>
          ) : null}
          <Button variant="outline" size="sm" onClick={() => setShowParams((value) => !value)}>
            <Gauge />
            {showParams ? "Hide parameters" : "Parameters"}
          </Button>
          <Button variant="ghost" size="sm" onClick={clear} disabled={turns.length === 0}>
            <Eraser />
            Clear
          </Button>
        </div>
      </div>

      {stats?.simulated ? (
        <Note tone="warning" icon={Gauge}>
          <p>
            These tokens came from the mock engine, so the wording is deterministic filler and the
            speed is a modelled number. This binary carries no engine that reads weights; build it
            with <span className="font-mono">--features llama-cpp</span> to run a real model, whose
            own tokenizer, chat template and device the numbers then describe.
          </p>
        </Note>
      ) : null}

      <div className="grid min-h-0 flex-1 gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
        <Card className="relative min-h-0">
          <div
            ref={transcriptRef}
            onScroll={trackScroll}
            className="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4"
          >
            {turns.length === 0 ? (
              <EmptyState
                icon={MessageSquare}
                title="No messages yet"
                description="Ask something short to see how streaming feels. The transcript stays in memory for this session only."
              />
            ) : (
              turns.map((turn) => (
                <div key={turn.id} className="flex animate-rise-in gap-3">
                  <span
                    aria-hidden
                    className={
                      turn.role === "user"
                        ? "flex size-8 shrink-0 items-center justify-center rounded-lg bg-secondary text-secondary-foreground"
                        : "flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary/15 text-primary"
                    }
                  >
                    {turn.role === "user" ? (
                      <UserRound className="size-4" />
                    ) : (
                      <Bot className="size-4" />
                    )}
                  </span>
                  <div className="group min-w-0 flex-1 space-y-1.5">
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
                        {turn.role === "user" ? "You" : "Model"}
                      </span>
                      {/* Visible on hover *and* on keyboard focus, so nobody has
                          to discover these by mouse. */}
                      <span className="flex items-center gap-1 opacity-0 transition-opacity duration-150 group-focus-within:opacity-100 group-hover:opacity-100">
                        {turn.content.length > 0 ? (
                          <CopyButton text={turn.content} label="Copy message" />
                        ) : null}
                        {turn.role === "assistant" && turn.id === lastAssistantId && !streaming ? (
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label="Regenerate answer"
                            title="Regenerate answer"
                            onClick={retry}
                          >
                            <RotateCw />
                          </Button>
                        ) : null}
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="Remove message"
                          onClick={() => removeTurn(turn.id)}
                        >
                          <X />
                        </Button>
                      </span>
                    </div>
                    <div
                      aria-busy={turn.streaming || undefined}
                      className={cn(
                        "rounded-lg border px-3.5 py-2.5",
                        turn.role === "user"
                          ? "border-transparent bg-secondary/60"
                          : "bg-background/50",
                      )}
                    >
                      {turn.content.length === 0 && turn.streaming ? (
                        <span className="flex items-center gap-2 text-xs text-muted-foreground">
                          <Progress />
                          waiting for the first token
                        </span>
                      ) : turn.role === "user" ? (
                        <p className="whitespace-pre-wrap text-sm leading-relaxed">{turn.content}</p>
                      ) : (
                        <>
                          <Markdown content={turn.content} />
                          {turn.streaming ? (
                            <span
                              aria-hidden
                              className="ml-0.5 inline-block h-4 w-1.5 animate-pulse-dot rounded-sm bg-primary align-middle"
                            />
                          ) : null}
                        </>
                      )}
                      {turn.error ? (
                        <p className="mt-2 text-xs text-destructive">{turn.error}</p>
                      ) : null}
                    </div>
                  </div>
                </div>
              ))
            )}
          </div>

          {pinned || turns.length === 0 ? null : (
            <Button
              variant="secondary"
              size="sm"
              className="absolute bottom-3 left-1/2 -translate-x-1/2 animate-rise-in rounded-full shadow-panel"
              onClick={() => scrollToBottom(true)}
            >
              <ArrowDown />
              Jump to latest
            </Button>
          )}

          <div className="space-y-2 border-t px-5 py-3">
            {lastError ? (
              <div className="flex items-center justify-between gap-2 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2">
                <span className="min-w-0 truncate text-xs text-destructive">{lastError}</span>
                <Button variant="outline" size="sm" onClick={retry}>
                  <RotateCw />
                  Retry
                </Button>
              </div>
            ) : null}
            <div className="flex items-end gap-2">
              <Textarea
                ref={input}
                value={draft}
                rows={2}
                placeholder="Ask the model…  (Enter to send, Shift+Enter for a newline)"
                className="min-h-[52px] flex-1"
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    void send(draft);
                  }
                }}
              />
              {streaming ? (
                <Button variant="destructive" onClick={() => void stop()}>
                  <Square />
                  Stop
                </Button>
              ) : (
                <Button onClick={() => void send(draft)} disabled={draft.trim().length === 0}>
                  <Send />
                  Send
                </Button>
              )}
            </div>
          </div>
        </Card>

        <div className="min-h-0 space-y-4 overflow-y-auto pr-0.5">
          <Card>
            <CardHeader
              title="Model"
              description={handle ? `Resident: ${handle.engine}` : "Choose what to talk to."}
            />
            <CardContent className="space-y-3">
              {catalog.isPending ? (
                <SkeletonList rows={2} />
              ) : catalog.isError ? (
                <Note tone="danger">
                  <p>{describeError(catalog.error)}</p>
                </Note>
              ) : modelOptions.length === 0 ? (
                <Note tone="warning">
                  <p className="font-medium">No downloaded models yet</p>
                  <p>
                    The chat can only answer from a local file. Start a download on the Models page
                    first.
                  </p>
                </Note>
              ) : null}
              <div className="flex gap-2">
                <Select
                  className="flex-1"
                  value={modelId}
                  options={modelOptions}
                  disabled={busy || streaming}
                  onChange={setModelId}
                />
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busy || streaming || modelId.length === 0}
                  onClick={async () => {
                    if (await loadModel(modelId)) {
                      input.current?.focus();
                    }
                  }}
                >
                  Load
                </Button>
              </div>
              {handle && handle.modelId === modelId ? (
                <p className="truncate font-mono text-[11px] text-muted-foreground">{handle.path}</p>
              ) : null}
            </CardContent>
          </Card>

          <Card>
            <CardHeader title="System prompt" description="Sent ahead of every transcript." />
            <CardContent>
              <Textarea
                value={systemPrompt}
                rows={4}
                onChange={(event) => setSystemPrompt(event.target.value)}
              />
            </CardContent>
          </Card>

          {showParams ? (
            <Card>
              <CardHeader
                title="Sampling"
                description={
                  settings.isPending
                    ? "Loading presets…"
                    : "Ranges match what the engine validates server-side."
                }
              />
              <CardContent>
                {presets.isPending || !params ? (
                  <SkeletonList rows={3} />
                ) : (
                  <SamplingPanel
                    presets={presets.data ?? []}
                    preset={preset}
                    params={params}
                    onPreset={setPreset}
                    onParams={setParams}
                    disabled={streaming}
                  />
                )}
              </CardContent>
            </Card>
          ) : null}
        </div>
      </div>
    </div>
  );
}
