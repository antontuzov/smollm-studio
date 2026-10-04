/** Benchmark progress arrives as an event, so it lives in a store. */

import { create } from "zustand";

import type { BenchmarkProgress } from "@/lib/types";

interface BenchmarkState {
  progress: BenchmarkProgress | null;
  running: boolean;
  setRunning: (running: boolean) => void;
  setProgress: (progress: BenchmarkProgress | null) => void;
}

export const useBenchmark = create<BenchmarkState>((set) => ({
  progress: null,
  running: false,
  setRunning: (running) => set({ running }),
  setProgress: (progress) => set({ progress }),
}));
