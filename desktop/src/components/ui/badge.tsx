import { cn } from "@/lib/utils";

import type { ReactNode } from "react";

export type BadgeTone = "neutral" | "info" | "success" | "warning" | "danger" | "primary";

const badgeToneClass: Record<BadgeTone, string> = {
  neutral: "bg-secondary text-secondary-foreground",
  info: "bg-accent/15 text-accent",
  success: "bg-success/15 text-success",
  warning: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  danger: "bg-destructive/15 text-destructive",
  primary: "bg-primary/15 text-primary",
};

const dotToneClass: Record<BadgeTone, string> = {
  neutral: "bg-muted-foreground",
  info: "bg-accent",
  success: "bg-success",
  warning: "bg-amber-500",
  danger: "bg-destructive",
  primary: "bg-primary",
};

interface BadgeProps {
  tone?: BadgeTone;
  className?: string;
  children: ReactNode;
}

export function Badge({ tone = "neutral", className, children }: BadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] font-medium",
        badgeToneClass[tone],
        className,
      )}
    >
      {children}
    </span>
  );
}

interface StatusPillProps {
  tone?: BadgeTone;
  label: ReactNode;
  /** Blink while the underlying thing is still working. */
  pulse?: boolean;
  className?: string;
}

export function StatusPill({ tone = "neutral", label, pulse = false, className }: StatusPillProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-2 rounded-full border bg-card/70 px-2.5 py-1 text-xs font-medium",
        className,
      )}
    >
      <span
        className={cn(
          "size-2 shrink-0 rounded-full",
          dotToneClass[tone],
          pulse && "animate-pulse-dot",
        )}
      />
      <span className="truncate">{label}</span>
    </span>
  );
}
