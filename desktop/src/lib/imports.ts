/**
 * Bringing in a model the user already owns.
 *
 * A .gguf sitting somewhere else on disk is unusable: the engine loads from the
 * folder the settings name, and the Library page lists what is inside it. So an
 * import is a copy into that folder, and Rust validates the header before and
 * after the bytes move. Both doors — the open panel and a file dropped on the
 * window — arrive here, because they need the same refresh and the same honesty
 * about what was refused.
 */

import { open } from "@tauri-apps/plugin-dialog";

import { reportFailure } from "./actions";
import { api } from "./api";
import { formatBytes } from "./format";
import { queryClient } from "./query-client";
import { queryKeys } from "./queries";
import { useImports } from "@/stores/imports";
import { toast } from "@/stores/ui";

import type { LocalModel } from "./types";

/**
 * Ask the OS for model files.
 *
 * A closed panel is not a failure, so it yields no paths and no toast; only a
 * panel the platform refused to open is reported.
 */
export async function pickModelFiles(): Promise<string[]> {
  try {
    const chosen = await open({
      title: "Import a GGUF model",
      multiple: true,
      filters: [{ name: "GGUF model", extensions: ["gguf"] }],
    });
    if (chosen === null) {
      return [];
    }
    return Array.isArray(chosen) ? chosen : [chosen];
  } catch (error) {
    reportFailure("Could not open the file panel", error);
    return [];
  }
}

export function isModelFile(path: string): boolean {
  return /\.gguf$/i.test(path);
}

/**
 * Copy each model into the library, one at a time.
 *
 * Sequential on purpose: every import is a large local copy, and starting them
 * together only makes the disk thrash while the progress list lies about what is
 * actually running.
 */
export async function importModelFiles(paths: string[]): Promise<LocalModel[]> {
  const models = paths.filter(isModelFile);
  const refused = paths.length - models.length;
  if (refused > 0) {
    toast({
      title: "Only .gguf files are models",
      description: `${refused} dropped file(s) were ignored.`,
      variant: "warning",
    });
  }

  const imported: LocalModel[] = [];
  for (const path of models) {
    const fileName = baseName(path);
    useImports.getState().start(fileName);
    try {
      imported.push(await api.importModel(path));
    } catch (error) {
      reportFailure(`Could not import ${fileName}`, error);
    } finally {
      useImports.getState().finish(fileName);
    }
  }

  if (imported.length > 0) {
    await queryClient.invalidateQueries({ queryKey: queryKeys.localModels });
    // `downloaded` is part of every catalog entry, so the list is stale now.
    await queryClient.invalidateQueries({ queryKey: ["catalog"] });
    toast({
      title: imported.length === 1 ? "Model imported" : `${imported.length} models imported`,
      description: imported
        .slice(0, 3)
        .map((model) => `${model.fileName} · ${formatBytes(model.sizeBytes)}`)
        .join(", "),
      variant: "success",
    });
  }
  return imported;
}

/** The last path segment, on either separator. */
function baseName(path: string): string {
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return separator >= 0 ? path.slice(separator + 1) : path;
}
