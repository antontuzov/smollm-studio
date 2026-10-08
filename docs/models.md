# Models

What this app can download, how it decides what fits, and how to bring your own
GGUF file.

## The catalog

`crates/smollm-models/data/catalog.json` is compiled into the binary
(`EMBEDDED_CATALOG_JSON`), so the Models page works with no network at all.
`schemaVersion` must be `1`; anything else is rejected rather than guessed at.

Each entry carries the coordinates needed to resolve and verify a download:

| Field | Why it is there |
| --- | --- |
| `id` | Stable key used by commands, settings and the server's `model` field |
| `hfRepo`, `filename`, `revision` | The exact blob: `https://huggingface.co/{repo}/resolve/{revision}/{filename}` |
| `parametersB`, `sizeMb`, `quantization` | Drive the RAM estimate and the size filters |
| `contextLength`, `recommendedRamGb` | What the card shows and what loading defaults to |
| `family`, `tags`, `speed`, `quality`, `vision` | Sorting, badges and recommendations |
| `architecture` | The GGUF header's `general.architecture` (`llama`, `qwen2`, `gemma3`, `phi3`, `granite`) — what an engine must support to load the file, and a Models page filter |
| `license` | Shown on the card; these models have different terms |
| `status` | `verified` or `placeholder` (see below) |

