import { useEffect, useState } from "react";
import { FolderInput, FolderOpen, KeyRound, Save, Settings2, Trash2 } from "lucide-react";

import { api } from "@/lib/api";
import { exportDiagnostics, openFolder } from "@/lib/actions";
import { describeError, formatBytes } from "@/lib/format";
import { describeMove, moveModelDir, pickModelDir, sameFolder } from "@/lib/model-dir";
import { queryClient } from "@/lib/query-client";
import {
  queryKeys,
  useAppInfo,
  useCatalog,
  useHfTokenStatus,
  useLocalModels,
  usePresets,
  useSettingsQuery,
} from "@/lib/queries";
import { useChat } from "@/stores/chat";
import { useEngine } from "@/stores/engine";
import { toast, useUi } from "@/stores/ui";
import { PageHeader } from "@/components/page-header";
import { SamplingPanel } from "@/components/sampling-panel";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/dialog";
import { FieldRow, Input, NumberField, Select, Switch } from "@/components/ui/field";
import { ErrorState, Note, SkeletonList } from "@/components/ui/feedback";

import type { Backend, Relocation, ResetOutcome, Settings, ThemeMode, TokenStatus } from "@/lib/types";

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

/** One line saying where the live token came from, or what is still missing. */
function tokenPlace(status: TokenStatus | undefined): string {
  if (!status) {
    return "Reading the credential store…";
  }
  if (!status.masked) {
    return "Gated models are refused until you save one.";
  }
  return status.source === "keychain"
    ? "kept in your OS credential store"
    : "taken from HF_TOKEN, which this app only reads";
}

