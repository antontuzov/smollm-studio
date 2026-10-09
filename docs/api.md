# API

Two surfaces: an OpenAI-compatible HTTP server for other programs, and the Tauri
command/event bridge for the UI. Both sit on the same engine, so a request that
works over HTTP works in the chat, and vice versa.

---

## HTTP server

Loopback only. No key is required or checked, which is safe precisely because
`0.0.0.0` is refused. Started from the Server page or `smollm serve`.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/health` | Liveness, engine name, resident model, request count, served model ids |
| GET | `/v1/models` | OpenAI model list: the resident model plus every downloaded model |
| GET | `/v1/models/{model_id}` | One model card |
| POST | `/v1/chat/completions` | Chat, streamed or aggregated |
| POST | `/v1/completions` | Legacy text completion |
| GET | `/v1/engine/metrics` | `EngineMetrics` for the running engine |

CORS is permissive on purpose: the server is local, and the clients that need it
are browsers, notebooks and SDKs on the same machine.

### `POST /v1/chat/completions`

```json
{
  "model": "qwen2.5-0.5b-instruct-gguf",
  "messages": [{ "role": "user", "content": "Hello" }],
  "temperature": 0.7,
  "top_p": 0.95,
  "max_tokens": 256,
  "stop": ["###"],
  "stream": true,
  "seed": 42,
  "presence_penalty": 0,
  "frequency_penalty": 0
}
```

Accepted and ignored where the backend cannot honour them yet: `n`, `user`,
`max_completion_tokens` (read as `max_tokens`), a top-level `system` string, and
`echo`/`suffix` on `/v1/completions`. A `prompt` array is served one item at a
time. This keeps official SDKs happy without faking features.

`messages[].content` accepts all three forms OpenAI allows: a plain string, `null`
(an assistant turn that only carried tool calls), and the list of typed parts SDKs
send for multimodal input. Text parts concatenate in order; image and audio parts
are dropped, because every engine behind this server generates from text.

If `model` is omitted, `""`, `"default"` or `"auto"`, the request goes to the model
resident in the engine. Nothing is loaded on demand: if no model is resident the
request fails with a 404 that says so, rather than quietly starting a download you
did not ask for.

### Streaming

`stream: true` returns `text/event-stream`: standard `chat.completion.chunk`
deltas, `finish_reason` on the final content chunk, then `data: [DONE]`.
With `stream_options: {"include_usage": true}` one more chunk lands between the
last content chunk and `[DONE]`: its `choices` is `[]` and it carries `usage`.
Content chunks never carry `usage`, so a strict client sees exactly the shape it
asked for. Responses set `Cache-Control: no-cache, no-transform` and
`x-accel-buffering: no`; with curl, `-N` keeps the client from buffering too.
Cancellation is honoured — closing the HTTP connection stops generation rather
than letting it run to completion.

### Errors

OpenAI's envelope, with the app's stable code carried in `code`:

```json
{
  "error": {
    "message": "Model 'qwen3-0.6b' has not been downloaded",
    "type": "not_found_error",
    "param": null,
    "code": "model_not_downloaded"
  }
}
```

| HTTP | `type` | Typical `code` |
| --- | --- | --- |
| 400 | `invalid_request_error` | `invalid_request` |
| 404 | `not_found_error` | `model_not_found`, `model_not_downloaded` |
| 501 | `model_not_supported` | `unsupported_backend`, `not_implemented` |
| 503 | `server_error` | `server_not_running` |
| 500 | `internal_server_error` | `internal_error`, `engine_load_failed` |

A body that is not valid JSON gets the same envelope at 400 with
`"param": "body"` — it is parsed by hand, so no client ever has to read Axum's
plain-text 422.

### Worked example

```bash
curl -N http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen2.5-0.5b-instruct-gguf",
       "messages":[{"role":"user","content":"One line about local models."}],
       "stream":true}'
