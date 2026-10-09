/**
 * A model file dropped anywhere on the window is an import.
 *
 * `dragDropEnabled` hands the webview real filesystem paths instead of letting it
 * navigate to a dropped file, which is the only safe behaviour for a desktop app:
 * a stray archive would otherwise replace the UI with a Finder view.
 */

import { useEffect } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { PackageOpen } from "lucide-react";

import { importModelFiles, isModelFile } from "@/lib/imports";
import { useImports } from "@/stores/imports";

import type { UnlistenFn } from "@tauri-apps/api/event";

export function DropZone() {
  const dragging = useImports((state) => state.dragging);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let disposed = false;
    const setDragging = useImports.getState().setDragging;

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        switch (event.payload.type) {
          case "enter":
            setDragging(event.payload.paths.some(isModelFile));
            break;
          case "drop":
            setDragging(false);
            void importModelFiles(event.payload.paths);
            break;
          case "leave":
            setDragging(false);
            break;
          case "over":
            break;
        }
      })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      // Without the listener the overlay never appears and a drop is ignored by
      // the OS. The Library page's import button still works, so this is not
      // worth a toast on every launch.
      .catch(() => undefined);

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  if (!dragging) {
    return null;
  }

  return (
    <div className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-3 rounded-2xl border-2 border-dashed border-primary/60 bg-background/70 backdrop-blur-[1px]" />
      <div className="relative flex items-center gap-3 rounded-xl border bg-card px-5 py-4 shadow-lg">
        <PackageOpen className="size-5 shrink-0 text-primary" />
        <div className="space-y-0.5">
          <p className="text-sm font-semibold">Drop to add it to the library</p>
          <p className="text-xs text-muted-foreground">
            The file is copied into the model folder; the original stays where it is.
          </p>
        </div>
      </div>
    </div>
  );
}
