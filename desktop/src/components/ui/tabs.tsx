import { cn } from "@/lib/utils";

import type { ComponentType, ReactNode } from "react";

export interface TabItem<T extends string> {
  id: T;
  label: string;
  icon?: ComponentType<{ className?: string }>;
}

interface TabsProps<T extends string> {
  value: T;
  onChange: (id: T) => void;
  items: TabItem<T>[];
  /** Optional panel content; omit it and render the panels yourself. */
  children?: ReactNode;
  className?: string;
}

/** A segmented control. Radix Tabs is not a dependency, and this is all we need. */
export function Tabs<T extends string>({
  value,
  onChange,
  items,
  children,
  className,
}: TabsProps<T>) {
  return (
    <div className={cn("space-y-4", className)}>
      <div
        role="tablist"
        className="inline-flex items-center gap-1 rounded-lg border bg-secondary/40 p-1"
      >
        {items.map((item) => {
          const Icon = item.icon;
          const active = item.id === value;
          return (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => onChange(item.id)}
              className={cn(
                "inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                active
                  ? "bg-card text-foreground shadow-soft"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {Icon ? <Icon className="size-3.5" /> : null}
              {item.label}
            </button>
          );
        })}
      </div>
      {children ? <div role="tabpanel">{children}</div> : null}
    </div>
  );
}
