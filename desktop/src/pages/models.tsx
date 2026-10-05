import { useMemo, useState } from "react";
import { Boxes, Filter, RefreshCw, Search, SlidersHorizontal, X } from "lucide-react";

import { cancelDownload, loadModel, pullModel } from "@/lib/actions";
import { describeError } from "@/lib/format";
import { useCatalog, useCatalogFacets } from "@/lib/queries";
import { useDownloads, useEngine } from "@/stores/engine";
import { useUi } from "@/stores/ui";
import { ModelCard } from "@/components/model-card";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { EmptyState, ErrorState, SkeletonCards } from "@/components/ui/feedback";
import { Input, Select, Switch } from "@/components/ui/field";

import type { CatalogEntry, CatalogFilterInput, FacetValue } from "@/lib/types";

const sortOptions = [
  { value: "recommended", label: "Recommended for this machine" },
  { value: "smallest", label: "Smallest download" },
  { value: "largest", label: "Largest download" },
  { value: "fastest", label: "Fastest tokens" },
  { value: "name", label: "Name" },
];

/**
 * Parameter bands the catalog is curated around. Edges are inclusive, matching
 * `minParametersB` / `maxParametersB` on the command.
 */
const sizeBands = [
  { value: "", label: "Any size", min: undefined, max: undefined },
  { value: "up-to-1", label: "Up to 1B params", min: undefined, max: 1 },
  { value: "1-2", label: "1B to 2B params", min: 1, max: 2 },
  { value: "2-3", label: "2B to 3B params", min: 2, max: 3 },
  { value: "3-4", label: "3B to 4B params", min: 3, max: 4 },
  { value: "4-plus", label: "4B and up", min: 4, max: undefined },
];

const availabilityOptions = [
  { value: "", label: "Downloaded or not" },
  { value: "yes", label: "In my library" },
  { value: "no", label: "Not downloaded" },
];

const defaultFilters: CatalogFilterInput = {
  query: "",
  sort: "recommended",
  hidePlaceholders: false,
};

/** A dropdown option list built from catalog data, so overlays add to it too. */
function facetOptions(facets: FacetValue[], anyLabel: string) {
  return [
    { value: "", label: anyLabel },
    ...facets.map((facet) => ({
      value: facet.value,
      label: `${facet.value} (${facet.count})`,
    })),
  ];
}

