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
| GET | `/health` | Liveness, engine name, resident model, request count |
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

If `model` is omitted, the server's default model is used. If nothing is
resident, the first request loads it — which is why the first call is slower.

### Streaming

`stream: true` returns `text/event-stream`: standard `chat.completion.chunk`
deltas, `finish_reason` on the final content chunk, then `data: [DONE]`.
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

30 commands, all `async`; filesystem and `sysinfo` work moves to the blocking
pool so the main thread never stalls. Arguments are camelCase in JavaScript and
snake_case in Rust. Every rejection serialises to `{ code, message, detail }`.

| Area | Commands |
| --- | --- |
| Machine | `detect_hardware`, `get_app_info`, `get_doctor_report` |
| Models | `list_catalog_models`, `catalog_facets`, `list_local_models`, `pull_model`, `cancel_download`, `retry_download`, `get_download_snapshot`, `delete_local_model` |
| Engine | `load_model`, `unload_model`, `get_engine_metrics`, `get_presets` |
| Chat | `start_chat_stream`, `stop_generation` |
| Server | `start_server`, `stop_server`, `get_server_status`, `get_server_examples` |
| Benchmarks | `run_benchmark` |
| Logs | `get_logs`, `clear_logs` |
| Settings | `get_settings`, `save_settings`, `reset_app_data`, `export_diagnostics` |
| Files | `open_model_folder`, `open_log_folder` |

`list_catalog_models` takes the whole filter bar as one `filters` object
(`query`, `sort`, `minParametersB`, `maxParametersB`, `quantization`, `tag`,
`license`, `architecture`, `hidePlaceholders`) plus `downloaded`, which is
`true`, `false` or `null` for "both". Empty strings mean "any", not "match
nothing". Each entry is also enriched with `downloaded`, `estimatedRamGb`,
`fitsMemory`, `downloading`, `downloadPercent` and the newest transfer's
`downloadState` plus `downloadError` (`null` when the model was never pulled) —
which is how the Models page can put a Retry button on the card itself instead of
sending you to the Transfers list. `catalog_facets` returns the distinct values behind those dropdowns,
with counts, derived from the loaded catalog including any local overlay.

`start_chat_stream` loads the model on demand, so the chat page can send to a
model that was never explicitly loaded. `run_benchmark` uses its own engine
instance so it never competes with the chat; on failure it reports through
`chat-error` with `requestId: "benchmark"`.

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
smollm run qwen2.5-0.5b-instruct-gguf --preset creative --max-tokens 128
smollm serve --port 8123 --model qwen2.5-0.5b-instruct-gguf
smollm bench qwen2.5-0.5b-instruct-gguf --runs 3
```

Global flags: `--models-dir DIR`, `--verbose` (engine, download and HTTP internals
on stderr). Most subcommands accept `--json` for scripting. `smollm serve
--examples-only` prints the client snippets without binding a port.
