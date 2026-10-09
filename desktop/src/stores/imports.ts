/**
 * Files coming in from outside the app.
 *
 * An import is a multi-gigabyte local copy, so it needs a busy state that
 * outlives whichever component started it: a drop is handled at the window
 * level, while the button that shows the progress is on the Library page.
 */

import { create } from "zustand";

interface ImportsState {
  /** A model file is being dragged over the window right now. */
  dragging: boolean;
  /** File names whose copy is in flight, in the order they started. */
  copying: string[];
  setDragging: (dragging: boolean) => void;
  start: (fileName: string) => void;
  finish: (fileName: string) => void;
}

export const useImports = create<ImportsState>((set) => ({
  dragging: false,
  copying: [],
  setDragging: (dragging) => set({ dragging }),
  start: (fileName) => set((state) => ({ copying: [...state.copying, fileName] })),
  finish: (fileName) =>
    set((state) => ({ copying: state.copying.filter((name) => name !== fileName) })),
}));
