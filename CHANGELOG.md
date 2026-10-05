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