export function SettingsPage() {
  const settings = useSettingsQuery();
  const appInfo = useAppInfo();
  const presets = usePresets();
  const token = useHfTokenStatus();
  const catalog = useCatalog({ query: "", sort: "recommended", hidePlaceholders: true });
  const library = useLocalModels();
  const handle = useEngine((state) => state.handle);
  const setTheme = useUi((state) => state.setTheme);

  const [draft, setDraft] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [showReset, setShowReset] = useState(false);
  const [resetResult, setResetResult] = useState<ResetOutcome | null>(null);
  const [pendingMove, setPendingMove] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  const [moveResult, setMoveResult] = useState<Relocation | null>(null);
  const [tokenDraft, setTokenDraft] = useState("");
  const [tokenBusy, setTokenBusy] = useState(false);
  const [tokenFailure, setTokenFailure] = useState<string | null>(null);
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
  const files = library.data ?? [];
  const libraryBytes = files.reduce((sum, model) => sum + model.sizeBytes, 0);
  const moveCount = library.isPending ? null : files.length;
  const status = token.data;
  // Unknown counts as unwritable only once the status has actually arrived.
  const writable = status?.keychain;
  const leftBehind = moveResult
    ? [...moveResult.duplicates, ...moveResult.conflicts, ...moveResult.failures]
    : [];

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
      await queryClient.invalidateQueries({ queryKey: ["catalog-facets"] });
    } catch (error) {
      setFailure(describeError(error));
    } finally {
      setSaving(false);
    }
  };

  const startMove = async () => {
    const chosen = await pickModelDir(appInfo.data?.modelsDir);
    if (!chosen) {
      return;
    }
    if (sameFolder(chosen, appInfo.data?.modelsDir)) {
      toast({ title: "That is already the model folder", variant: "default" });
      return;
    }
    setMoveResult(null);
    setPendingMove(chosen);
  };

  const confirmMove = async () => {
    const target = pendingMove;
    if (!target) {
      return;
    }
    setMoving(true);
    const report = await moveModelDir(target);
    setMoving(false);
    setPendingMove(null);
    if (report) {
      setMoveResult(report);
      // The draft has to hold exactly what Rust just persisted, or the next Save
      // would point the setting back at the folder the files left. `report.to` is
      // the canonicalised path, so it is not used here.
      setDraft((previous) => (previous ? { ...previous, modelDir: target } : previous));
      toast(describeMove(report));
    }
  };

  /**
   * Save, then show what Rust read back from the credential store rather than
   * what was typed: the masked ends are the only part that may reach the screen.
   */
  const saveToken = async () => {
    const pasted = tokenDraft.trim();
    if (!pasted) {
      setTokenFailure("Paste the token first.");
      return;
    }
    setTokenBusy(true);
    setTokenFailure(null);
    try {
      const stored = await api.setHfToken(pasted);
      queryClient.setQueryData(queryKeys.hfToken, stored);
      // The field is emptied on purpose: a token that is still selectable text is
      // a token that can be copied into a chat window.
      setTokenDraft("");
      toast({ title: "Token saved in your credential store", variant: "success" });
    } catch (error) {
      setTokenFailure(describeError(error));
    } finally {
      setTokenBusy(false);
    }
  };

  const removeToken = async () => {
    setTokenBusy(true);
    setTokenFailure(null);
    try {
      const left = await api.clearHfToken();
      queryClient.setQueryData(queryKeys.hfToken, left);
      // A `HF_TOKEN` in the environment is not ours to delete, and the status
      // Rust returns says whether something is still being sent.
      toast({
        title: "Stored token removed",
        description: left.masked
          ? `A token still comes from ${left.source === "environment" ? "HF_TOKEN" : "the store"}.`
          : "Gated models will be refused again.",
        variant: "default",
      });
    } catch (error) {
      setTokenFailure(describeError(error));
    } finally {
      setTokenBusy(false);
    }
  };

  const reset = async () => {
    setResetting(true);
    setFailure(null);
    try {
      const outcome = await api.resetAppData();
      setResetResult(outcome);
      await queryClient.invalidateQueries();
      // The window still shows a transcript whose file was just deleted, so it
      // stops being a saved conversation rather than resurrecting itself.
      useChat.getState().startNew();
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
        description="Settings are plain JSON in the data folder and applied when you press Save; a Hugging Face token is the one thing kept in your OS credential store instead. Nothing is sent anywhere."
        icon={Settings2}
        sticky
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
                <Button variant="outline" onClick={() => void startMove()}>
                  <FolderInput />
                  Move
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                Leave empty for the default location. Saving points downloads, the library scan and
                resume bookkeeping at the new folder; existing files are not copied.{" "}
                <strong>Move</strong> takes them with it, so the setting and the files cannot
                disagree about where the models are.
              </p>
              {moveResult ? (
                <Note tone={leftBehind.length > 0 ? "warning" : "info"}>
                  <p className="font-medium">
                    {moveResult.moved + moveResult.copied > 0
                      ? `${moveResult.moved} renamed, ${moveResult.copied} copied — ${formatBytes(
                          moveResult.bytes,
                        )} now in ${moveResult.to}`
                      : `${moveResult.from} held no model files, so nothing had to move`}
                  </p>
                  {leftBehind.length > 0 ? (
                    <p>
                      Left where they are — a move never overwrites a file it found in the target,
                      and one it could not prove stayed put: {leftBehind.join(", ")}.
                    </p>
                  ) : null}
                </Note>
              ) : null}
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
                  ? "This build carries no engine that runs weights, so the mock engine answers whichever backend you pick here. Build with --features llama-cpp for real GGUF inference on Metal, CUDA or Vulkan; docs/models.md explains the toolchain it needs."
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

        <Card>
          <CardHeader
            title="Hugging Face access"
            description="Gated models need a token; everything else in the catalog does not."
          />
          <CardContent className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={status?.masked ? "success" : "neutral"}>
                {status?.masked ?? "no token"}
              </Badge>
              <span className="text-xs text-muted-foreground">{tokenPlace(status)}</span>
            </div>
            <FieldRow
              label={status?.masked ? "Replace the token" : "Access token"}
              hint="Make a fine-grained token at huggingface.co/settings/tokens that may read gated repositories. It goes into your OS credential store, never into settings.json, and is sent only to huggingface.co — a redirect to a CDN is served without it."
              htmlFor="hf-token"
            >
              <Input
                id="hf-token"
                type="password"
                value={tokenDraft}
                onChange={(event) => setTokenDraft(event.target.value)}
                placeholder="hf_…"
                autoComplete="off"
                spellCheck={false}
                disabled={tokenBusy || writable === false}
              />
            </FieldRow>
            {token.isError ? (
              <ErrorState
                message="The credential store could not be read"
                detail={describeError(token.error)}
                onRetry={() => void token.refetch()}
              />
            ) : null}
            {tokenFailure ? (
              <ErrorState message="The token was not saved" detail={tokenFailure} />
            ) : null}
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                onClick={() => void saveToken()}
                disabled={tokenBusy || writable === false}
              >
                <KeyRound />
                {tokenBusy ? "Working…" : status?.masked ? "Replace token" : "Save token"}
              </Button>
              {status?.masked ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void removeToken()}
                  disabled={tokenBusy}
                >
                  <Trash2 />
                  Remove
                </Button>
              ) : null}
            </div>
            {writable === false ? (
              <Note tone="warning">
                <p>
                  This build compiles no credential store for your platform, so the field above cannot
                  save. Export <span className="font-mono text-[11px]">HF_TOKEN</span> instead — it is
                  read the same way and goes to the same host.
                </p>
              </Note>
            ) : null}
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
        open={pendingMove !== null}
        onOpenChange={(open) => {
          if (!open) {
            setPendingMove(null);
          }
        }}
        title="Move the model folder?"
        description={
          <>
            <p>
              {moveCount === null ? (
                <>The file list is still loading, so the report afterwards says what moved.</>
              ) : moveCount === 0 ? (
                <>
                  <span className="font-mono text-[11px]">{appInfo.data?.modelsDir}</span> holds no
                  model files, so this only points the app at{" "}
                  <span className="font-mono text-[11px]">{pendingMove}</span>.
                </>
              ) : (
                <>
                  {moveCount} file(s) · {formatBytes(libraryBytes)} move from{" "}
                  <span className="font-mono text-[11px]">{appInfo.data?.modelsDir}</span> to{" "}
                  <span className="font-mono text-[11px]">{pendingMove}</span>.
                </>
              )}
            </p>
            <p className="mt-2">
              On one volume a file is renamed, which is instant. Across volumes it is copied and its
              header re-read before the original is deleted. A name the new folder already holds
              stays in both places and is listed afterwards: nothing is overwritten, and the old
              folder is never deleted. A paused download goes with its folder; files that are not
              models are left where they are.
            </p>
            {handle ? (
              <p className="mt-2">
                {handle.displayName} is loaded right now, so this will be refused until the model is
                unloaded.
              </p>
            ) : null}
          </>
        }
        confirmLabel={
          moveCount === null
            ? "Move the files"
            : moveCount > 0
              ? `Move ${moveCount} file(s)`
              : "Change the folder"
        }
        pending={moving}
        onConfirm={() => void confirmMove()}
      />

      <ConfirmDialog
        open={showReset}
        onOpenChange={setShowReset}
        title="Reset app data?"
        description="Settings return to their defaults and the in-memory log buffer is cleared. Model files are kept — nothing you downloaded is thrown away — and so is a Hugging Face token, which lives in your OS credential store rather than in this folder; remove it from the Hugging Face access card if you want it gone."
        confirmLabel="Reset settings"
        destructive
        pending={resetting}
        onConfirm={() => void reset()}
      />
    </>
  );
}
