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
}

interface UiState {
  page: PageId;
  theme: ThemeMode;
  toasts: Toast[];
  setPage: (page: PageId) => void;
  setTheme: (theme: ThemeMode) => void;
  pushToast: (toast: Omit<Toast, "id">) => void;
  dismissToast: (id: string) => void;
}

let toastSequence = 0;

export const useUi = create<UiState>((set) => ({
  page: "home",
  theme: "light",
  toasts: [],
  setPage: (page) => set({ page }),
  setTheme: (theme) => set({ theme }),
  pushToast: (toast) => {
    toastSequence += 1;
    const id = `toast-${toastSequence}`;
    set((state) => ({ toasts: [...state.toasts, { ...toast, id }].slice(-4) }));
    // Toasts are transient by design; the Logs page keeps the permanent record.
    setTimeout(() => {
      set((state) => ({ toasts: state.toasts.filter((entry) => entry.id !== id) }));
    }, toast.variant === "error" ? 8000 : 4500);
  },
  dismissToast: (id) => set((state) => ({ toasts: state.toasts.filter((entry) => entry.id !== id) })),
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
