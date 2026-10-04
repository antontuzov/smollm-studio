import { cn } from "@/lib/utils";

import type { HTMLAttributes, ReactNode } from "react";

export function Card({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("panel flex flex-col", className)} {...props} />;
}

interface CardHeaderProps {
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  className?: string;
}

export function CardHeader({ title, description, actions, className }: CardHeaderProps) {
  return (
    <div
      className={cn(
        "flex items-start justify-between gap-4 border-b px-5 py-4",
        className,
      )}
    >
      <div className="min-w-0 space-y-1">
        <h3 className="truncate text-sm font-semibold leading-none tracking-tight">{title}</h3>
        {description ? (
          <p className="text-xs leading-relaxed text-muted-foreground">{description}</p>
        ) : null}
      </div>
      {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
    </div>
  );
}

export function CardContent({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("flex-1 px-5 py-4", className)} {...props} />;
}

export function CardFooter({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("flex items-center justify-between gap-3 border-t px-5 py-3", className)}
      {...props}
    />
  );
}

type StatTone = "default" | "accent" | "success" | "warning" | "danger";

const statToneClass: Record<StatTone, string> = {
  default: "text-foreground",
  accent: "text-accent",
  success: "text-success",
  warning: "text-amber-500 dark:text-amber-400",
  danger: "text-destructive",
};

interface StatProps {
  label: string;
  value: ReactNode;
  hint?: ReactNode;
  tone?: StatTone;
  className?: string;
}

export function Stat({ label, value, hint, tone = "default", className }: StatProps) {
  return (
    <div className={cn("rounded-lg border bg-card/60 px-4 py-3", className)}>
      <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
        {label}
      </p>
      <p className={cn("stat-value mt-1.5 break-words", statToneClass[tone])}>{value}</p>
      {hint ? <p className="mt-1 text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}
