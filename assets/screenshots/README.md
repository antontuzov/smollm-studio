# Screenshots

The README links five captures that are **not committed** to this repository —
`assets/screenshots/*.png` is gitignored so the repo stays clone-friendly.
Drop these files here and the README image tags resolve as-is.

| File | Page | What should be visible |
| --- | --- | --- |
| `home.png` | Home | Hardware summary, the recommended model, and the download queue idle |
| `models.png` | Models | The catalog grid with at least one entry marked downloaded |
| `library.png` | Library | One or more GGUF files on disk with size and context |
| `chat.png` | Chat | A streamed assistant reply mid-generation, token counter running |
| `server.png` | Server | Server running on `127.0.0.1:8080` with the `curl` example |

## How to capture

Run the app (`cd desktop && pnpm tauri dev`), open the page, then use the system
screenshot tool. The default window is 1280×820 (minimum 1024×640), which reads
well at README width.

To match the README's light-first look, leave the theme on **Light**. If you want
to show the alternate theme, capture both and name the dark one with a `-dark`
suffix (e.g. `chat-dark.png`) and update the README accordingly.

Chat and server captures need a model, and with the default `mock` engine the
reply is simulated — that's fine for a screenshot as long as the README's
"what is real, and what is a seam" section stays above the fold.

## Style rules for these

- No visible user paths, usernames, or HF tokens in the shot.
- Don't crop out the sidebar — the brand mark and nav are part of the design.
- Keep the native title bar; this is a desktop app, not a web page.
