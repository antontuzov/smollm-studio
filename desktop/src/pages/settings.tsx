import { useEffect, useState } from "react";
import { FolderOpen, Save, Settings2, Trash2 } from "lucide-react";

import { api } from "@/lib/api";
import { exportDiagnostics, openFolder } from "@/lib/actions";
import { describeError } from "@/lib/format";
import { queryClient } from "@/lib/query-client";
import {
  queryKeys,
  useAppInfo,
  useCatalog,
  usePresets,
  useSettingsQuery,
} from "@/lib/queries";
import { useUi } from "@/stores/ui";
import { PageHeader } from "@/components/page-header";
import { SamplingPanel } from "@/components/sampling-panel";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/dialog";
import { Input, NumberField, Select, Switch } from "@/components/ui/field";
import { ErrorState, Note, SkeletonList } from "@/components/ui/feedback";

import type { Backend, ResetOutcome, Settings, ThemeMode } from "@/lib/types";

const themeOptions = [
  { value: "dark", label: "Dark" },
  { value: "light", label: "Light" },
  { value: "system", label: "Follow the system" },
];

const backendOptions: { value: Backend; label: string }[] = [
  { value: "cpu", label: "CPU" },
  { value: "metal", label: "Metal (Apple Silicon)" },
  { value: "cuda", label: "CUDA (NVIDIA)" },
  { value: "vulkan", label: "Vulkan" },
  { value: "mock", label: "Mock engine (simulated)" },
];

