/**
 * Command helpers that mutate state.
 *
 * Pages call these instead of `api.*` directly so a failure always produces the
 * same honest toast and the shared stores stay in step with Rust.
 */

import { api } from "./api";
import { describeError } from "./format";
import { queryClient } from "./query-client";
import { queryKeys } from "./queries";
import { useEngine, useDownloads } from "@/stores/engine";
import { toast } from "@/stores/ui";

import { CommandError } from "./types";

import type { DownloadTask, LoadModelOptions, ServerConfig, ServerStatus } from "./types";

/** One toast shape for every failed command helper in `lib/`. */
export function reportFailure(title: string, error: unknown): void {
  const message = describeError(error);
  const detail = error instanceof CommandError ? error.detail : undefined;
  toast({
    title,
    description: detail ? `${message} — ${detail}` : message,
    variant: "error",
  });
}

export async function pullModel(modelId: string): Promise<DownloadTask | null> {
  try {
    const task = await api.pullModel(modelId);
    useDownloads.getState().applyTask(task);
    toast({ title: "Download started", description: `${task.displayName} · ${task.fileName}` });
    return task;
  } catch (error) {
    reportFailure("Could not start the download", error);
    return null;
  }
}

export async function cancelDownload(downloadId: string): Promise<void> {
  try {
    await api.cancelDownload(downloadId);
    await queryClient.invalidateQueries({ queryKey: queryKeys.downloads });
  } catch (error) {
    reportFailure("Could not cancel the download", error);
  }
}

export async function retryDownload(downloadId: string): Promise<void> {
  try {
    const task = await api.retryDownload(downloadId);
    useDownloads.getState().applyTask(task);
  } catch (error) {
    reportFailure("Could not resume the download", error);
  }
}

export async function loadModel(
  modelId: string,
  options?: LoadModelOptions,
): Promise<boolean> {
  useEngine.getState().setBusy(true);
  try {
    const response = await api.loadModel(modelId, options);
    useEngine.getState().applyLoad(response);
    await queryClient.invalidateQueries({ queryKey: queryKeys.engineMetrics });
    toast({
      title: `Loaded ${response.handle.displayName}`,
      description: response.simulated
        ? "The mock engine is answering: output is simulated, not real model weights."
        : `${response.engine} · ${response.handle.contextLength} token context`,
      variant: response.warnings.length > 0 ? "warning" : "success",
    });
    for (const warning of response.warnings) {
      toast({ title: "Load warning", description: warning, variant: "warning" });
    }
    return true;
  } catch (error) {
    useEngine.getState().setBusy(false);
    reportFailure("Could not load the model", error);
    return false;
  }
}

export async function unloadModel(): Promise<void> {
  useEngine.getState().setBusy(true);
  try {
    await api.unloadModel();
    useEngine.getState().reset();
    await queryClient.invalidateQueries({ queryKey: queryKeys.engineMetrics });
    toast({ title: "Model unloaded", description: "Memory released." });
  } catch (error) {
    useEngine.getState().setBusy(false);
    reportFailure("Could not unload the model", error);
  }
}

export async function deleteLocalModel(fileName: string): Promise<boolean> {
  try {
    const path = await api.deleteLocalModel(fileName);
    await queryClient.invalidateQueries({ queryKey: queryKeys.localModels });
    await queryClient.invalidateQueries({ queryKey: ["catalog"] });
    toast({ title: "File removed", description: path });
    return true;
  } catch (error) {
    reportFailure("Could not delete the file", error);
    return false;
  }
}

export async function startServer(config?: ServerConfig): Promise<ServerStatus | null> {
  try {
    const status = await api.startServer(config);
    await queryClient.invalidateQueries({ queryKey: queryKeys.serverStatus });
    toast({
      title: "Server listening",
      description: status.simulated
        ? `${status.baseUrl} — answers are simulated by the mock engine.`
        : `${status.baseUrl} · loopback only`,
      variant: "success",
    });
    return status;
  } catch (error) {
    reportFailure("Could not start the server", error);
    return null;
  }
}

export async function stopServer(): Promise<void> {
  try {
    await api.stopServer();
    await queryClient.invalidateQueries({ queryKey: queryKeys.serverStatus });
    toast({ title: "Server stopped" });
  } catch (error) {
    reportFailure("Could not stop the server", error);
  }
}

export async function openFolder(kind: "models" | "logs"): Promise<void> {
  try {
    const path = kind === "models" ? await api.openModelFolder() : await api.openLogFolder();
    toast({ title: "Folder opened", description: path });
  } catch (error) {
    reportFailure("Could not open the folder", error);
  }
}

export async function exportDiagnostics(): Promise<void> {
  try {
    const path = await api.exportDiagnostics();
    toast({ title: "Diagnostics written", description: path });
  } catch (error) {
    reportFailure("Could not write the diagnostics file", error);
  }
}
