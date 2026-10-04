import { cn } from "@/lib/utils";

import type { ComponentType, ReactNode } from "react";

interface PageHeaderProps {
  title: string;
  description?: ReactNode;
  icon?: ComponentType<{ className?: string }>;
  actions?: ReactNode;
  className?: string;
}

export function PageHeader({ title, description, icon: Icon, actions, className }: PageHeaderProps) {
  return (
    <header
      className={cn(
        "flex flex-wrap items-start justify-between gap-4 border-b px-6 py-5",
        className,
      )}
    >
      <div className="flex min-w-0 items-start gap-3">
        {Icon ? (
          <span className="mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-lg border bg-secondary/50">
            <Icon className="size-4" />
          </span>
        ) : null}
        <div className="min-w-0 space-y-1">
          <h1 className="truncate text-lg font-semibold tracking-tight">{title}</h1>
          {description ? (
            <p className="max-w-2xl text-xs leading-relaxed text-muted-foreground">{description}</p>
          ) : null}
        </div>
      </div>
      {actions ? <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  );
}
