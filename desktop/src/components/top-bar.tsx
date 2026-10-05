import { CircleStop, MessageSquare, Monitor, Moon, Server, Sun } from "lucide-react";

import { formatRate } from "@/lib/format";
import { useServerStatus } from "@/lib/queries";
import { engineLabel, engineTone } from "@/lib/presentation";
import { useEngine } from "@/stores/engine";
import { useUi } from "@/stores/ui";
import { Button } from "@/components/ui/button";
import { StatusPill } from "@/components/ui/badge";

import type { ComponentType } from "react";
import type { ThemeMode } from "@/lib/types";

const themeIcon: Record<ThemeMode, ComponentType<{ className?: string }>> = {
  light: Sun,
  dark: Moon,
  system: Monitor,
};

const themeOrder: ThemeMode[] = ["dark", "light", "system"];

function nextTheme(current: ThemeMode): ThemeMode {
  const index = themeOrder.indexOf(current);
  return themeOrder[(index + 1) % themeOrder.length];
}

export function TopBar() {
  const metrics = useEngine((state) => state.metrics);
  const handle = useEngine((state) => state.handle);
  const setPage = useUi((state) => state.setPage);
  const theme = useUi((state) => state.theme);
  const setTheme = useUi((state) => state.setTheme);
  const server = useServerStatus();
  const running = server.data?.running ?? false;

  const Icon = themeIcon[theme];

  return (
    <header className="flex items-center justify-between gap-3 border-b bg-card/30 px-6 py-2.5">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <StatusPill
          tone={metrics ? engineTone(metrics.simulated) : "neutral"}
          label={metrics ? engineLabel(metrics.engine, metrics.simulated) : "engine starting"}
        />
        <button
          type="button"
          onClick={() => setPage(handle ? "chat" : "library")}
          className="inline-flex min-w-0 items-center gap-2 rounded-full border bg-card/70 px-2.5 py-1 text-xs font-medium transition-colors hover:border-primary/40"
        >
          {handle ? (
            <>
              <MessageSquare className="size-3.5 shrink-0 text-primary" />
              <span className="truncate">{handle.displayName}</span>
              {metrics && metrics.tokensPerSecond > 0 ? (
                <span className="shrink-0 text-muted-foreground">
                  {formatRate(metrics.tokensPerSecond)} tok/s
                </span>
              ) : null}
            </>
          ) : (
            <>
              <CircleStop className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate text-muted-foreground">no model loaded</span>
            </>
          )}
        </button>
        <button
          type="button"
          onClick={() => setPage("server")}
          className="inline-flex items-center gap-2 rounded-full border bg-card/70 px-2.5 py-1 text-xs font-medium transition-colors hover:border-accent/40"
        >
          <Server className={running ? "size-3.5 text-success" : "size-3.5 text-muted-foreground"} />
          <span className={running ? "" : "text-muted-foreground"}>
            {running ? `${server.data?.host}:${server.data?.port}` : "server stopped"}
          </span>
        </button>
      </div>

      <div className="flex shrink-0 items-center gap-2">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Theme: ${theme}. Click to change.`}
          title={`Theme: ${theme} (click to cycle light, dark, system)`}
          onClick={() => setTheme(nextTheme(theme))}
        >
          {/* Keyed by theme so each swap gets its own entrance. */}
          <span key={theme} className="block animate-pop-in">
            <Icon />
          </span>
        </Button>
      </div>
    </header>
  );
}
