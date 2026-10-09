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
- **llama.cpp is a real engine.** `--features llama-cpp` now links llama.cpp
  through the `llama-cpp-2` bindings and answers with actual model output: it
  loads the GGUF, samples with the app's temperature/top-p/top-k/min-p and
  penalty settings, honours stop sequences across token boundaries without
  leaking a marker, and reports tokens per second, prompt and completion counts
  from the library rather than from a script. Measured on an Apple M1 with Metal
  and flash attention: 44–65 tok/s on SmolLM2 360M Q4_K_M. Without the feature
  nothing changes — MockEngine still answers, and still says it is simulated.
- **Generation runs on a decode worker that can be stopped.** llama.cpp blocks
  inside its own call stack for the whole reply, so each generation now owns a
  thread, hands tokens to the async layers over a channel, and checks cancellation
  between tokens: pressing Stop ends the reply within a token, and a client that
  goes away — a closed chat stream, a disconnected `curl` — stops the work instead
  of letting a model write into a vanished socket. The model, backend and context
  are built on that thread, so nothing borrows across it.
- **The model's own tokenizer and chat template.** llama.cpp reads the chat
  template embedded in the file (38 prompt tokens for a two-message chat on
  SmolLM2, counted by the model's vocabulary), so prompts are no longer rendered
  from the app's architecture table, and token counts in the UI are real. The
  table stays as the fallback for engines that ship no template.
- **Memory fit is measured, not guessed.** A GGUF header now yields the bytes its
  tensor section actually holds plus `head_count`/`head_count_kv`, so the RAM
  figure — the load refusal in the app, the pre-run warning in the CLI, the
  *Needs RAM* line on a model card — comes from the file's own attention geometry
  instead of a constant per token. Grouped-query models drop notably: SmolLM2 360M
  at 8K context is 0.9 GB measured against 1.6 GB estimated. A model that is not
  on disk yet still gets the old heuristic, and the Library page shows the weight
  bytes it read.
- **Two questions answered by llama.cpp itself.** Which device the weights really
  landed on (`metrics.backend` now reports `metal` when the Metal backend ran,
  `cpu` when it did not, and logs a name this app does not model instead of
  inventing a match) and how much memory it allows itself there (11.8 GiB of this
  M1's 16 GiB, its own working-set cap), which `smollm hardware` prints as an
  `Offload` line and the Home page shows under *This machine*.
- **llama.cpp logs go through `tracing`.** Its scheduler and graph traces used to
  print hundreds of lines per generation straight to stderr, past the log file and
  past every filter; they are routed into the app's own subscriber now, so
  `RUST_LOG` decides what is shown and the Logs page keeps the rest.
- **Import a model you already own.** A `.gguf` that lives outside the model
  folder used to be unusable — the engine loads from the folder the settings name
  and the Library lists what is inside it. The Library page now has an **Import
  from disk** button (a native multi-select open panel, from the
  `tauri-plugin-dialog` this also added), and a `.gguf` dropped anywhere on the
  window imports too, behind an overlay that says what will happen — which is why
  `dragDropEnabled` is on: with it off, a dropped file made the webview navigate
  away from the app. Both doors call one `import_model` command that canonicalises
  the path, refuses anything whose header does not parse (wrong extension, a
  folder, a file whose bytes are not GGUF), checks free space first, and stages
  the copy under the same `.gguf.part` name a download uses before renaming it
  into place, so an interrupted import is swept by the existing start-up cleanup
  rather than joining the library as a broken model. Nothing is moved or
  overwritten: the source file stays where it was, two *different* files with one
  name become `name.gguf` and `name-2.gguf`, and a file already inside the folder
  is listed instead of copied a second time. `smollm models import <path>` is the
  same code without a window.
- **Conversations survive quitting the app.** A chat used to live only in memory,
  so closing the window deleted it. Each finished answer is now written to
  `<data dir>/sessions/<id>.json` as one file per conversation — JSON rather than
  SQLite, so nothing new has to be installed to read it, written to a temporary
  name and renamed so a crash mid-write cannot leave a half-transcript. A new
  `smollm-core::session` module owns the format and the round trip
  (`save`/`load`/`index`/`search`/`export`), and the Chat page has a
  *Conversations* panel that lists them, reopens one on click, renames it,
  searches titles and answers, exports to Markdown or JSON through the OS save
  panel, and deletes. Saves happen at turn boundaries rather than per token, are
  serialised so two events in the same tick cannot mint two files, and name the
  file after the first question. A transcript with no turns is never written, so
  an abandoned window leaves no orphan. The last conversation reopens on launch,
  a corrupt file is listed as unreadable instead of hiding the rest, *Reset app
  data* reports how many conversations it cleared and keeps the models, and
  nothing leaves the machine.
- **Library files can be re-verified.** A model that downloaded cleanly can still
  go wrong later — a disk that filled mid-copy, a folder moved between volumes, a
  file something else truncated — and until now the Library had no way to notice:
  the scan only reads the header, so a short file looked fine until the engine
  failed to load it. **Verify** on the Library header (or one file's own Verify
  button) runs `verify_local_models`, which answers five questions per file: the
  file is readable, its header parses, weight bytes follow the data section and
  reach at least as far as the deepest offset the header declares, the bytes per
  parameter land in a range a real quantisation occupies, and — for a catalog file
  — the size still matches what Hugging Face publishes. Each answer is *passed*,
  *failed* or *skipped*, and skipped is not a soft pass: it means the file gave no
  number to compare against. To make the truncation check real, the GGUF reader
  now also keeps the largest tensor offset a v3 header records
  (`GgufHeader::min_file_bytes`), which on the bundled 360M model pinpoints the
  last tensor to within 4 KiB of the file's end. The honest limit is documented
  rather than hidden: GGUF carries no per-file checksum, so bit rot inside tensor
  data cannot be seen from outside, and a file that fails is deleted and
  re-downloaded, not repaired. `smollm models verify [FILE] [--json]` is the same
  code, and exits non-zero when a file fails.
- **The model folder can be moved, and takes its files with it.** Editing
  *Model folder* only ever re-pointed the app, so the library stayed behind in a
  folder nothing read any more — the honest description of that click was "my
  models have disappeared". **Move** on the Settings row now relocates the files
  and persists the new path in one step, and **Save** still only re-points, so
  pressing Save cannot copy gigabytes. The move is rename-first: on one volume a
  file is renamed, so no bytes are re-written, and only when the rename fails does
  it copy — free space checked first, bytes into the `.gguf.part` staging name,
  the source deleted only after the copy is proved by length and a GGUF header
  re-read. Nothing is ever overwritten: a name the target already holds stays in
  both folders and is reported as a duplicate (equal length) or a conflict
  (different length), the old folder is never deleted, and anything that could not
  move is listed in the note afterwards, because `settings.json` alone cannot say
  where the models are. A paused download's partial file moves with the folder it
  belongs to; files that are not models are left alone. A move is refused while a
  model is loaded — the engine has that file open — or while a transfer is running.
  `smollm models move DIR [--json]` is the same `relocate_from`, and is the one CLI
  command that writes `settings.json` because the files really did move; it exits
  non-zero when a file was left behind.
- **A Hugging Face token for gated repos, kept in the OS credential store.** Llama's
  own repositories and some of Google's answer `401` until their licence has been
  accepted from an account that can see them, and this app had nowhere to put the
  token that proves the account: gated entries simply failed. A new
  `smollm-core::secrets` module reads and writes one entry — service *SmolLLM
  Studio*, account *huggingface* — in the macOS Keychain or the Windows Credential
  Manager, and holds the value in a `Secret` that prints its own mask from both
  `Debug` and `Display`, so a token cannot escape into a log line by accident: the
  mask keeps three characters from the head and four from the tail, and goes to all
  stars below twelve, where the ends would be most of the value. The plaintext never
  enters `settings.json`, no command returns it, and the bearer header is attached
  only after the request's host is compared with the configured endpoint's, so the
  CDN a download redirects to is served without it. **Settings → Hugging Face
  access** shows where the token came from and saves or removes one; `smollm auth
  status|set|clear` is the same three actions, and `set` reads from stdin so a token
  never lands in shell history or in `ps`. On Linux, and in CI, the crate compiles
  with no credential backend — a marked TODO adapter — and `HF_TOKEN` is read
  instead. A token survives a model-folder move because the rebuilt download engine
  clones the same client rather than re-reading the store; *Reset app data*
  deliberately does not clear it, since that entry belongs to the OS, not to the
  data folder.

### Fixed

- **Gemma files are readable, so their downloads survive.** The GGUF reader
  capped array lengths at 260,000 on the reasoning that no vocabulary is
  bigger, and Gemma 3's is 262,144 — both its `tokenizer.ggml.tokens` and its
  `scores` array. The header check therefore failed on a file that is perfectly
  fine, which discarded a completed 720 MB download as "not a readable GGUF
  model" and left both Gemma catalog entries unusable. The cap is 1,048,576
  now: it exists to stop a corrupt count making the reader loop, not to hold
  items, since vocabulary arrays already collapse to a truncation marker past
  their first 512 entries. Pinned by a test that reads a 262,144-element array,
  and checked against the real `gemma-3-1b-it-qat-Q4_0.gguf` header (version 3,
  340 tensors, 39 metadata keys) on 2026-10-09.
- **Downloads work again, after two faults that hid each other.**
  `HfClient::resolve_url` put `resolve` before the repository —
  `https://huggingface.co/resolve/{repo}/…`, which is not a path the hub serves —
  while the fake Hugging Face in the transfer tests answered *any* path, so the
  suite stayed green around a URL that could never fetch. Then Hugging Face
  stopped serving its metadata endpoint (`/api/models/{repo}/resolve/{revision}/`
  `{file}` now answers `404` for files it still serves), and `probe` reported that
  as "the catalog entry may be out of date", refusing the download before it asked
  for a byte. The client builds `{repo}/resolve/{revision}/{filename}` now, the
  shape `ModelDescriptor::download_url` has always documented; a probe that says
  nothing useful falls back to a HEAD of the file itself, while `401` and `403`
  still mean an unaccepted licence, since that is the one thing only the API can
  say; and the fake server answers only the path it really serves, so the URL is
  pinned by a unit test and by every transfer test. A 271 MB
  `SmolLM2-360M-Instruct-Q4_K_M.gguf` was fetched from Hugging Face and passed all
  five integrity checks on 2026-10-09.
- **Quant labels are the quant the file actually holds.** `general.file_type` is
  llama.cpp's `llama_ftype` *enum*, and that enum keeps holes where formats were
  removed (4–6, 33–35). The table here was a dense list, so every name from
  index 2 on sat two to six slots early: the bundled `SmolLM2-360M-Instruct-Q4_K_M`
  (`file_type = 15`) was reported as **Q5_K_M**, and `BF16` — which is 32 — was
  claimed by index 2. The table is now positional, with the removed values left
  empty so an unknown index answers "unknown" instead of guessing. Only `F32`,
  `F16` and `Q8_0` used to be labelled correctly.
- **GGUF files that store tensor dimensions as `uint64` are readable again.**
  The spec writes them as `uint32` in v3, but real v3 exports have shipped the
  wider width anyway; the reader followed the version, diverged on the first
  tensor and reported no parameter count and no weight size — silently, because
  tensor infos were best effort. It now tries the width the version implies, then
  the other, and treats a zero rank or a zero-length dimension as the divergence
  it is. `SmolLM2-360M-Instruct-Q4_K_M.gguf` reports 361,821,120 parameters and
  268,803,840 weight bytes, which is what llama.cpp's own tensor offsets say.
- **A chat that auto-loads a model no longer blocks a worker thread.** The
  native engine reads hundreds of megabytes and builds GPU kernels, which is
  minutes of work on a CPU-bound pool if it happens inside an async command. It
  goes to `spawn_blocking` now, like every other native call.
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
