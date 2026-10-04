import { Info, LoaderCircle, ShieldAlert, TriangleAlert } from "lucide-react";

import { Button } from "./button";
import { cn } from "@/lib/utils";

import type { ComponentType, ReactNode } from "react";

export function Skeleton({ className }: { className?: string }) {
  return <div className={cn("animate-pulse rounded-md bg-secondary/70", className)} />;
}

/** A stack of rows that stands in for a list while its query is loading. */
export function SkeletonList({ rows = 4, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn("space-y-3", className)}>
      {Array.from({ length: rows }, (_, index) => (
        <div key={index} className="flex items-center gap-4 rounded-lg border px-4 py-3">
          <Skeleton className="h-10 w-10 shrink-0 rounded-md" />
          <div className="flex-1 space-y-2">
            <Skeleton className="h-3 w-1/3" />
            <Skeleton className="h-3 w-2/3" />
          </div>
          <Skeleton className="h-7 w-20" />
        </div>
      ))}
    </div>
  );
}

interface ProgressProps {
  /** 0-100. An indeterminate bar is used when this is not a finite number. */
  value?: number;
  className?: string;
  tone?: "primary" | "accent" | "success";
}

const barToneClass: Record<NonNullable<ProgressProps["tone"]>, string> = {
  primary: "bg-primary",
  accent: "bg-accent",
  success: "bg-success",
};

export function Progress({ value, className, tone = "primary" }: ProgressProps) {
  const known = typeof value === "number" && Number.isFinite(value);
  return (
    <div
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={known ? Math.round(value ?? 0) : undefined}
      className={cn("h-1.5 w-full overflow-hidden rounded-full bg-secondary", className)}
    >
      {known ? (
        <div
          className={cn("h-full rounded-full transition-[width] duration-300", barToneClass[tone])}
          style={{ width: `${Math.min(100, Math.max(0, value ?? 0))}%` }}
        />
      ) : (
        <div className={cn("h-full w-1/3 animate-pulse rounded-full", barToneClass[tone])} />
      )}
    </div>
  );
}

interface EmptyStateProps {
  icon?: ComponentType<{ className?: string }>;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export function EmptyState({
  icon: Icon = Info,
  title,
  description,
  action,
  className,
}: EmptyStateProps) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed px-6 py-12 text-center",
        className,
      )}
    >
      <Icon className="size-6 text-muted-foreground" />
      <div className="space-y-1">
        <p className="text-sm font-medium">{title}</p>
        {description ? (
          <p className="mx-auto max-w-md text-xs leading-relaxed text-muted-foreground">
            {description}
          </p>
        ) : null}
      </div>
      {action}
    </div>
  );
}

interface ErrorStateProps {
  message: string;
  detail?: string;
  onRetry?: () => void;
  retryLabel?: string;
  className?: string;
}

export function ErrorState({
  message,
  detail,
  onRetry,
  retryLabel = "Try again",
  className,
}: ErrorStateProps) {
  return (
    <div
      className={cn(
        "flex items-start gap-3 rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3",
        className,
      )}
    >
      <ShieldAlert className="mt-0.5 size-4 shrink-0 text-destructive" />
      <div className="min-w-0 flex-1 space-y-1">
        <p className="text-sm font-medium text-destructive">{message}</p>
        {detail ? (
          <p className="break-words font-mono text-xs text-muted-foreground">{detail}</p>
        ) : null}
      </div>
      {onRetry ? (
        <Button size="sm" variant="outline" onClick={onRetry}>
          {retryLabel}
        </Button>
      ) : null}
    </div>
  );
}

type NoteTone = "info" | "warning" | "danger" | "neutral";

const noteToneClass: Record<NoteTone, string> = {
  info: "border-accent/30 bg-accent/10 text-accent",
  warning: "border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300",
  danger: "border-destructive/30 bg-destructive/10 text-destructive",
  neutral: "border bg-secondary/50 text-muted-foreground",
};

const noteIcon: Record<NoteTone, ComponentType<{ className?: string }>> = {
  info: Info,
  warning: TriangleAlert,
  danger: ShieldAlert,
  neutral: Info,
};

/** A short, honest banner: the place where "this is simulated" belongs. */
export function Note({
  tone = "info",
  icon: Icon = noteIcon[tone],
  children,
  className,
}: {
  tone?: NoteTone;
  icon?: ComponentType<{ className?: string }>;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex items-start gap-2.5 rounded-lg border px-3.5 py-2.5 text-xs leading-relaxed",
        noteToneClass[tone],
        className,
      )}
    >
      <Icon className="mt-0.5 size-4 shrink-0" />
      <div className="min-w-0 flex-1 space-y-1">{children}</div>
    </div>
  );
}

export function Busy({ label }: { label: string }) {
  return (
    <span className="inline-flex items-center gap-2 text-xs text-muted-foreground">
      <LoaderCircle className="size-3.5 animate-spin" />
      {label}
    </span>
  );
}
