# Changelog

All notable changes to SmolLLM Studio are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Model catalog: the 2B band and an `architecture` field.** Two new entries —
  MiniCPM5 2B and Granite 3.1 2B Instruct — fill the gap between 1.7B and 3B, so
  every size band the Models page offers has at least one curated model. Each
  entry now also carries `architecture`, read from the GGUF file's own header
  rather than inferred from the repo name; the two new downloads, their byte
  sizes, parameter counts and licenses were resolved against Hugging Face on
  2026-10-05. `ModelCatalog::validate()` requires it and a test pins band
  coverage.
- **Granite prompt family.** `ModelFamily::Granite` renders IBM's Harmony-style
  markers (`<|start_of_role|>…<|end_of_role|>…<|end_of_text|>`), copied from the
  chat template embedded in the file. Previously a Granite GGUF would have been
  prompted as generic ChatML.
- **Models page filters**: parameter band, quantisation, tag, license,
  architecture and downloaded-or-not, on top of the existing search, sort and
  hide-unverified switch. The dropdown options come from a new `catalog_facets`
  command, so a `catalog.local.json` overlay extends them automatically.
- **Automatic retry with backoff.** A dropped connection, timeout, `429` or
  `5xx` is retried up to four times, honouring `Retry-After` when the server
  sends it, and shows as a new `retrying` state — amber *attempt 2 of 4* prose
  rather than a red failure. Cancel still works while a task waits between
  attempts. The CLI reports the same attempts through its progress stream.
- **Retry on the model card.** A failed or cancelled download now shows its
  reason and a **Retry download** button on the Models page card, with the
  percentage already on disk, rather than silently reverting to a plain Download
  button and sending you to find the Transfers list. `list_catalog_models`
  entries carry `downloadState` and `downloadError` (the newest transfer for that
  model), and the card's status pill reports the real state, so a task in backoff
  reads *retrying* instead of *running*.
- **Download-engine tests over a real socket.** Nine of them run against a
  loopback HTTP server that can drop connections, ignore `Range`, re-upload under
  a new `ETag`, serve a non-GGUF body and gate the repo: resume continuity,
  restart-on-`200`, provenance mismatch, header and checksum validation, joining
  an in-flight transfer and the retry state. `HfClient` gained a private
  `endpoint` so the whole path is testable without touching the internet.
- **`stream_options.include_usage` is honoured.** A streamed chat now ends with
  `data:` chunks a strict OpenAI client expects: content deltas carry no `usage`,
  and when the flag is set one final `chat.completion.chunk` arrives whose
  `choices` is `[]` and which holds the token counts, just before `[DONE]`.
  Previously the field was parsed and dropped. Every SSE response also sets
  `Cache-Control: no-cache, no-transform` and `x-accel-buffering: no`, because a
  proxy that buffers turns a token stream into a wait.
- **The Server page says what the server offers.** `ServerStatus` carries
  `servedModels` — the exact ids `/v1/models` answers for, resident model first —
  shown as chips, and `get_server_examples` gained `health` (a `curl -s …/health`)
  and `curlStream` (the same call with `stream: true`, built for `curl -N`), which
  become a third tab. The snippets no longer wait on a loaded model. `smollm serve`
  prints the same set, and the CLI's endpoint list names `/v1/models/{id}` and
  `/v1/engine/metrics` too.
- **Two more over-the-socket HTTP tests**, for the part-list `content` form and for
  `include_usage`, taking `crates/smollm-server` to eleven tests that run against
  a real listener rather than a mocked request.
- **`EngineKind` separates *compiled* from *able to run*.** `compiled()` says a
  cargo feature is in the binary, `is_linked()` says the engine behind it has the
  native library it needs, and `is_available()` requires both. `unavailability()`
  states which of the two is missing, in the sentence every surface shows — CLI
  warning, load toast, `unsupported_backend`. Selection uses `is_available()`, so
  no configuration can pick an engine that only knows how to refuse.

### Fixed

- **`messages[].content` now accepts what OpenAI sends.** A list of typed parts
  (`[{"type":"text",…}]`) and `null` on an assistant tool-call turn were both
  hard `422`s, which broke SDK clients that had done nothing wrong. Text parts are
  joined in order and non-text parts are dropped — the engines behind this server
  generate from text — and the flattening lives in the wire types, so the app's own
  `ChatMessage` stays a plain string pair.
- **A malformed body answers in OpenAI's envelope.** Bodies that are not valid
  JSON were rejected by Axum's `Json` extractor: a bare `422` with a plain-text
  body no SDK can parse. They are parsed by hand now, so the reply is a `400` with
  `{"error":{"type":"invalid_request_error","param":"body",…}}`.
- **The mock quoted the chat template, not the question.** `MockEngine` echoes the
  user's line back into its canned answer, but with the app's rendered prompt the
  last line is a template marker, so every demo answer began *"Short answer about
  &lt;assistant&gt;"*. ChatML, `<user>`-style, Gemma and Granite scaffolding and the
  bare role words are now stripped first.
- **The docs promised lazy loading the server does not do.** `docs/api.md` and
  `docs/getting-started.md` both claimed a request would load the default model
  when nothing was resident. It does not: the routes never touch the downloader, and
  the request fails with a `404` that says so. The copy now matches, including the
  Server page's own notes.
- **A resume could not produce a corrupt model any more.** The old transfer sent
  a `Range` header and then appended whatever came back. A server that ignores
  `Range` and answers `200` with the whole file would therefore be written onto
  the end of the partial, doubling its length with a duplicated head. The resume
  path now only appends on a `206` whose `Content-Range` starts exactly at the
  local offset; anything else discards the partial and re-downloads, and a
  `Content-Range` that disagrees is treated as a transient server fault.
