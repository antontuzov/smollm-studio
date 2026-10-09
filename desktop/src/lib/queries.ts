/**
 * TanStack Query wiring.
 *
 * Read-only commands become queries here so every page shares one cache and one
 * set of keys; commands that change state are called directly from handlers.
 */

import { useQuery } from "@tanstack/react-query";

import { api } from "./api";
import type { CatalogEntry, CatalogFilterInput, HardwareReport } from "./types";

export const queryKeys = {
  hardware: ["hardware"] as const,
  appInfo: ["app-info"] as const,
  doctor: ["doctor"] as const,
  catalog: (filters: CatalogFilterInput) =>
    ["catalog", filters] as const,
  catalogFacets: ["catalog-facets"] as const,
  localModels: ["local-models"] as const,
  downloads: ["downloads"] as const,
  engineMetrics: ["engine-metrics"] as const,
  sessions: ["chat-sessions"] as const,
  serverStatus: ["server-status"] as const,
  serverExamples: (modelId: string) => ["server-examples", modelId] as const,
  logs: ["logs"] as const,
  settings: ["settings"] as const,
  hfToken: ["hf-token"] as const,
  presets: ["presets"] as const,
};

/** Hardware barely changes while the window is open, so it is cached hard. */
export function useHardware() {
  return useQuery({
    queryKey: queryKeys.hardware,
    queryFn: api.detectHardware,
    staleTime: 5 * 60 * 1000,
  });
}

export function useAppInfo() {
  return useQuery({
    queryKey: queryKeys.appInfo,
    queryFn: api.getAppInfo,
    staleTime: Infinity,
  });
}

export function useDoctor() {
  return useQuery({ queryKey: queryKeys.doctor, queryFn: api.getDoctorReport, staleTime: 60_000 });
}

export function useCatalog(input: CatalogFilterInput) {
  return useQuery({
    // The whole filter object is the key, so any change refetches.
    queryKey: queryKeys.catalog(input),
    queryFn: () => api.listCatalogModels(input),
    staleTime: 30_000,
  });
}

/** Filter option lists; they only change when the catalog or an overlay does. */
export function useCatalogFacets() {
  return useQuery({
    queryKey: queryKeys.catalogFacets,
    queryFn: api.catalogFacets,
    staleTime: Infinity,
  });
}

export function useLocalModels() {
  return useQuery({ queryKey: queryKeys.localModels, queryFn: api.listLocalModels });
}

export function useDownloadSnapshot() {
  return useQuery({ queryKey: queryKeys.downloads, queryFn: api.downloadSnapshot });
}

export function useEngineMetrics() {
  return useQuery({ queryKey: queryKeys.engineMetrics, queryFn: api.engineMetrics });
}

export function useServerStatus() {
  return useQuery({
    queryKey: queryKeys.serverStatus,
    queryFn: api.serverStatus,
    // Poll only while it is actually listening: a stopped server costs nothing.
    refetchInterval: (query) => (query.state.data?.running ? 2000 : false),
  });
}

export function useServerExamples(modelId: string) {
  return useQuery({
    queryKey: queryKeys.serverExamples(modelId),
    // No model chosen is a valid state: the snippets say `"model": "default"`,
    // which the server resolves to whatever is resident, and /health never
    // needs one.
    queryFn: () => api.serverExamples(modelId.length > 0 ? modelId : undefined),
  });
}

export function useSettingsQuery() {
  return useQuery({ queryKey: queryKeys.settings, queryFn: api.getSettings, staleTime: Infinity });
}

export function usePresets() {
  return useQuery({ queryKey: queryKeys.presets, queryFn: api.getPresets, staleTime: Infinity });
}

/**
 * Which Hugging Face token the engine would send.
 *
 * Reading it asks the OS credential store, so the answer is cached until the app
 * changes it: saving and removing both replace this entry with what Rust read
 * back.
 */
export function useHfTokenStatus() {
  return useQuery({ queryKey: queryKeys.hfToken, queryFn: api.hfTokenStatus, staleTime: Infinity });
}

/** Saved conversations, newest edit first. */
export function useSessions() {
  return useQuery({ queryKey: queryKeys.sessions, queryFn: api.listChatSessions });
}

/** Full-text matches over those conversations. */
export function useSessionSearch(query: string) {
  const trimmed = query.trim();
  return useQuery({
    // Under the sessions key, so one invalidation refreshes both views.
    queryKey: [...queryKeys.sessions, "search", trimmed],
    queryFn: () => api.searchChatSessions(trimmed),
    enabled: trimmed.length > 0,
  });
}

/** The subset of catalog entries for a list of ids, resolved client-side. */
export function pickEntries(entries: CatalogEntry[], ids: string[]): CatalogEntry[] {
  return ids
    .map((id) => entries.find((entry) => entry.id === id))
    .filter((entry): entry is CatalogEntry => entry !== undefined);
}

export function hardwareSummary(hardware: HardwareReport): string {
  return `${hardware.cpuBrand || hardware.platform} · ${hardware.logicalCores} cores · ${hardware.totalRamGb.toFixed(0)} GB RAM`;
}