export function SettingsPage() {
  const settings = useSettingsQuery();
  const appInfo = useAppInfo();
  const presets = usePresets();
  const catalog = useCatalog({ query: "", sort: "recommended", hidePlaceholders: true });
  const setTheme = useUi((state) => state.setTheme);

  const [draft, setDraft] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [showReset, setShowReset] = useState(false);
  const [resetResult, setResetResult] = useState<ResetOutcome | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    if (settings.data && draft === null) {
      setDraft(settings.data);
      setTheme(settings.data.theme);
    }
  }, [settings.data, draft, setTheme]);

  if (settings.isPending) {
    return (
      <div className="space-y-5 p-6">
        <PageHeader title="Settings" icon={Settings2} />
        <SkeletonList rows={5} />
      </div>
    );
  }

  if (!draft) {
    return (
      <div className="p-6">
        <ErrorState
          message="Settings could not be read"
          detail={describeError(settings.error)}
          onRetry={() => void settings.refetch()}
        />
      </div>
    );
  }

  const patch = (changes: Partial<Settings>) => setDraft({ ...draft, ...changes });
  const downloaded = (catalog.data ?? []).filter((model) => model.downloaded);
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings.data ?? draft);

  const save = async () => {
    setSaving(true);
    setFailure(null);
    try {
      const saved = await api.saveSettings(draft);
      setDraft(saved);
      setTheme(saved.theme);
      await queryClient.invalidateQueries({ queryKey: queryKeys.settings });
      await queryClient.invalidateQueries({ queryKey: queryKeys.appInfo });
      await queryClient.invalidateQueries({ queryKey: queryKeys.engineMetrics });
      // A new model directory changes what the library and the catalog can see.
      await queryClient.invalidateQueries({ queryKey: queryKeys.localModels });
      await queryClient.invalidateQueries({ queryKey: ["catalog"] });
    } catch (error) {
      setFailure(describeError(error));
    } finally {
      setSaving(false);
    }
  };

  const reset = async () => {
    setResetting(true);
    setFailure(null);
    try {
      const outcome = await api.resetAppData();
      setResetResult(outcome);
      await queryClient.invalidateQueries();
      // Re-seed the draft from the defaults Rust just wrote back.
      setDraft(null);
    } catch (error) {
      setFailure(describeError(error));
    } finally {
      setResetting(false);
      setShowReset(false);
    }
  };

  return (
    <>
      <PageHeader
        title="Settings"
        description="Everything is stored as plain JSON in the data folder and applied when you press Save. Nothing is sent anywhere."
        icon={Settings2}
        actions={
          <>
            {dirty ? <Badge tone="warning">unsaved changes</Badge> : null}
            <Button
              variant="outline"
              onClick={() => settings.data && setDraft(settings.data)}
              disabled={!dirty || saving}
            >
              Discard
            </Button>
            <Button onClick={() => void save()} disabled={!dirty || saving}>
              <Save />
              {saving ? "Saving…" : "Save settings"}
            </Button>
          </>
        }
      />

      <div className="grid gap-5 p-6 xl:grid-cols-2">
        {failure ? (
          <div className="xl:col-span-2">
            <ErrorState message="The change was not saved" detail={failure} />
          </div>
        ) : null}

        <Card>
          <CardHeader
            title="Appearance"
            description="Light is how the app opens; dark is a fully tuned alternate."
          />
          <CardContent>
            <Select
              label="Theme"
              value={draft.theme}
              options={themeOptions}
              onChange={(theme) => {
                patch({ theme: theme as ThemeMode });
                setTheme(theme as ThemeMode);
              }}
              hint="Applies immediately, and is remembered when you save."
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader
            title="Models and engine"
            description="Defaults for loading, used when a request does not override them."
          />
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <label htmlFor="model-dir" className="text-xs font-medium text-muted-foreground">
                Model folder
              </label>
              <div className="flex gap-2">
                <Input
                  id="model-dir"
                  className="font-mono text-xs"
                  value={draft.modelDir ?? ""}
                  placeholder={appInfo.data?.modelsDir ?? ""}
                  onChange={(event) => patch({ modelDir: event.target.value })}
                />
                <Button variant="outline" onClick={() => void openFolder("models")}>
                  <FolderOpen />
                  Show
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                Leave empty for the default location. Saving points downloads, the library scan and
                resume bookkeeping at the new folder; existing files are not copied.
              </p>
            </div>

            <Select
              label="Default model"
              value={draft.defaultModelId ?? ""}
              options={[
                { value: "", label: "No default" },
                ...downloaded.map((model) => ({ value: model.id, label: model.displayName })),
              ]}
              onChange={(defaultModelId) => patch({ defaultModelId })}
              hint="Used by the local server when a request does not name a model."
            />

            <div className="grid gap-4 sm:grid-cols-2">
              <NumberField
                label="Context length"
                value={draft.defaultContextLength}
                min={512}
                max={32_768}
                step={512}
                suffix="tokens"
                onChange={(defaultContextLength) => patch({ defaultContextLength })}
              />
              <NumberField
                label="GPU layers"
                value={draft.defaultGpuLayers}
                min={-1}
                max={99}
                onChange={(defaultGpuLayers) => patch({ defaultGpuLayers })}
                hint="-1 offloads everything the backend supports."
              />
            </div>

            <Select
              label="Backend"
              value={draft.defaultBackend}
              options={backendOptions.map((option) => ({
                value: option.value,
                label: option.label,
              }))}
              onChange={(backend) => patch({ defaultBackend: backend as Backend })}
              hint={
                appInfo.data?.simulatedEngine
                  ? "This build only contains the mock engine and the GGUF metadata reader; native backends need the llama-cpp feature flag."
                  : "The engine falls back to something usable and tells you when it does."
              }
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader
            title="Chat defaults"
            description="Preset and sampling used for a new transcript."
          />
          <CardContent>
            <SamplingPanel
              presets={presets.data ?? []}
              preset={draft.chatPreset}
              params={draft.sampling}
              onPreset={(chatPreset, sampling) => patch({ chatPreset, sampling })}
              onParams={(sampling) => patch({ sampling })}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader title="Local server" description="Used when the Server page opens." />
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <label htmlFor="server-host" className="text-xs font-medium text-muted-foreground">
                Host
              </label>
              <Input
                id="server-host"
                className="font-mono text-xs"
                value={draft.serverHost}
                onChange={(event) => patch({ serverHost: event.target.value })}
              />
              <p className="text-xs text-muted-foreground">
                Loopback only: the backend refuses other bind addresses.
              </p>
            </div>
            <NumberField
              label="Port"
              value={draft.serverPort}
              min={1}
              max={65_535}
              onChange={(serverPort) => patch({ serverPort })}
            />
          </CardContent>
        </Card>

        <Card className="xl:col-span-2">
          <CardHeader title="Data and privacy" description="Where things live, and how to get them out." />
          <CardContent className="space-y-4">
            <Switch
              checked={draft.autoUpdateChecks}
              onCheckedChange={(autoUpdateChecks) => patch({ autoUpdateChecks })}
              label="Check for a newer release on launch (not in this build)"
            />
            <div className="grid gap-3 lg:grid-cols-2">
              <Note tone="neutral">
                <p>
                  This switch is stored in your settings, but nothing acts on it yet: this build
                  makes no update request at all. When the updater lands it will ask GitHub for the
                  latest release number at most once a day, with no identifiers and no usage data
                  leaving the machine. The catalog and the files you download come from Hugging Face,
                  and only when you press Download.
                </p>
              </Note>
              {resetResult ? (
                <Note tone="info">
                  <p className="font-medium">Reset finished</p>
                  <p>{resetResult.note}</p>
                  <p>
                    {resetResult.removedPartFiles} partial file(s) removed,{" "}
                    {resetResult.keptModels} model file(s) kept in {resetResult.info.modelsDir}.
                  </p>
                </Note>
              ) : null}
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => void openFolder("models")}>
                <FolderOpen />
                Model folder
              </Button>
              <Button variant="outline" size="sm" onClick={() => void openFolder("logs")}>
                <FolderOpen />
                Log folder
              </Button>
              <Button variant="outline" size="sm" onClick={() => void exportDiagnostics()}>
                <Save />
                Export diagnostics
              </Button>
              <Button variant="destructive" size="sm" onClick={() => setShowReset(true)}>
                <Trash2 />
                Reset app data
              </Button>
            </div>
          </CardContent>
          <CardFooter>
            <span className="truncate font-mono text-[11px] text-muted-foreground">
              {appInfo.data?.dataDir ?? "data folder"}
            </span>
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {appInfo.data?.platform ?? ""} {appInfo.data?.arch ?? ""} · v
              {appInfo.data?.version ?? ""}
            </span>
          </CardFooter>
        </Card>
      </div>

      <ConfirmDialog
        open={showReset}
        onOpenChange={setShowReset}
        title="Reset app data?"
        description="Settings return to their defaults and the in-memory log buffer is cleared. Model files are kept — nothing you downloaded is thrown away."
        confirmLabel="Reset settings"
        destructive
        pending={resetting}
        onConfirm={() => void reset()}
      />
    </>
  );
}
