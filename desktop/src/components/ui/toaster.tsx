import { CircleCheck, CircleX, Info, TriangleAlert, X } from "lucide-react";

import { useUi } from "@/stores/ui";
import { cn } from "@/lib/utils";

import type { ComponentType } from "react";
import type { ToastVariant } from "@/stores/ui";

const variantIcon: Record<ToastVariant, ComponentType<{ className?: string }>> = {
  default: Info,
  success: CircleCheck,
  error: CircleX,
  warning: TriangleAlert,
};

const variantClass: Record<ToastVariant, string> = {
  default: "text-foreground",
  success: "border-success/40",
  error: "border-destructive/40",
  warning: "border-amber-500/40",
};

const iconClass: Record<ToastVariant, string> = {
  default: "text-accent",
  success: "text-success",
  error: "text-destructive",
  warning: "text-amber-500",
};

/** The toast viewport. Toasts are pushed with `toast()` from the UI store. */
export function Toaster() {
  const toasts = useUi((state) => state.toasts);
  const dismiss = useUi((state) => state.dismissToast);
  const hold = useUi((state) => state.holdToast);
  const resume = useUi((state) => state.resumeToast);

  if (toasts.length === 0) {
    return null;
  }
  return (
    <div
      aria-label="Notifications"
      className="pointer-events-none fixed bottom-4 right-4 z-50 flex w-80 flex-col gap-2"
    >
      {toasts.map((entry) => {
        const Icon = variantIcon[entry.variant];
        // An error deserves to interrupt; a confirmation should not.
        const urgent = entry.variant === "error" || entry.variant === "warning";
        return (
          <div
            key={entry.id}
            role={urgent ? "alert" : "status"}
            onMouseEnter={() => hold(entry.id)}
            onMouseLeave={() => resume(entry.id)}
            onFocus={() => hold(entry.id)}
            onBlur={() => resume(entry.id)}
            className={cn(
              "panel pointer-events-auto flex gap-3 px-4 py-3 shadow-panel",
              entry.leaving ? "animate-toast-out" : "animate-toast-in",
              variantClass[entry.variant],
            )}
          >
            <Icon className={cn("mt-0.5 size-4 shrink-0", iconClass[entry.variant])} />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium leading-snug tracking-tight">{entry.title}</p>
              {entry.description ? (
                <p className="mt-0.5 break-words text-xs leading-relaxed text-muted-foreground">
                  {entry.description}
                </p>
              ) : null}
            </div>
            <button
              type="button"
              aria-label="Dismiss notification"
              className="-mr-1 -mt-1 self-start rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
              onClick={() => dismiss(entry.id)}
            >
              <X className="size-3.5" />
            </button>
          </div>
        );
      })}
    </div>
  );
}
