/**
 * The single Rust -> React event bridge.
 *
 * Subscriptions live here so a stream keeps filling the transcript even when the
 * user is on another page, and so exactly one place owns cache invalidation and
 * the shared stores.
 */

import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { events, onDownloadUpdates } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { useDownloadSnapshot, useEngineMetrics, queryKeys } from "@/lib/queries";
import { useBenchmark } from "@/stores/benchmark";
import { useChat } from "@/stores/chat";
import { useDownloads, useEngine } from "@/stores/engine";
import { useServerLog } from "@/stores/server-log";
import { toast } from "@/stores/ui";

import type { UnlistenFn } from "@tauri-apps/api/event";

export function EventBridge() {
  const queryClient = useQueryClient();
  const metrics = useEngineMetrics();
  const downloads = useDownloadSnapshot();

  useEffect(() => {
    if (metrics.data) {
      useEngine.getState().applyMetrics(metrics.data);
    }
  }, [metrics.data]);

  useEffect(() => {
    if (downloads.data) {
      useDownloads.getState().applySnapshot(downloads.data);
    }
  }, [downloads.data]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    const register = (subscription: Promise<UnlistenFn>) => {
      subscription.then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      });
    };

    // A benchmark failure also arrives on `chat-error`, and a late event from a
    // replaced request must never write into the current transcript.
    const isCurrentChat = (requestId: string) => useChat.getState().requestId === requestId;

    register(
      events.onChatToken((event) => {
        if (isCurrentChat(event.requestId)) {
          useChat.getState().appendToken(event.token.text);
        }
      }),
    );
    register(
      events.onChatDone((event) => {
        if (!isCurrentChat(event.requestId)) {
          return;
        }
        useChat.getState().finishStream({
          tokensPerSecond: event.tokensPerSecond,
          elapsedMs: event.elapsedMs,
          usage: event.usage ?? null,
          finishReason: event.finishReason,
          simulated: event.simulated,
        });
        // Token counters moved, so the metrics panels should re-read them.
        void queryClient.invalidateQueries({ queryKey: queryKeys.engineMetrics });
      }),
    );
    register(
      events.onChatError((event) => {
        if (isCurrentChat(event.requestId)) {
          useChat.getState().failStream(event.message);
          return;
        }
        if (event.requestId === "benchmark") {
          useBenchmark.getState().setRunning(false);
          toast({ title: "Benchmark failed", description: event.message, variant: "error" });
        }
      }),
    );
    register(events.onServerLog((event) => useServerLog.getState().append(event)));
    register(events.onBenchmarkProgress((progress) => useBenchmark.getState().setProgress(progress)));
    register(
      onDownloadUpdates((update) => {
        useDownloads.getState().applyUpdate(update);
        if (update.kind === "progress") {
          return;
        }
        void queryClient.invalidateQueries({ queryKey: queryKeys.downloads });
        void queryClient.invalidateQueries({ queryKey: queryKeys.localModels });
        // `downloaded` is part of every catalog entry, so the list is stale now.
        void queryClient.invalidateQueries({ queryKey: ["catalog"] });

        if (update.kind === "complete") {
          toast({
            title: "Download finished",
            description: `${update.payload.modelId} · ${formatBytes(update.payload.sizeBytes)}`,
            variant: "success",
          });
        } else if (update.kind === "error") {
          toast({
            title: "Download failed",
            description: `${update.payload.message} (${update.payload.code})`,
            variant: "error",
          });
        } else {
          toast({
            title: "Download cancelled",
            description: `${formatBytes(update.payload.partialBytes)} kept as a .part file for the next attempt.`,
            variant: "warning",
          });
        }
      }),
    );

    return () => {
      disposed = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, [queryClient]);

  return null;
}
