import { useMemo, useState } from "react";
import { Boxes, RefreshCw, Search, SlidersHorizontal } from "lucide-react";

import { cancelDownload, loadModel, pullModel } from "@/lib/actions";
import { describeError } from "@/lib/format";
import { useCatalog } from "@/lib/queries";
import { useDownloads, useEngine } from "@/stores/engine";
import { useUi } from "@/stores/ui";
import { ModelCard } from "@/components/model-card";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { EmptyState, ErrorState, SkeletonList } from "@/components/ui/feedback";
import { Input, Select, Switch } from "@/components/ui/field";

import type { CatalogEntry } from "@/lib/types";

const sortOptions = [
  { value: "recommended", label: "Recommended for this machine" },
  { value: "smallest", label: "Smallest download" },
  { value: "largest", label: "Largest download" },
  { value: "fastest", label: "Fastest tokens" },
  { value: "name", label: "Name" },
];

const sizeOptions = [
  { value: "", label: "Any size" },
  { value: "1", label: "Up to 1B params" },
  { value: "2", label: "Up to 2B params" },
  { value: "4", label: "Up to 4B params" },
  { value: "8", label: "Up to 8B params" },
];

export function ModelsPage() {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState("recommended");
  const [maxSize, setMaxSize] = useState("");
  const [hidePlaceholders, setHidePlaceholders] = useState(false);

  const tasks = useDownloads((state) => state.tasks);
  const busy = useEngine((state) => state.busy);
  const setPage = useUi((state) => state.setPage);
  const catalog = useCatalog({
    query,
    sort,
    maxParametersB: maxSize.length > 0 ? Number(maxSize) : undefined,
    hidePlaceholders,
  });

  const downloadIdFor = useMemo(() => {
    const map = new Map<string, string>();
    for (const task of Object.values(tasks)) {
      map.set(task.modelId, task.id);
    }
    return map;
  }, [tasks]);

  const models: CatalogEntry[] = catalog.data ?? [];

  return (
    <>
      <PageHeader
        title="Models"
        description="A curated list of small GGUF models, checked against the RAM in this machine. Placeholders are entries whose Hugging Face id has not been verified from here."
        icon={Boxes}
        actions={
          <Button variant="outline" onClick={() => void catalog.refetch()}>
            <RefreshCw />
            Reload catalog
          </Button>
        }
      />

      <div className="space-y-5 p-6">
        <Card>
          <CardHeader
            title="Filters"
            description={
              catalog.isFetching
                ? "Filtering…"
                : `${models.length} of the catalog shown${hidePlaceholders ? " · unverified hidden" : ""}`
            }
            actions={<SlidersHorizontal className="size-4 text-muted-foreground" />}
          />
          <CardContent>
            <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
              <div className="space-y-2">
                <label htmlFor="model-search" className="text-xs font-medium text-muted-foreground">
                  Search
                </label>
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    id="model-search"
                    className="pl-9"
                    placeholder="qwen, phi, coding…"
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                  />
                </div>
              </div>
              <Select
                label="Sort by"
                value={sort}
                options={sortOptions}
                onChange={setSort}
              />
              <Select
                label="Model size"
                value={maxSize}
                options={sizeOptions}
                onChange={setMaxSize}
              />
              <div className="flex items-end pb-1">
                <Switch
                  checked={hidePlaceholders}
                  onCheckedChange={setHidePlaceholders}
                  label="Hide unverified ids"
                />
              </div>
            </div>
          </CardContent>
        </Card>

        {catalog.isPending ? (
          <SkeletonList rows={6} />
        ) : catalog.isError ? (
          <ErrorState
            message="The model catalog could not be read"
            detail={describeError(catalog.error)}
            onRetry={() => void catalog.refetch()}
            retryLabel="Reload"
          />
        ) : models.length === 0 ? (
          <EmptyState
            icon={Search}
            title="No models match these filters"
            description="Try a shorter search term, allow larger models, or show unverified ids to see the full catalog."
            action={
              <Button
                variant="outline"
                onClick={() => {
                  setQuery("");
                  setMaxSize("");
                  setHidePlaceholders(false);
                  setSort("recommended");
                }}
              >
                Clear filters
              </Button>
            }
          />
        ) : (
          <div className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-3">
            {models.map((model) => (
              <ModelCard
                key={model.id}
                model={model}
                busy={busy}
                onPull={(id) => void pullModel(id)}
                onLoad={(id) => void loadModel(id)}
                onChat={async (id) => {
                  if (await loadModel(id)) {
                    setPage("chat");
                  }
                }}
                onCancel={(id) => {
                  const downloadId = downloadIdFor.get(id);
                  if (downloadId) {
                    void cancelDownload(downloadId);
                  }
                }}
              />
            ))}
          </div>
        )}
      </div>
    </>
  );
}
