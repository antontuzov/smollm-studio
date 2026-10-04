/**
 * Engine and download state shared across pages.
 *
 * The chat transcript and the local server act on the same loaded model, so
 * "what is loaded right now" belongs here rather than inside one page.
 */

import { create } from "zustand";

import type { DownloadUpdate } from "@/lib/api";
import type {
  DownloadProgress,
  DownloadTask,
  EngineMetrics,
  LoadModelResponse,
  ModelHandle,
} from "@/lib/types";

interface EngineState {
  handle: ModelHandle | null;
  metrics: EngineMetrics | null;
  warnings: string[];
  busy: boolean;
  applyLoad: (response: LoadModelResponse) => void;
  applyMetrics: (metrics: EngineMetrics) => void;
  setBusy: (busy: boolean) => void;
  reset: () => void;
}

export const useEngine = create<EngineState>((set) => ({
  handle: null,
  metrics: null,
  warnings: [],
  busy: false,
  applyLoad: (response) =>
    set({
      handle: response.handle,
      metrics: null,
      warnings: response.warnings,
      busy: false,
    }),
  applyMetrics: (metrics) =>
    set((state) => ({
      metrics,
      // An unload reports `loaded: false`, and the handle goes with it.
      handle: metrics.loaded ? state.handle : null,
    })),
  setBusy: (busy) => set({ busy }),
  reset: () => set({ handle: null, metrics: null, warnings: [], busy: false }),
}));

interface DownloadsState {
  tasks: Record<string, DownloadTask>;
  live: Record<string, DownloadProgress>;
  applySnapshot: (tasks: DownloadTask[]) => void;
  applyTask: (task: DownloadTask) => void;
  applyUpdate: (update: DownloadUpdate) => void;
}

function withoutKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}

export const useDownloads = create<DownloadsState>((set) => ({
  tasks: {},
  live: {},
  applySnapshot: (tasks) =>
    set(() => ({ tasks: Object.fromEntries(tasks.map((task) => [task.id, task])) })),
  applyTask: (task) => set((state) => ({ tasks: { ...state.tasks, [task.id]: task } })),
  applyUpdate: (update) => {
    if (update.kind === "progress") {
      const progress = update.payload;
      set((state) => ({ live: { ...state.live, [progress.downloadId]: progress } }));
      return;
    }
    // A terminal event means the live row is no longer interesting.
    const { downloadId } = update.payload;
    set((state) => ({ live: withoutKey(state.live, downloadId) }));
  },
}));

/** Transfers still moving, newest first. */
export function selectActiveTasks(tasks: Record<string, DownloadTask>): DownloadTask[] {
  return Object.values(tasks)
    .filter(
      (task) => task.state === "queued" || task.state === "running" || task.state === "verifying",
    )
    .sort((left, right) => right.startedMs - left.startedMs);
}