```

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8080/v1", api_key="unused")
for chunk in client.chat.completions.create(
    model="qwen2.5-0.5b-instruct-gguf",
    messages=[{"role": "user", "content": "One line about local models."}],
    stream=True,
):
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

The Server page generates both snippets from the live host, port and model, so
they are never stale.

---

## Tauri commands

44 commands, all `async` but for `new_chat_session` (which mints an identity and
does no I/O); filesystem and `sysinfo` work moves to the blocking
pool so the main thread never stalls. Arguments are camelCase in JavaScript and
snake_case in Rust. Every rejection serialises to `{ code, message, detail }`.

| Area | Commands |
| --- | --- |
| Machine | `detect_hardware`, `get_app_info`, `get_doctor_report` |
| Models | `list_catalog_models`, `catalog_facets`, `list_local_models`, `pull_model`, `cancel_download`, `retry_download`, `get_download_snapshot`, `delete_local_model`, `import_model`, `verify_local_models` |
| Engine | `load_model`, `unload_model`, `get_engine_metrics`, `get_presets` |
| Chat | `start_chat_stream`, `stop_generation` |
| Conversations | `new_chat_session`, `save_chat_session`, `list_chat_sessions`, `search_chat_sessions`, `get_chat_session`, `rename_chat_session`, `delete_chat_session`, `export_chat_session` |
| Server | `start_server`, `stop_server`, `get_server_status`, `get_server_examples` |
| Benchmarks | `run_benchmark` |
| Logs | `get_logs`, `clear_logs` |
| Settings | `get_settings`, `save_settings`, `set_model_dir`, `reset_app_data`, `export_diagnostics` |
| Credentials | `get_hf_token_status`, `set_hf_token`, `clear_hf_token` |
| Files | `open_model_folder`, `open_log_folder` |

`list_catalog_models` takes the whole filter bar as one `filters` object
(`query`, `sort`, `minParametersB`, `maxParametersB`, `quantization`, `tag`,
`license`, `architecture`, `hidePlaceholders`) plus `downloaded`, which is
`true`, `false` or `null` for "both". Empty strings mean "any", not "match
nothing". Each entry is also enriched with `downloaded`, `estimatedRamGb`,
`fitsMemory`, `downloading`, `downloadPercent` and the newest transfer's
`downloadState` plus `downloadError` (`null` when the model was never pulled) —
which is how the Models page can put a Retry button on the card itself instead of
sending you to the Transfers list. For a model that is already on disk
`estimatedRamGb` is measured from that file's header (its tensor bytes plus a KV
cache sized by its own attention geometry); before a download there is nothing to
read, so it falls back to the size heuristic. `catalog_facets` returns the distinct values behind those dropdowns,
with counts, derived from the loaded catalog including any local overlay.

`start_chat_stream` loads the model on demand, so the chat page can send to a
model that was never explicitly loaded. `run_benchmark` uses its own engine
instance so it never competes with the chat; on failure it reports through
`chat-error` with `requestId: "benchmark"`.

Conversations are one JSON file per chat under `<data dir>/sessions/<id>.json`,
written to a temporary name and renamed, so an interrupted write cannot leave a
half transcript. Rust mints the id — `new_chat_session` returns an identity
without touching the disk, and `save_chat_session` stamps `updatedAtMs` and titles
an untitled chat after its first question before writing. So the frontend can keep
sending the placeholder title, and the file name and the store cannot disagree
about which chat a turn belongs to. `list_chat_sessions` returns a
`SessionIndex`: summaries newest first, each with `turnCount`, `modelId` and a
`preview`, plus an `unreadable` list of file names that failed to parse — a broken
transcript is reported, never silently dropped from the list.
`search_chat_sessions` matches titles and turn text and returns hits with a
`snippet` around the match. `export_chat_session` takes a destination path plus
`markdown` or `json` and returns the path written, so the OS save panel decides
where a conversation goes. Ids are restricted to `[A-Za-z0-9_-]` (64 chars max),
which is also what stops a path from arriving in one.

`import_model` takes one absolute `path` — from the native open panel or a file
dropped on the window — and copies that file into the model folder, returning the
`LocalModel` it produced. It is validation before bytes: the path is
canonicalised, a folder or a missing file is refused, the extension must be
`.gguf`, and the header must parse. The copy is staged under the same
`.gguf.part` name a download uses, then its size is compared and its header
re-read before the rename, so a truncated file cannot enter the library. Nothing
is moved or overwritten: a name already held by a *different* file becomes
`name-2.gguf`, and a file that already lives in the model folder is listed
without a second copy.

`verify_local_models` takes an optional `fileName` and returns one
`ModelVerification` per file: `fileName`, `path`, `ok`, and `checks` — each a
`label`, a `status` of `passed` / `failed` / `skipped`, and a `detail` saying what
was measured. Without a `fileName` every scanned file is checked. The checks are
the ones a GGUF file can answer for itself: the file is readable, its header
parses, the tensor data section is present and reaches at least as far as the
deepest offset the header declares, the weight bytes per parameter land in a range
any quantisation could produce, and — for a file whose name is in the catalog —
the size still matches what Hugging Face publishes. `skipped` is a real state, not
a pass: it means the file gave no number to compare against. GGUF carries no
per-file checksum, so a flipped byte inside tensor data is invisible here; a file
that fails is deleted and downloaded again, not repaired.

`set_model_dir` takes an absolute `modelDir`, relocates what the old model folder
holds into it, persists the new path in `settings.json`, and returns a
`Relocation`: `from`, `to`, `moved` (renamed in place, so no bytes re-written),
`copied` (re-written because the folders are on different volumes), `bytes`, and
three lists that account for every other file — `duplicates` (the destination
already held that name at the same length), `conflicts` (same name, different
length) and `failures` (the reason a file could not move). Nothing is overwritten
and the old folder is never deleted, so anything in those lists is still exactly
where it was. `save_settings` keeps re-pointing without touching a file, which is
why **Move** is its own command rather than a flag on Save: pressing Save should
never copy gigabytes. A move that could break live work is refused with
`invalid_request` and the reason — a model is resident, so its file is open, or a
transfer is still running; a cross-volume copy that would not fit reports
`insufficient_disk_space` instead.

The three credential commands are the only bridge the Hugging Face token has to
the UI, and it is one-directional. `set_hf_token` takes the pasted token, writes it
to the OS credential store, and returns the `TokenStatus` it read back;
`clear_hf_token` removes it; `get_hf_token_status` reports what would be sent.
`TokenStatus` is `{ source, masked, keychain }` — `source` is `keychain`,
`environment` or `none`, `masked` is the first three characters and the last four
(`hf_…wxyz`) or `null`, `keychain` says whether this build can write to a store at
all. The whole token therefore never crosses the bridge in either direction: there
is no command that returns it, so the webview cannot hold, render or copy it, and a
`save_settings` payload cannot carry it either. All three run on the blocking pool
because a keychain lookup waits on a system agent — macOS asks you to allow the
first access an unsigned build makes. Saving a blank field is refused with
`invalid_request` before the store is touched, and on a platform with no store
compiled in, `set_hf_token` fails with `config_error` telling you to set `HF_TOKEN`
rather than writing the token somewhere plain.

`get_server_status` returns `running`, `host`, `port`, `baseUrl`, `engine`,
`loadedModel`, `servedModels`, `requests`, `uptimeSeconds` and `simulated`.
`servedModels` is the exact id list `/v1/models` offers, resident model first,
so the Server page can show what a client would see rather than what the
library happens to contain. `get_server_examples` returns `curl`, `curlStream`
(the same call with `stream: true`, built for `curl -N`), `python`, `health`,
`baseUrl` and the `model` the snippets name — `"default"` until something is
resident, which the server then resolves for you.

## Events

Long operations report through events instead of holding a command open.

| Event | Payload | Emitted |
| --- | --- | --- |
| `chat-token` | `{ requestId, token }` | Every generated token |
| `chat-done` | `{ requestId, text, finishReason, usage, elapsedMs, tokensPerSecond, simulated }` | End of a stream |
| `chat-error` | `{ requestId, code, message, detail }` | Failed stream, including benchmarks |
| `download-progress` | `{ downloadId, modelId, fileName, state, downloadedBytes, totalBytes, percent, bytesPerSecond, attempt, maxAttempts, error }` | Throttled during transfer |
| `download-complete` / `download-error` / `download-cancelled` | task summary | Terminal states |
| `server-log` | `{ timestampMs, level, message }` | Each request the server handles |
| `server-state` | `"started"` or `"stopped"` | The server changed state |
| `server-models-updated` | `()` | The advertised model list changed, e.g. after a download |
| `server-watch-stopped` | `()` | The request-traffic watcher exited |
| `benchmark-progress` | `{ stage, percent, message }` | Load → prompt → generate per run |

`requestId` is the correlation key: the UI ignores tokens from a request it has
already finished or replaced, which is what makes Stop and retry safe.

`state` in a download payload is one of `queued`, `running`, `retrying`,
`verifying`, `complete`, `cancelled`, `failed`. `retrying` means a transient
network error and an automatic second attempt — `error` carries the reason and
`attempt`/`maxAttempts` the count — so it is still active work, not a failure.

---

## Command line

`smollm` is a peer of the desktop app, not a debug tool. It shares the crates, so
the catalog, downloads, engine and server are the same code.

```bash
smollm hardware --json
smollm doctor --json
smollm models list --query qwen --sort fastest
smollm models pull llama-3.2-1b-instruct-gguf
smollm models local --json
smollm models rm llama-3.2-1b-instruct-q4_k_m.gguf
smollm auth status
smollm run qwen2.5-0.5b-instruct-gguf --preset creative --max-tokens 128
smollm serve --port 8123 --model qwen2.5-0.5b-instruct-gguf
smollm bench qwen2.5-0.5b-instruct-gguf --runs 3
```

Global flags: `--models-dir DIR`, `--verbose` (engine, download and HTTP internals
on stderr). Most subcommands accept `--json` for scripting. `smollm serve
--examples-only` prints the client snippets without binding a port.

`auth status` prints the masked token and where it comes from; `auth set` reads the
token from stdin — argument or pipe, never a flag, because an argument survives in
shell history and in `ps` — and `auth clear` removes the stored one. These are the
same two stores the desktop app uses, so a token saved either way works for both.
