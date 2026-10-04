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
| `license` | Shown on the card; these models have different terms |
| `status` | `verified` or `placeholder` (see below) |

Eleven entries ship, all between 0.36B and 4B parameters. The list in
[the README](../README.md#model-support) is generated from the same file.

### `verified` versus `placeholder`

An entry is `verified` when its repository and file name were confirmed against
the Hugging Face resolve API. `placeholder` means the coordinates are plausible
but unconfirmed — the card says so, and the Models page can hide them with one
switch. Nothing is silently presented as certain when it is not.

`ModelCatalog::validate()` enforces the invariants on every load: non-empty
unique ids, `org/name` repositories, `.gguf` file names, parameter counts in
0.1–200, plausible sizes, context ≥ 512, and a buildable download URL. Problems
become warnings in diagnostics rather than a crash at startup.

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

- Resumable: bytes go to `<name>.gguf.part` and continue from the existing
  offset with a `Range` request.
- Size-checked: the finished length must match the advertised total.
- SHA-256-checked when a digest is available; the state machine shows a distinct
  *Verifying checksum* step. A mismatch fails the task, it does not silently
  keep the file.
- Cancellable from the card or the sidebar, and retryable from the Transfers
  list.
- Disk is checked before the first byte: if the estimate does not fit with room
  to spare, you get `insufficient_disk_space` rather than a full volume.

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
streams. Selection order is: what you asked for → what is compiled in
→ `mock`, with a warning recorded in the log and surfaced in the UI.

| Backend | Cargo feature | State |
| --- | --- | --- |
| `mock` | `mock` (default) | Streams tokens so the whole pipeline is testable. **Simulated output.** |
| `gguf-metadata` | always on | Reads headers; no generation. |
| `llama-cpp` | `llama-cpp` | Adapter seam with a `TODO`. Needs vendored llama.cpp sources and a C/C++ toolchain. |
| `candle` | `candle` | Adapter seam with a `TODO`. Experimental. |

Enabling a feature today compiles an adapter that answers
`unsupported_backend` — "no native library is linked yet" — rather than
pretending to run inference. See
[`crates/smollm-engine/src/llama.rs`](../crates/smollm-engine/src/llama.rs).
That is the honest state of this project, and the single most useful contribution
would be to replace it.
