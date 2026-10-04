/** Lines from the local server's request log, fed by `server-log` events. */

import { create } from "zustand";

import type { ServerLogEvent } from "@/lib/types";

export interface ServerLine extends ServerLogEvent {
  id: string;
}

const MAX_LINES = 200;

interface ServerLogState {
  lines: ServerLine[];
  append: (line: ServerLogEvent) => void;
  clear: () => void;
}

let sequence = 0;

export const useServerLog = create<ServerLogState>((set) => ({
  lines: [],
  append: (line) =>
    set((state) => {
      sequence += 1;
      return { lines: [...state.lines, { ...line, id: `line-${sequence}` }].slice(-MAX_LINES) };
    }),
  clear: () => set({ lines: [] }),
}));
