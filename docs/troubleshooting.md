# Troubleshooting

Every failure in this app has a stable machine-readable code. The Logs page shows
the technical text; the toast shows one friendly sentence. Start there —
**Logs** with the *Problems only* switch on usually answers the question before
you have to ask it.

## The output is nonsense

Expected in a default build, and not a bug: it has no inference library, so
`mock` streams plausible text to exercise the whole pipeline. Every place that
shows it labels it **simulated** — the top-bar pill, the chat stats, the load
toast, the Server page, the benchmark results.

If you built with `--features llama-cpp` and still see that label, the feature did
not reach the binary you are running: `pnpm tauri dev --features llama-cpp` for
the window, `cargo build -p smollm-cli --features llama-cpp` for the CLI, and
`smollm hardware` should then print an `Offload` line naming a device. Real
answers also need a model that is actually on disk — a simulated engine never
reads weights. See
[models.md — backends](models.md#backends).

## `model_not_downloaded`

The model is named but not on disk. Download it from the Models page, or check
that the model folder in **Settings → Models and engine** still points where your
files are. Typing a new path there does not move existing files — **Move** on that
row does, and reports what it relocated. A file the new folder already holds by
name stays in both places rather than being overwritten, so a Library that looks
empty after a move may be holding the leftover list in the note under the field.

## `model_not_found`

A typo, or a catalog id that a `catalog.local.json` overlay removed. `smollm
models list` prints what the app can currently see; the Library page shows files
that exist without a catalog entry.

## `download_failed`

Network, a revoked Hugging Face revision, or a moved file. Transient breakage —
a dropped connection, a timeout, `429`, `5xx` — is retried automatically up to
four times, so a row that says *retrying (attempt 2 of 4)* needs nothing from
you. A terminal failure keeps its `.part` file, so **Retry** in the Transfers
list resumes from the last byte rather than starting over.

If it fails at exactly the same offset twice, the remote file has probably
changed; the app discards the mismatched partial by itself when the recorded
`ETag` or URL differs, so a plain retry is usually enough.

A message about *consent* or an *Accept licence* means the repo is gated: sign in
on huggingface.co, accept the model licence, and retry. A `404` means the
filename or revision in the catalog no longer exists upstream.

## `download_cancelled`

You pressed Cancel. Not an error; the partial file is kept deliberately.

## `insufficient_disk_space`

The download checks free space plus 64 MB of headroom before writing. Free up
space, or point the model folder at a larger volume in Settings. A move to
another volume checks free space the same way before it copies, and the source
file is kept when the copy cannot fit.

## `invalid_request` from a move

Two states make a model folder move unsafe, and both are refused rather than
renamed out from under the work: a model that is loaded, because the engine holds
that file open — unload it first — and a download that is still running, because
it would resume against a folder that no longer has its bytes — wait for it or
cancel it. Anything the move could not finish is named in the report afterwards,
and neither folder lost a file to get there.

## `insufficient_memory`

The estimate said no. For a model already on disk the number comes from its own
header — the bytes the tensor section holds plus a KV cache sized by the model's
real attention geometry — so lowering the context length changes it immediately.
Cheapest fixes in order: lower the context length (the KV cache dominates), pick a
smaller quantisation, close browsers, then pick a smaller model.
[hardware.md](hardware.md) has the arithmetic.

## `engine_load_failed`

Usually a truncated or non-GGUF file. The Library page shows the parse error from
the header read; `gguf_parse` means the file is not a readable GGUF at all.
Re-download rather than trusting the file.

## `unsupported_backend`

Some engine was asked to do what this binary cannot. The `candle` cargo feature
compiles an adapter that answers with this note until a runner is linked into it,
and the GGUF metadata reader refuses to generate at all. llama.cpp is the one
adapter that is complete: build with `--features llama-cpp` and it loads GGUF
files for real.

Nothing picks such an engine for you. Requesting Metal or CUDA on a build that
cannot run it resolves to MockEngine and adds a warning saying which engine
answered, and `smollm run --engine llama-cpp` prints the same admission as a
warning line rather than failing. So this error reaching you means an adapter was
constructed deliberately, not chosen by the app.

## `server_already_running` / `server_not_running`

The server lives inside the app process: no daemon, no autostart. Stopping the
app stops the API. `server_already_running` means the port is taken, most often
by the same app after an unclean quit — change the port on the Server page.

## The server refuses to start on 0.0.0.0

By design. Only loopback addresses are accepted, because there is no
authentication. To reach it from another machine, use an SSH tunnel:

```bash
ssh -N -L 8080:127.0.0.1:8080 user@this-machine
```

## `config_error`

`settings.json` is hand-editable and therefore hand-breakable. The app falls back
to defaults and reports the problem rather than refusing to start. **Reset app
data** in Settings clears settings and the log buffer; it will not delete models.

## The window is blank

The frontend is served from `dist/` in a release build and from
`http://localhost:1420` in development. A blank window in `pnpm tauri dev` nearly
always means Vite exited — check the terminal that launched it.

## Getting useful diagnostics

**Settings → Data and privacy → Export diagnostics** writes hardware, effective
settings and the tail of the log buffer into the logs folder. Attach that file to
a bug report; it contains paths and no personal data beyond your usernames
appearing inside those paths.

## Building from source fails

| Symptom | Cause |
| --- | --- |
| `failed to get tauri icon` / bundle errors | Missing `desktop/src-tauri/icons`; run `python3 scripts/render_icons.py` |
| `pkg-config` / glib errors on Linux | Tauri Linux prerequisites; the CLI needs none of them |
| `linker not found` | Install the platform C toolchain (`xcode-select --install`, or Build Tools for Visual Studio) |
| `pnpm: command not found` inside `tauri dev` | pnpm is not on the PATH the Tauri child shell sees; `corepack enable` fixes it |