export function ModelsPage() {
  const [filters, setFilters] = useState<CatalogFilterInput>(defaultFilters);
  const [band, setBand] = useState("");

  const tasks = useDownloads((state) => state.tasks);
  const busy = useEngine((state) => state.busy);
  const setPage = useUi((state) => state.setPage);
  const facets = useCatalogFacets();
  const catalog = useCatalog(filters);

  const patch = (next: Partial<CatalogFilterInput>) =>
    setFilters((current) => ({ ...current, ...next }));

  const selectBand = (value: string) => {
    setBand(value);
    const selected = sizeBands.find((option) => option.value === value) ?? sizeBands[0];
    patch({ minParametersB: selected.min, maxParametersB: selected.max });
  };

  const pickAvailability = (value: string) => {
    patch({
      downloaded: value === "yes" ? true : value === "no" ? false : undefined,
    });
  };

  const availability = filters.downloaded === undefined ? "" : filters.downloaded ? "yes" : "no";

  const activeCount = useMemo(() => {
    let count = 0;
    if (filters.query.trim().length > 0) {
      count += 1;
    }
    if (band.length > 0) {
      count += 1;
    }
    for (const value of [filters.quantization, filters.tag, filters.license, filters.architecture]) {
      if (value) {
        count += 1;
      }
    }
    if (filters.downloaded !== undefined) {
      count += 1;
    }
    if (filters.hidePlaceholders) {
      count += 1;
    }
    return count;
  }, [band, filters]);

  const clearFilters = () => {
    setBand("");
    setFilters(defaultFilters);
  };

  const downloadIdFor = useMemo(() => {
    const map = new Map<string, string>();
    for (const task of Object.values(tasks)) {
      map.set(task.modelId, task.id);
    }
    return map;
  }, [tasks]);

  const models: CatalogEntry[] = catalog.data ?? [];
  const options = facets.data;

  return (
    <>
      <PageHeader
        title="Models"
        description="A curated list of small GGUF models, checked against the RAM in this machine. Placeholders are entries whose Hugging Face id has not been verified from here."
        icon={Boxes}
        actions={
          <>
            {activeCount > 0 ? (
              <Button variant="ghost" size="sm" onClick={clearFilters}>
                <X />
                Clear {activeCount} filter{activeCount === 1 ? "" : "s"}
              </Button>
            ) : null}
            <Button variant="outline" onClick={() => void catalog.refetch()}>
              <RefreshCw />
              Reload catalog
            </Button>
          </>
        }
      />

      <div className="space-y-5 p-6">
        <Card>
          <CardHeader
            title="Filters"
            description={
              catalog.isFetching
                ? "Filtering…"
                : `${models.length} of the catalog shown${
                    filters.hidePlaceholders ? " · unverified hidden" : ""
                  }`
            }
            actions={<SlidersHorizontal className="size-4 text-muted-foreground" />}
          />
          <CardContent>
            <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
              <div className="space-y-2 xl:col-span-2">
                <label htmlFor="model-search" className="text-xs font-medium text-muted-foreground">
                  Search
                </label>
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    id="model-search"
                    className="pl-9"
                    placeholder="qwen, phi, coding…"
                    value={filters.query}
                    onChange={(event) => patch({ query: event.target.value })}
                  />
                </div>
              </div>
              <Select
                label="Sort by"
                value={filters.sort}
                options={sortOptions}
                onChange={(value) => patch({ sort: value })}
              />
              <Select
                label="Availability"
                value={availability}
                options={availabilityOptions}
                onChange={pickAvailability}
              />
              <Select label="Parameter size" value={band} options={sizeBands} onChange={selectBand} />
              <Select
                label="Quantization"
                value={filters.quantization ?? ""}
                options={facetOptions(options?.quantizations ?? [], "Any quantization")}
                onChange={(value) => patch({ quantization: value || undefined })}
                disabled={facets.isPending}
                hint="The GGUF quantisation the download is."
              />
              <Select
                label="Tag"
                value={filters.tag ?? ""}
                options={facetOptions(options?.tags ?? [], "Any tag")}
                onChange={(value) => patch({ tag: value || undefined })}
                disabled={facets.isPending}
              />
              <Select
                label="License"
                value={filters.license ?? ""}
                options={facetOptions(options?.licenses ?? [], "Any license")}
                onChange={(value) => patch({ license: value || undefined })}
                disabled={facets.isPending}
                hint="Terms differ per model; the card shows each one."
              />
              <Select
                label="Architecture"
                value={filters.architecture ?? ""}
                options={facetOptions(options?.architectures ?? [], "Any architecture")}
                onChange={(value) => patch({ architecture: value || undefined })}
                disabled={facets.isPending}
                hint="What the file's GGUF header declares an engine must support."
              />
              <div className="flex items-end pb-1">
                <Switch
                  checked={filters.hidePlaceholders}
                  onCheckedChange={(checked) => patch({ hidePlaceholders: checked })}
                  label="Hide unverified ids"
                />
              </div>
            </div>
          </CardContent>
        </Card>

        {catalog.isPending ? (
          <SkeletonCards cards={6} />
        ) : catalog.isError ? (
          <ErrorState
            message="The model catalog could not be read"
            detail={describeError(catalog.error)}
            onRetry={() => void catalog.refetch()}
            retryLabel="Reload"
          />
        ) : models.length === 0 ? (
          <EmptyState
            icon={activeCount > 0 ? Filter : Search}
            title="No models match these filters"
            description="Try a shorter search term, a wider size band, or show unverified ids to see the full catalog."
            action={
              <Button variant="outline" onClick={clearFilters}>
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
