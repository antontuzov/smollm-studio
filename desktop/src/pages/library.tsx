import { useState } from "react";
import {
  Download,
  FolderOpen,
  HardDrive,
  MessageSquare,
  Play,
  RefreshCw,
  Trash2,
} from "lucide-react";

import { deleteLocalModel, loadModel, openFolder, unloadModel } from "@/lib/actions";
import { formatBytes, formatDateTime, describeError } from "@/lib/format";
import { useLocalModels } from "@/lib/queries";
import { useEngine } from "@/stores/engine";
import { useUi } from "@/stores/ui";
import { DownloadList } from "@/components/download-list";
import { PageHeader } from "@/components/page-header";
import { Badge, StatusPill } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/dialog";
import { EmptyState, ErrorState, Note, SkeletonList } from "@/components/ui/feedback";

import type { LocalModel } from "@/lib/types";

export function LibraryPage() {
  const [pendingDelete, setPendingDelete] = useState<LocalModel | null>(null);
  const [removing, setRemoving] = useState(false);
  const library = useLocalModels();
  const handle = useEngine((state) => state.handle);
  const metrics = useEngine((state) => state.metrics);
  const busy = useEngine((state) => state.busy);
  const setPage = useUi((state) => state.setPage);

  const models = library.data ?? [];
  const totalBytes = models.reduce((sum, model) => sum + model.sizeBytes, 0);

  const confirmDelete = async () => {
    const target = pendingDelete;
    if (!target) {
      return;
    }
    setRemoving(true);
    const removed = await deleteLocalModel(target.fileName);
    setRemoving(false);
    setPendingDelete(null);
    if (removed && handle?.path === target.path) {
      // The file behind the resident model is gone; drop the stale badge.
      await unloadModel();
    }
  };

  return (
    <>
      <PageHeader
        title="Library"
        description={`Every .gguf file in the model folder, parsed straight from the file header. ${models.length} file(s) · ${formatBytes(totalBytes)} on disk.`}
        icon={HardDrive}
        actions={
          <>
            <Button variant="outline" onClick={() => void openFolder("models")}>
              <FolderOpen />
              Show folder
            </Button>
            <Button variant="outline" onClick={() => void library.refetch()}>
              <RefreshCw />
              Rescan
            </Button>
          </>
        }
      />

      <div className="grid gap-5 xl:grid-cols-[minmax(0,1fr)_360px]">
        <div className="space-y-4 p-6 pb-0">
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
              description="Pick a model on the Models page and press Download. Files you copy into the model folder by hand are listed here too."
              action={
                <Button onClick={() => setPage("models")}>
                  <Download />
                  Browse models
                </Button>
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
                    onLoad={() => void loadModel(model.catalogId ?? model.fileName)}
                    onChat={async () => {
                      if (await loadModel(model.catalogId ?? model.fileName)) {
                        setPage("chat");
                      }
                    }}
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
  onLoad,
  onChat,
  onDelete,
}: {
  model: LocalModel;
  loaded: boolean;
  simulated: boolean;
  busy: boolean;
  onLoad: () => void;
  onChat: () => void;
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
    </Card>
  );
}
