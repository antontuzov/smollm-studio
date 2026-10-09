import { useState } from "react";
import {
  Download,
  FilePlus2,
  FolderOpen,
  HardDrive,
  MessageSquare,
  Play,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from "lucide-react";

import {
  deleteLocalModel,
  loadModel,
  openFolder,
  unloadModel,
  verifyModels,
} from "@/lib/actions";
import { formatBytes, formatDateTime, describeError } from "@/lib/format";
import { importModelFiles, pickModelFiles } from "@/lib/imports";
import { useLocalModels } from "@/lib/queries";
import { useEngine } from "@/stores/engine";
import { useImports } from "@/stores/imports";
import { useUi } from "@/stores/ui";
import { cn } from "@/lib/utils";
import { DownloadList } from "@/components/download-list";
import { PageHeader } from "@/components/page-header";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/dialog";
import { EmptyState, ErrorState, Note, SkeletonList } from "@/components/ui/feedback";

import type { LocalModel, ModelVerification } from "@/lib/types";

export function LibraryPage() {
  const [pendingDelete, setPendingDelete] = useState<LocalModel | null>(null);
  const [removing, setRemoving] = useState(false);
  const [reports, setReports] = useState<Record<string, ModelVerification>>({});
  const [checking, setChecking] = useState(false);
  const library = useLocalModels();
  const copying = useImports((state) => state.copying);
  const handle = useEngine((state) => state.handle);
  const metrics = useEngine((state) => state.metrics);
  const busy = useEngine((state) => state.busy);
  const setPage = useUi((state) => state.setPage);

  const models = library.data ?? [];
  const totalBytes = models.reduce((sum, model) => sum + model.sizeBytes, 0);
  const checked = Object.values(reports);
  const broken = checked.filter((report) => !report.ok);

  const importFromDisk = async () => {
    // The panel is the only place a path can come from besides a drop, and both
    // end up in the same command so the validation is the same.
    await importModelFiles(await pickModelFiles());
  };

  const runVerification = async (fileName?: string) => {
    setChecking(true);
    const result = await verifyModels(fileName);
    setChecking(false);
    if (result) {
      setReports((previous) => {
        const next = { ...previous };
        for (const report of result) {
          next[report.fileName] = report;
        }
        return next;
      });
    }
  };

  const confirmDelete = async () => {
    const target = pendingDelete;
    if (!target) {
      return;
    }
    setRemoving(true);
    const removed = await deleteLocalModel(target.fileName);
    setRemoving(false);
    setPendingDelete(null);
    if (removed) {
      // A deleted file has no report to keep showing.
      setReports((previous) => {
        const next = { ...previous };
        delete next[target.fileName];
        return next;
      });
    }
    if (removed && handle?.path === target.path) {
      // The file behind the resident model is gone; drop the stale badge.
      await unloadModel();
    }
  };

  return (
    <>
      <PageHeader
        title="Library"
        description={`Every .gguf file in the model folder, parsed straight from the file header. ${models.length} file(s) · ${formatBytes(totalBytes)} on disk. Import or drop adds a file from anywhere on this machine; Verify re-reads each header and checks it against the bytes actually present.`}
        icon={HardDrive}
        actions={
          <>
            <Button variant="outline" onClick={() => void openFolder("models")}>
              <FolderOpen />
              Show folder
            </Button>
            <Button
              variant="outline"
              onClick={() => void runVerification()}
              disabled={checking || models.length === 0}
            >
              <ShieldCheck />
              {checking ? "Verifying…" : "Verify"}
            </Button>
            <Button variant="outline" onClick={() => void library.refetch()}>
              <RefreshCw />
              Rescan
            </Button>
            <Button onClick={() => void importFromDisk()} disabled={copying.length > 0}>
              <FilePlus2 />
              Import from disk
            </Button>
          </>
        }
      />

      <div className="grid gap-5 xl:grid-cols-[minmax(0,1fr)_360px]">
        <div className="space-y-4 p-6 pb-0">
          {copying.length > 0 ? (
            <Note tone="info">
              <p>
                Copying {copying.join(", ")} into the model folder. A large file takes a
                moment; the list refreshes as each one lands.
              </p>
            </Note>
          ) : null}

          {checked.length > 0 ? (
            <Note tone={broken.length > 0 ? "danger" : "info"}>
              <p>
                {broken.length === 0 ? (
                  <>
                    <span className="font-medium">
                      {checked.length} file(s) verified.
                    </span>{" "}
                    Each header parsed, its tensor data is present and its size is
                    plausible for the parameters it declares.
                  </>
                ) : (
                  <>
                    <span className="font-medium">
                      {broken.length} of {checked.length} file(s) failed.
                    </span>{" "}
                    {broken.map((report) => report.fileName).join(", ")} should be
                    deleted and downloaded again.
                  </>
                )}
              </p>
              <p className="text-[11px] text-muted-foreground">
                GGUF files carry no checksum, so this proves the file is whole by
                its own header and size — not that every weight byte survived a
                disk error. A model that loads but answers nonsense is not
                something this check can predict.
              </p>
            </Note>
          ) : null}

          {handle ? (
            <Note tone="info">
              <p>
                <span className="font-medium">{handle.displayName}</span> is resident in{" "}
                <span className="font-mono">{handle.engine}</span> with a {handle.contextLength}{" "}
                token context.
              </p>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="outline" onClick={() => void unloadModel()} disabled={busy}>
                  Unload
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setPage("chat")}>
                  <MessageSquare />
                  Open chat
                </Button>
              </div>
            </Note>
          ) : null}

          {library.isPending ? (
            <SkeletonList rows={4} />
          ) : library.isError ? (
            <ErrorState
              message="The model folder could not be scanned"
              detail={describeError(library.error)}
              onRetry={() => void library.refetch()}
              retryLabel="Rescan"
            />
          ) : models.length === 0 ? (
            <EmptyState
              icon={HardDrive}
              title="No models downloaded yet"
              description="Pick a model on the Models page and press Download. A .gguf you already have works too: import it, or drop it anywhere on this window — it is copied in, never moved."
              action={
                <div className="flex flex-wrap gap-2">
                  <Button onClick={() => setPage("models")}>
                    <Download />
                    Browse models
                  </Button>
                  <Button variant="outline" onClick={() => void importFromDisk()}>
                    <FilePlus2 />
                    Import from disk
                  </Button>
                </div>
              }
            />
          ) : (
            <ul className="space-y-3">
              {models.map((model) => (
                <li key={model.path}>
                  <LocalModelRow
                    model={model}
                    loaded={handle?.path === model.path}
                    simulated={metrics?.simulated ?? false}
                    busy={busy}
                    report={reports[model.fileName]}
                    checking={checking}
                    onLoad={() => void loadModel(model.catalogId ?? model.fileName)}
                    onChat={async () => {
                      if (await loadModel(model.catalogId ?? model.fileName)) {
                        setPage("chat");
                      }
                    }}
                    onVerify={() => void runVerification(model.fileName)}
                    onDelete={() => setPendingDelete(model)}
                  />
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="space-y-4 p-6 pl-0">
          <Card>
            <CardHeader title="Transfers" description="This session's downloads." />
            <CardContent>
              <DownloadList />
            </CardContent>
          </Card>
        </div>
      </div>

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null);
          }
        }}
        title="Delete this model file?"
        description={
          pendingDelete
            ? `${pendingDelete.fileName} (${formatBytes(pendingDelete.sizeBytes)}) will be removed from disk. You can download it again at any time.`
            : ""
        }
        confirmLabel="Delete file"
        destructive
        pending={removing}
        onConfirm={() => void confirmDelete()}
      />
    </>
  );
}

function LocalModelRow({
  model,
  loaded,
  simulated,
  busy,
  report,
  checking,
  onLoad,
  onChat,
  onVerify,
  onDelete,
}: {
  model: LocalModel;
  loaded: boolean;
  simulated: boolean;
  busy: boolean;
  report?: ModelVerification;
  checking: boolean;
  onLoad: () => void;
  onChat: () => void;
  onVerify: () => void;
  onDelete: () => void;
}) {
  const { metadata } = model;
  return (
    <Card className="gap-0 p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-sm font-semibold">{metadata.name ?? model.fileName}</h3>
            {loaded ? <StatusPill tone={simulated ? "warning" : "success"} label="loaded" pulse={simulated} /> : null}
          </div>
          <p className="truncate font-mono text-[11px] text-muted-foreground">{model.fileName}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {loaded ? null : (
            <Button size="sm" onClick={onLoad} disabled={busy}>
              <Play />
              Load
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={onChat} disabled={busy}>
            <MessageSquare />
            Chat
          </Button>
          <Button size="sm" variant="ghost" onClick={onVerify} disabled={checking}>
            <ShieldCheck />
            {report ? "Re-check" : "Verify"}
          </Button>
          <Button size="sm" variant="ghost" aria-label="Delete file" onClick={onDelete}>
            <Trash2 className="text-destructive" />
          </Button>
        </div>
      </div>

      <div className="mt-3 flex flex-wrap gap-1.5 text-[11px]">
        {metadata.architecture ? <Badge>{metadata.architecture}</Badge> : null}
        {metadata.quantization ? <Badge tone="primary">{metadata.quantization}</Badge> : null}
        {metadata.parametersB ? <Badge>{metadata.parametersB}B params</Badge> : null}
        {metadata.contextLength ? <Badge>{metadata.contextLength} context</Badge> : null}
        {metadata.trainType ? <Badge tone="info">{metadata.trainType}</Badge> : null}
        {metadata.blockCount ? <Badge tone="neutral">{metadata.blockCount} blocks</Badge> : null}
        {metadata.weightBytes ? (
          <Badge tone="neutral">{formatBytes(metadata.weightBytes)} of weights</Badge>
        ) : null}
        {metadata.vocabSize ? <Badge tone="neutral">{metadata.vocabSize.toLocaleString()} vocab</Badge> : null}
        <Badge tone={model.catalogId ? "success" : "neutral"}>
          {model.catalogId ? "from catalog" : "manual file"}
        </Badge>
      </div>

      <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
        <span className="tabular-nums">{formatBytes(model.sizeBytes)}</span>
        <span className="truncate font-mono">{model.path}</span>
        <span>{formatDateTime(model.modifiedMs)}</span>
      </div>

      {model.parseError ? (
        <Note tone="warning" className="mt-3">
          <p className="font-medium">The GGUF header could not be parsed</p>
          <p>{model.parseError}</p>
          <p>The file is still listed and can be loaded if it is valid.</p>
        </Note>
      ) : null}

      {report ? <VerificationReport report={report} /> : null}
    </Card>
  );
}

function VerificationReport({ report }: { report: ModelVerification }) {
  const failed = report.checks.filter((check) => check.status === "failed").length;
  return (
    <div className="mt-3 rounded-lg border bg-secondary/40 px-3 py-2.5">
      <p className="text-[11px] font-medium">
        {report.ok
          ? "Verified: every check the file can answer for itself came back clean."
          : `Verification failed on ${failed} check(s).`}
      </p>
      <ul className="mt-1.5 space-y-1">
        {report.checks.map((check) => (
          <li key={check.label} className="flex items-start gap-2 text-[11px]">
            <span
              className={cn(
                "mt-0.5 size-1.5 shrink-0 rounded-full",
                check.status === "passed" && "bg-emerald-500",
                check.status === "failed" && "bg-destructive",
                check.status === "skipped" && "bg-muted-foreground/40",
              )}
              aria-hidden
            />
            <span className="min-w-0">
              <span className="font-medium">{check.label}</span>{" "}
              <span className="text-muted-foreground">{check.detail}</span>
              {check.status === "skipped" ? (
                <span className="text-muted-foreground/70"> (not judged)</span>
              ) : null}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