Thirteen entries ship, from 0.36B to 4B parameters, with at least one model in
every band the Models page filters by: under 1B, 1–2B, 2–3B, 3–4B and 4B and up.
`ModelCatalog::validate()` and the test suite hold that band coverage in place, so
a future edit cannot quietly empty a band. The list in
[the README](../README.md#model-support) is generated from the same file.

### `verified` versus `placeholder`

An entry is `verified` when its repository and file name were confirmed against
the Hugging Face resolve API. `placeholder` means the coordinates are plausible
but unconfirmed — the card says so, and the Models page can hide them with one
switch. Nothing is silently presented as certain when it is not.

`ModelCatalog::validate()` enforces the invariants on every load: non-empty
unique ids, `org/name` repositories, `.gguf` file names, parameter counts in
0.1–200, plausible sizes, context ≥ 512, a non-empty `architecture`, and a
buildable download URL. Problems
become warnings in diagnostics rather than a crash at startup.

## Filters

`ModelCatalog::filter()` applies search, parameter band, quantisation, tag,
license and architecture. `ModelCatalog::facets()` reports the distinct values
with entry counts, and the Models page builds every dropdown from it — so an
overlay entry with a new license or tag appears in the UI without a code change.
Whether a model is already downloaded is answered next to the library scan (in
the `list_catalog_models` command), not in this crate, so the catalog stays a
pure data structure.

## Overlays and your own models

Drop a file named `catalog.local.json` next to `settings.json` in the data
folder. Same schema, and entries merge by `id`: a matching id replaces the
bundled one, a new id is appended. That is how you add a private or unpublished
model without touching the repository.

Simpler still: **any `.gguf` file placed in the model folder shows up on the
Library page.** The scanner reads the file header directly — architecture,
quantisation, parameter count, context length, tensor count, license — so the
card is accurate even for a file the catalog has never heard of. A file whose
header cannot be parsed is listed with the parse error visible instead of being
hidden.

## Downloads

A transfer is a small state machine: *queued → running → (retrying)* →
*verifying → complete*, and it can end in *failed* or *cancelled*.

**Resume.** Bytes are written to `<name>.gguf.part`, so an interrupted file is
never lost. The next attempt asks the server for the missing tail with a `Range`
request and only accepts the reply if the response is `206` and its
`Content-Range` starts exactly where the partial ends. A server that answers
`200` instead is treated as unable to resume: the partial is discarded and the
file is written from scratch, because splicing a full body onto a tail would
produce a corrupt model.

Each partial also writes a sidecar, `<name>.gguf.part.meta.json`, recording the
URL and the file `ETag` it came from. If either changed — a re-uploaded export, a
different revision, a redirect to another mirror — the old bytes are dropped
rather than stitched onto the new stream. A partial without a sidecar (written by
an older build) is kept, since the size, GGUF header and checksum checks below
still have to pass before the file is published.

**Validation before the file is trusted.** In order: the on-disk length must
match the advertised total; the first bytes must parse as a real GGUF header with
a sane tensor count (this is what rejects an HTML error page that was served with
a `200`); the SHA-256 must match when a digest is available. Only then is the
`.part` fsynced and renamed into place, and only then does it appear in the local
library — and, if the local server is running, in `/v1/models` without a restart.
A failed check deletes the partial; a half-written model never enters the
library.

**Network errors.** Transient failures — a dropped connection, `429`, `5xx`, an
unparseable `Content-Range`, a `416` the server should not have sent — are retried
automatically up to four times with capped backoff, and honour `Retry-After` when
the server sends it. The Transfers row shows *retrying (attempt 2 of 4)* in amber
instead of pretending the download died; cancel still works while the task waits
between attempts. Authoritative sizes and digests come from the Hugging Face file
metadata endpoint; if that endpoint is unreachable, the download continues with
the catalog's own estimate and the size check becomes best-effort, so an API
outage does not make the app useless offline. Consent-gated repos fail with a
message that says to accept the licence rather than a bare `401`.

**Cancel, retry, disk.** Cancel is cooperative: the transfer loop checks a flag
between chunks, keeps the partial, and emits *cancelled*. Retry re-enters the
same path, so it resumes where it left off — and it is offered in both places a
failure is visible: the **Retry download** button on the model card and the
**Retry** button in the Transfers list. Free space is checked before the first
byte, with 64 MB of headroom, so you get `insufficient_disk_space` rather than a
full volume.

Progress events are throttled to about five per second and carry bytes, total,
percentage and the rolling speed, which is what the Transfers sidebar renders as
`MB / MB · % · MB/s`.

`smollm models pull <id>` uses the identical code path.

## Quantisation, briefly

Quantisation stores weights in fewer bits. Lower size, more loss. The catalog
sticks to Q4-class because that is where small models stay good.

| Tag | Bits/weight | Relative size | Comment |
| --- | --- | --- | --- |
| Q2_K | ~2.6 | smallest | Usually too degraded below 7B |
| Q3_K_M | ~3.4 | small | A 4B at Q3 can be worth it on 8 GB |
| Q4_K_M | ~4.6 | default | The sweet spot; what this catalog ships |
| Q5_K_M | ~5.7 | +24% | Noticeably better if RAM allows |
| Q8_0 | ~8.5 | +85% | Near-fp16; rarely worth it under 4B |
| IQ_* | varies | varies | Importance-sampled; backend support differs |

Files are named by convention (`model-Q4_K_M.gguf`), and the header records the
quantisation, so the Library page can show it truthfully.

## Backends

`Engine` is a trait; `EngineManager` holds one loaded model and hands out token
streams. Selection order is: what you asked for → what this build can actually
run → `mock`, with a warning recorded in the log and surfaced in the UI.

| Backend | Cargo feature | State |
| --- | --- | --- |
| `mock` | `mock` (default) | Streams tokens so the whole pipeline is testable. **Simulated output.** |
| `gguf-metadata` | always on | Reads headers; no generation. |
| `llama-cpp` | `llama-cpp` | **Real inference.** Loads the GGUF through llama.cpp: its own tokenizer and chat template, tokens streamed from a dedicated decode thread, cancellation checked between tokens, and metrics naming the device that actually ran. Compiles llama.cpp from the sources the `llama-cpp-2` bindings vendor, so it needs cmake and a C/C++ toolchain. |
| `candle` | `candle` | Adapter seam with a `TODO`. Experimental. |

`EngineKind` keeps those two questions apart on purpose: `compiled()` says the
feature is in the binary, `is_available()` also requires the native code to be
there, and nothing selects an engine that fails the second test. With
`--features llama-cpp` the llama.cpp engine passes both and is what answers;
without it, and for `candle` in any build, the app stays on MockEngine and says
so rather than pretending to run inference. See
[`crates/smollm-engine/src/llama.rs`](../crates/smollm-engine/src/llama.rs).