- **Partial files carry provenance.** Each `.part` writes a
  `<name>.gguf.part.meta.json` sidecar with the source URL and `ETag`. If the
  remote file is re-uploaded, the revision moves, or the URL redirects elsewhere,
  the stale bytes are discarded instead of being spliced onto the new stream.
- **Resumed downloads verified the wrong checksum.** The SHA-256 was computed
  over the bytes of the last attempt only, so a file resumed after one dropped
  connection could never match its digest and was deleted. The digest is now read
  from the finished file on disk.
- **Non-GGUF responses no longer enter the library.** A login page or error
  document served with a `200` and a plausible length used to be renamed into
  place and listed as a model. The completed file's GGUF header is parsed before
  publishing, so the task fails with `gguf_parse` and the partial is removed.
- **Authoritative sizes instead of catalog guesses.** The transfer now asks the
  Hugging Face file-metadata endpoint for the real byte length and digest before
  downloading, which makes the percentage, the speed and the size check honest,
  and detects a truncated stream that ends at a round number. If that endpoint is
  unreachable, the download proceeds on the catalog's estimate rather than
  failing outright.
- **A completed download is immediately servable.** The download event pump
  refreshes the local server's advertised model list on completion, so a fresh
  model appears in `/v1/models` without restarting the server.
- **The last write buffer is no longer lost on a failed attempt.** Flushing now
  happens before the error propagates, so a resume restarts at the true offset
  instead of up to 64 KiB earlier, and the file is fsynced before the rename into
  the library.
- `HfClient::probe` returned the metadata endpoint as the download URL; it now
  returns the file URL and keeps the metadata URL internal.
- **`--features llama-cpp` used to cost the app its ability to run anything.**
  `is_available()` asked only whether a feature was compiled, so
  `kind_for_backend` preferred the unlinked adapter over Mock, every
  `load_model` came back `unsupported_backend`, and `fallback_warning` stayed
  silent because the engine looked native. Metal, CUDA, Vulkan and CPU all
  resolve to MockEngine now and say so in one sentence; `smollm run --engine
  llama-cpp` prints the reason and answers normally instead of aborting. The
  engine crate's tests are gated on the engine they need, so
  `cargo test -p smollm-engine` passes with `--no-default-features`,
  `--features llama-cpp`, `--features candle` and `--all-features` — the first
  three previously did not compile at all.
- **Three screens promised what a feature flag cannot deliver.** Chat, Home and
  Settings said building with `llama-cpp` yields real inference; the flag
  compiles an adapter and links no library, so they now say what is missing and
  that the numbers shown are modelled, not measured.

## [0.1.0] - 2026-10-04

First release. It is a complete, working *application shell* for running small
GGUF models locally — and it is honest about which parts do real inference.

### Added

- **Workspace of six libraries plus the desktop app**: `smollm-core` (config,
  errors, paths, telemetry-free logging), `smollm-engine` (the `Engine` trait
  and `EngineManager`), `smollm-models` (catalog, discovery, download manager),
  `smollm-hardware` (detection and the doctor), `smollm-server` (OpenAI-compatible
  HTTP layer), `smollm-cli` (the `smollm` binary), and
  `smollm-studio-desktop` (Tauri v2).
- **Engine abstraction with a feature-gated backend** — `mock` (default, always
  available), `llama-cpp`, and `candle`. The inference adapters are clearly
  marked seams: selecting a native backend returns
  `unsupported_backend` rather than pretending to work.
- **Eight pages**: Home, Models, Library, Chat, Server, Benchmarks, Logs,
  Settings — wired to 29 Tauri commands and 11 event channels (chat streaming,
  download progress, server state, benchmark progress).
- **Download manager** with resume, SHA-256 verification where the catalog
  provides it, cancellation, and live progress events.
- **Curated catalog** of 11 small models (0.36B–4B, Q4-class quantisations),
  every entry verified against its Hugging Face repository, plus a
  `catalog.local.json` overlay for your own entries.
- **Hardware doctor**: RAM/disk headroom rules, parameter-count guardrails, and
  backend selection (metal → cuda → vulkan → cpu) from real system data.
- **OpenAI-compatible server** on axum: `/v1/chat/completions`, `/v1/completions`,
  `/v1/models`, `/v1/engine/metrics`, `/health`, permissive CORS, and SSE
  termination with `data: [DONE]`.
- **CLI twin** (`smollm`) covering model listing/discovery, download, chat,
  serve, and hardware reporting.
- **Light-first UI**: light is how the app opens, dark is a fully tuned
  alternate, following the system setting is available. Accent palette in
  indigo/cyan, tokens as HSL Tailwind variables.
- **App icon pipeline** driven by a single committed master at
  `assets/brand/logo.png`, regenerating PNGs, `.ico`, and `.icns` with
  `python3 scripts/render_icons.py`.
- **Docs**: `README.md` plus `docs/getting-started.md`, `docs/models.md`,
  `docs/hardware.md`, `docs/api.md`, and `docs/troubleshooting.md`.
- **CI** (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and
  frontend `typecheck`/`lint`/`build`) and a release workflow that builds
  macOS and Windows bundles.

### Known limitations

- Real token generation is not implemented. With the default `mock` engine,
  chat replies are simulated and benchmark numbers are synthetic. This is the
  single most important thing to read before relying on the app; the reason and
  the path forward are in the README's "what is real, and what is a seam".
- Model *downloads* and GGUF *metadata* are real, but downloaded weights are
  never loaded into an engine in this version.
- Linux is not a supported target yet (macOS and Windows only).

[0.1.0]: https://github.com/smollm-studio/smollm-studio/releases/tag/v0.1.0
