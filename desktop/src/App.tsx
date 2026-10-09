import { useEffect } from "react";
import { QueryClientProvider } from "@tanstack/react-query";

import { queryClient } from "@/lib/query-client";
import { BenchmarksPage } from "@/pages/benchmarks";
import { ChatPage } from "@/pages/chat";
import { HomePage } from "@/pages/home";
import { LibraryPage } from "@/pages/library";
import { LogsPage } from "@/pages/logs";
import { ModelsPage } from "@/pages/models";
import { ServerPage } from "@/pages/server";
import { SettingsPage } from "@/pages/settings";
import { EventBridge } from "@/components/event-bridge";
import { DropZone } from "@/components/drop-zone";
import { Sidebar } from "@/components/sidebar";
import { TopBar } from "@/components/top-bar";
import { Toaster } from "@/components/ui/toaster";
import { PAGES, prefersDark, useUi } from "@/stores/ui";

import type { PageId } from "@/stores/ui";

export function App() {
  const page = useUi((state) => state.page);
  const theme = useUi((state) => state.theme);

  useEffect(() => {
    const root = document.documentElement;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = prefersDark(theme);
      root.classList.toggle("dark", dark);
      // Lets native controls (scrollbars, form fields) pick the matching palette.
      root.style.colorScheme = dark ? "dark" : "light";
    };
    apply();
    if (theme !== "system") {
      return;
    }
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) {
        return;
      }
      const index = Number(event.key) - 1;
      if (!Number.isInteger(index) || index < 0 || index >= PAGES.length) {
        return;
      }
      event.preventDefault();
      useUi.getState().setPage(PAGES[index].id);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <QueryClientProvider client={queryClient}>
      <EventBridge />
      <DropZone />
      <div className="flex h-full overflow-hidden">
        <Sidebar />
        <div className="flex min-w-0 flex-1 flex-col">
          <TopBar />
          <main
            className={
              page === "chat"
                ? "min-h-0 flex-1 overflow-hidden"
                : "min-h-0 flex-1 overflow-y-auto"
            }
          >
            {renderPage(page)}
          </main>
        </div>
      </div>
      <Toaster />
    </QueryClientProvider>
  );
}

function renderPage(page: PageId) {
  switch (page) {
    case "models":
      return <ModelsPage />;
    case "library":
      return <LibraryPage />;
    case "chat":
      return <ChatPage />;
    case "server":
      return <ServerPage />;
    case "benchmarks":
      return <BenchmarksPage />;
    case "logs":
      return <LogsPage />;
    case "settings":
      return <SettingsPage />;
    default:
      return <HomePage />;
  }
}
