/** Navigation, theme and toasts: the UI state that never touches Rust. */

import { create } from "zustand";

import type { ThemeMode } from "@/lib/types";

export type PageId =
  | "home"
  | "models"
  | "library"
  | "chat"
  | "server"
  | "benchmarks"
  | "logs"
  | "settings";

export const PAGES: { id: PageId; label: string; shortcut: string }[] = [
  { id: "home", label: "Home", shortcut: "⌘1" },
  { id: "models", label: "Models", shortcut: "⌘2" },
  { id: "library", label: "Library", shortcut: "⌘3" },
  { id: "chat", label: "Chat", shortcut: "⌘4" },
  { id: "server", label: "Server", shortcut: "⌘5" },
  { id: "benchmarks", label: "Benchmarks", shortcut: "⌘6" },
  { id: "logs", label: "Logs", shortcut: "⌘7" },
  { id: "settings", label: "Settings", shortcut: "⌘8" },
];

export type ToastVariant = "default" | "success" | "error" | "warning";

export interface Toast {
  id: string;
  title: string;
  description?: string;
  variant: ToastVariant;
  /** Set just before removal so the exit animation can play. */
  leaving: boolean;
}

interface UiState {
  page: PageId;
  theme: ThemeMode;
  toasts: Toast[];
  setPage: (page: PageId) => void;
  setTheme: (theme: ThemeMode) => void;
  pushToast: (toast: Omit<Toast, "id" | "leaving">) => void;
  dismissToast: (id: string) => void;
  /** Stop a toast from disappearing while the pointer is on it. */
  holdToast: (id: string) => void;
  resumeToast: (id: string) => void;
}

/** How long a toast stays up before it starts leaving. */
const toastLifetime = (variant: ToastVariant): number => (variant === "error" ? 8000 : 4500);

/** Must outlast the exit animation in tailwind.config.ts, or the toast pops. */
const toastExitMs = 160;

let toastSequence = 0;

/** id -> the pending auto-dismiss timer and the time it was armed. */
const timers = new Map<string, { handle: number; remainingMs: number }>();

export const useUi = create<UiState>((set, get) => ({
  page: "home",
  theme: "light",
  toasts: [],
  setPage: (page) => set({ page }),
  setTheme: (theme) => set({ theme }),
  pushToast: (toast) => {
    toastSequence += 1;
    const id = `toast-${toastSequence}`;
    const duration = toastLifetime(toast.variant);
    set((state) => {
      const next = [...state.toasts, { ...toast, id, leaving: false }];
      // Anything pushed out of the window must not keep a timer alive.
      for (const dropped of next.slice(0, Math.max(0, next.length - 4))) {
        const pending = timers.get(dropped.id);
        if (pending) {
          window.clearTimeout(pending.handle);
          timers.delete(dropped.id);
        }
      }
      return { toasts: next.slice(-4) };
    });
    timers.set(id, {
      handle: window.setTimeout(() => get().dismissToast(id), duration),
      remainingMs: duration,
    });
  },
  dismissToast: (id) => {
    const pending = timers.get(id);
    if (pending) {
      window.clearTimeout(pending.handle);
      timers.delete(id);
    }
    set((state) => ({
      toasts: state.toasts.map((entry) => (entry.id === id ? { ...entry, leaving: true } : entry)),
    }));
    window.setTimeout(() => {
      set((state) => ({ toasts: state.toasts.filter((entry) => entry.id !== id) }));
    }, toastExitMs);
  },
  holdToast: (id) => {
    const pending = timers.get(id);
    if (!pending) {
      return;
    }
    window.clearTimeout(pending.handle);
    // Record roughly what is left so resuming does not restart the whole clock.
    timers.set(id, { handle: 0, remainingMs: Math.min(pending.remainingMs, 1800) });
  },
  resumeToast: (id) => {
    const pending = timers.get(id);
    if (!pending || pending.handle !== 0) {
      return;
    }
    timers.set(id, {
      handle: window.setTimeout(() => get().dismissToast(id), pending.remainingMs),
      remainingMs: pending.remainingMs,
    });
  },
}));

/** Fire a toast from anywhere without importing the store's setter. */
export function toast(input: {
  title: string;
  description?: string;
  variant?: ToastVariant;
}): void {
  useUi.getState().pushToast({ variant: "default", ...input });
}

/** Resolve `system` against the OS preference. */
export function prefersDark(theme: ThemeMode): boolean {
  if (theme === "dark") {
    return true;
  }
  if (theme === "light") {
    return false;
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}
