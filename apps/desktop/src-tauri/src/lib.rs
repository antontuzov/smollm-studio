//! FreeCode's Tauri host.
//!
//! The crate's only jobs are to expose the Rust side to React and to keep that surface honest: the
//! UI never computes a route or a refusal — it asks, and this host answers from `freecode-config`
//! and the free-model registry. Commands are `async` so the blocking probes run on the async runtime
//! instead of the event-loop thread.

mod agent;
mod appearance;
mod approve;
mod brand;
mod chat;
mod clipboard;
mod diagnostics;
mod extensions;
mod free_models;
mod github;
mod instructions;
mod opencode;
mod paths;
mod preview;
mod provider;
mod reveal;
mod sessions;
mod shell;
mod status;
mod storage;
mod undo;
mod update;

use tauri::{AppHandle, Manager, Theme};
use tauri_plugin_dialog::DialogExt;

use appearance::AppearanceView;
use freecode_config::Settings;
use status::ModelStatusView;

/// `System` asks the webview; a fixed Light/Dark is honoured even if the OS disagrees.
fn os_prefers_light(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.theme().ok())
        .is_some_and(|theme| matches!(theme, Theme::Light))
}

#[tauri::command]
async fn model_status() -> Result<ModelStatusView, String> {
    let settings = appearance::load_settings()?;
    let mut view = status::build(&settings);
    // The local OpenCode service is the one thing on this card that is neither in the settings nor in
    // the registry: it is a process, so it is asked, and the answer rides on the status the window
    // already polls. A probe that fails is "not running", which is what the card says.
    let service = opencode::status().await;
    view.opencode = status::OpenCodeView {
        available: service.available,
        version: service.version,
    };
    Ok(view)
}

/// The Extensions page: the tool registry as the agent is handed it, with the current approval dial
/// and sandbox applied to every row by the same engine that decides a call mid-run.
#[tauri::command]
async fn extensions_inventory(app: AppHandle) -> Result<extensions::Inventory, String> {
    let settings = appearance::load_settings()?;
    // The folder a run's file calls are confined to, which is the project picked in Settings and not
    // necessarily the directory this host keeps its own data in.
    let root = paths::project_root(Some(&app), &settings)?;
    tauri::async_runtime::spawn_blocking(move || extensions::Inventory::build(&root, &settings))
        .await
        .map_err(|e| format!("tool inventory failed: {e}"))
}

/// The Storage page: the conversations this host has filed, measured from the files on disk.
#[tauri::command]
async fn storage_report(app: AppHandle) -> Result<storage::Report, String> {
    let root = paths::workspace_root(Some(&app))?;
    tauri::async_runtime::spawn_blocking(move || storage::Report::build(&root))
        .await
        .map_err(|e| format!("storage report failed: {e}"))
}

/// The Diagnostics page: which machine this is and which build is running on it.
#[tauri::command]
async fn diagnostics_report(app: AppHandle) -> Result<diagnostics::Report, String> {
    let root = paths::workspace_root(Some(&app))?;
    let version = app.package_info().version.to_string();
    Ok(diagnostics::Report::build(&root, &version))
}

#[tauri::command]
async fn load_appearance(app: AppHandle) -> Result<AppearanceView, String> {
    let settings = appearance::load_settings()?;
    Ok(appearance::view(
        &settings.appearance,
        os_prefers_light(&app),
    ))
}

/// Persists the shell's controls and echoes what the host now believes, so the UI never keeps a
/// stale `resolvedMode`. `resolvedMode` is host-computed and is not read from the payload.
#[tauri::command]
async fn save_appearance(
    app: AppHandle,
    appearance: AppearanceView,
) -> Result<AppearanceView, String> {
    let mut settings = crate::appearance::load_settings()?;
    crate::appearance::apply(&appearance, &mut settings.appearance);
    crate::appearance::save_settings(&settings)?;
    wear_icon(&app, settings.appearance.app_icon);
    Ok(crate::appearance::view(
        &settings.appearance,
        os_prefers_light(&app),
    ))
}

/// The whole settings document, for the Settings surface. Deserialising on the way out is the
/// validation: a payload the schema rejects never reaches the file.
#[tauri::command]
async fn load_settings() -> Result<Settings, String> {
    appearance::load_settings()
}

/// The Settings surface edits the loaded document in place and hands it back, so a field it does
/// not render still round-trips untouched. The host answers with what it stored.
#[tauri::command]
async fn save_settings(app: AppHandle, mut settings: Settings) -> Result<Settings, String> {
    // The page saves on every keystroke, so the address and model are stored as typed; the full
    // HTTPS/loopback rule is applied when a turn is resolved, before a request exists. The one check
    // that cannot wait is embedded credentials: a key pasted into the address field must never reach
    // `settings.json`. `remote_turn` re-checks everything, so a hand-edited file gets no further.
    let remote = &mut settings.remote_provider;
    remote.base_url = remote.base_url.trim().to_string();
    remote.model = remote.model.trim().to_string();
    provider::reject_embedded_credentials(&remote.base_url)?;
    // The picker clears its selection with an empty string; an empty pick is the same state as no
    // pick, so the document never files a `""` id the chat path would have to refuse later.
    if settings.free_model.as_deref() == Some("") {
        settings.free_model = None;
    }
    settings.schema_version = freecode_config::SETTINGS_SCHEMA_VERSION;
    appearance::save_settings(&settings)?;
    wear_icon(&app, settings.appearance.app_icon);
    Ok(settings)
}

/// Open the desktop's folder chooser and hand back the folder the user picked, or nothing when they
/// cancel. The dialog runs here rather than in the webview, so the page is handed a path and still
/// holds no filesystem capability of its own. The answer is not stored here: the settings document
/// belongs to the page, and a write from this side would race the copy the user is editing.
#[tauri::command]
async fn choose_project_folder(app: AppHandle, title: String) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title(title)
            .set_can_create_directories(false)
            .blocking_pick_folder()
    })
    .await
    .map_err(|e| format!("the folder chooser stopped: {e}"))?;
    Ok(picked
        .and_then(|folder| folder.into_path().ok())
        .map(|path| path.display().to_string()))
}

/// Whether the Keychain holds a remote-provider key. The key itself never crosses this boundary.
#[tauri::command]
async fn remote_key_present() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(provider::key_saved)
        .await
        .map_err(|e| format!("the keychain check stopped: {e}"))
}

/// Store a pasted key in the login Keychain. The answer is the new "a key is saved" state; the
/// value that was handed over is not echoed back and is not written to the settings document.
#[tauri::command]
async fn save_remote_key(key: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || provider::save_key(&key))
        .await
        .map_err(|e| format!("the keychain write stopped: {e}"))??;
    Ok(true)
}

/// Remove the saved key. Answering `false` is the state the UI draws, not an error: there was
/// nothing to remove, or the credential is gone now either way.
#[tauri::command]
async fn clear_remote_key() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(provider::clear_key)
        .await
        .map_err(|e| format!("the keychain write stopped: {e}"))??;
    Ok(false)
}

/// Whether the Keychain holds a GitHub token. The token itself never crosses this boundary.
#[tauri::command]
async fn github_token_present() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(github::token_saved)
        .await
        .map_err(|e| format!("the keychain check stopped: {e}"))
}

/// Store a pasted GitHub token in the login Keychain. The answer is the new "a token is saved"
/// state; the value that was handed over is not echoed back and is not written to the settings
/// document.
#[tauri::command]
async fn save_github_token(token: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || github::save_token(&token))
        .await
        .map_err(|e| format!("the keychain write stopped: {e}"))??;
    Ok(true)
}

/// Remove the saved GitHub token. Answering `false` is the state the UI draws, not an error: there
/// was nothing to remove, or the credential is gone now either way.
#[tauri::command]
async fn clear_github_token() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(github::clear_token)
        .await
        .map_err(|e| format!("the keychain write stopped: {e}"))??;
    Ok(false)
}

/// The free-model registry as the picker draws it. No secret rides along, because no route here uses
/// one: the list carries ids, labels and whether the FreeCode app on this Mac is answering, which is
/// the only way any of these models can be asked.
#[tauri::command]
async fn free_models_list() -> Result<Vec<free_models::FreeModelInfo>, String> {
    // Asked before the list is drawn so the rows a first download shows are the state the turn will
    // find: a service that is not up yet is started here rather than after the user has picked.
    let local = opencode::ensure().await.is_some();
    Ok(free_models::infos(local))
}

/// Whether the OpenCode app's local service answers, and which version. The status card and the
/// picker both name it, because a free model answers through this service.
#[tauri::command]
async fn opencode_status() -> Result<opencode::Status, String> {
    Ok(opencode::status().await)
}

/// Move the app's face to the tile the document names. The pin is host state because the Dock is
/// host-drawn, so a save from either settings surface has to hand it over and repaint: without the
/// second half the window shows the duck the user picked while its own tile stays the one it launched
/// with. Clearing the pin hands this launch's roll back rather than choosing a new one.
fn wear_icon(app: &AppHandle, pinned: Option<u8>) {
    brand::prefer(pinned.map(usize::from));
    brand::set_dock_tile(app);
}

/// SIGTERM, SIGINT and SIGHUP never reach [`RunEvent::Exit`]: their default action stops the process
/// where it stands, which would leave the terminal's shell running after the window is gone.
/// Registering a listener takes those signals over, so the shell dies first and the process then
/// exits with the status the signal would have produced anyway.
///
/// `std::process::exit` rather than `AppHandle::exit`: a graceful teardown is precisely the thing that
/// might not return, and the one promise this path makes is that the child does not outlive the host.
#[cfg(unix)]
fn watch_signals(app: AppHandle) {
    use tokio::signal::unix::{signal, SignalKind};
    for kind in [
        SignalKind::terminate(),
        SignalKind::interrupt(),
        SignalKind::hangup(),
    ] {
        let app = app.clone();
        let status = 128 + kind.as_raw_value();
        // Registration needs the reactor, so it happens inside the task rather than in `setup`.
        tauri::async_runtime::spawn(async move {
            let Ok(mut stream) = signal(kind) else { return };
            if stream.recv().await.is_some() {
                shell::shutdown(&app);
                std::process::exit(status);
            }
        });
    }
}

/// On Windows the equivalent is a Job Object; there is no signal to catch. The app-exit hook covers
/// the paths that exist there.
#[cfg(not(unix))]
fn watch_signals(_: AppHandle) {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Read before the builder below asks for the mascot's number: a user who pinned a tile should
    // open the app wearing it, on the Dock and in the window both, from the first frame. Nothing
    // here is fatal — a document that will not parse leaves the pin unset, which is the roll that
    // shipped before there was a picker, and the same read reports the failure later.
    let pinned = appearance::load_settings()
        .ok()
        .and_then(|settings| settings.appearance.app_icon)
        .map(usize::from);
    brand::prefer(pinned);

    let app = tauri::Builder::default()
        // The conversation the next request is built from, and the one-answer-at-a-time flag.
        .manage(chat::ChatState::default())
        // The panel's own shell, kept across lines so `cd` and an exported variable survive from one
        // command to the next. One shell: the panel has one prompt, and a second one would be a
        // process nobody can see. See `shell`.
        .manage(shell::ShellState::default())
        // The page calls waiting for the window's browser panel to answer. A tool thread parks on its
        // own channel; `preview::page_reply` is what sends. See `preview`.
        .manage(preview::PageBridge::default())
        // The calls waiting for the window to answer their approval prompt. A tool thread parks on its
        // own channel; `approve::resolve_approval` is what sends. See `approve`.
        .manage(approve::ApprovalBridge::default())
        // One process owns the engine, the conversation and the saved key, so a second launch hands
        // over and exits; the window already open comes forward instead. This plugin has to be
        // registered before the others — it runs their setup only after claiming the instance.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        // The one way this app leaves the machine for the user's browser: refusal links and the links
        // in an answer. The UI cannot reach it directly — the capability scopes it to `http` and
        // `https`, so a `file://` or a `javascript:` address in a model's own text never launches.
        .plugin(tauri_plugin_opener::init())
        // The project folder is picked with the platform's own chooser. Registered as a Rust plugin,
        // so no dialog permission is handed to the webview: the only caller is the host command.
        .plugin(tauri_plugin_dialog::init())
        // The in-app browser: `preview://localhost/…` serves the project folder the run worked in,
        // confined by the same sandbox as every other read of it. See `preview`.
        .register_uri_scheme_protocol(preview::SCHEME, preview::handle)
        // The duck this launch wears, handed to the page before any of its own scripts run, so the
        // window and the Dock tile are the same picture from the first frame rather than a moment
        // apart. See `brand`.
        .append_invoke_initialization_script(brand::init_script())
        .invoke_handler(tauri::generate_handler![
            model_status,
            // What the agent can reach and what this build runs, for the Extensions page.
            extensions_inventory,
            // What this app has put on the machine.
            storage_report,
            // Which machine that is, and which build is running on it.
            diagnostics_report,
            // Whether a newer build of the app has been published. Asked for, never polled.
            update::check_for_update,
            load_appearance,
            save_appearance,
            load_settings,
            save_settings,
            // The project folder, chosen with the desktop's own folder dialog. It answers with the
            // path and stores nothing; the Settings surface writes the document it is editing.
            choose_project_folder,
            // The remote provider's one secret, held by the macOS Keychain rather than the settings
            // document. The page can ask whether a key is there, replace it, or remove it — never
            // read it back.
            remote_key_present,
            save_remote_key,
            clear_remote_key,
            // The GitHub token, for the one API call the agent makes: opening a pull request. It
            // lives in the Keychain like the provider's key, and the push needs none — git uses the
            // credential this machine already holds for the remote.
            github_token_present,
            save_github_token,
            clear_github_token,
            // The free tier's registry. It carries no secret and asks for none: the rows are answered
            // by the local service below, and nothing here reads a keychain.
            free_models_list,
            // The OpenCode app's local service: whether it is running, so the picker can say which
            // free models can answer. The password in its state file never leaves the host.
            opencode_status,
            // The one write the UI may make to the machine it runs on: the text of a code fence, onto
            // the pasteboard. See `clipboard`.
            clipboard::copy_text,
            // Handing a file this run changed back to the desktop's default handler. Confined to the
            // workspace by the same sandbox a `read_file` goes through — see `reveal`.
            reveal::open_file,
            // The way out for a page on the web: the panel's own control hands the address to the
            // desktop's browser, and the host lets only `http` and `https` through — see `reveal`.
            reveal::open_url,
            // The same door for the folder itself, which is the palette's "Open workspace files".
            // It takes no path from the window: the root settings names is the only one it can show.
            reveal::reveal_workspace,
            // The in-app address of one of the run's pages, which the review's open control frames
            // instead of leaving the app — see `preview`.
            preview::preview_url,
            // The page a blank browser tab starts on, in the same scheme and under the same policy.
            preview::preview_entry,
            // The window's answer to one page operation the run asked for — see `preview`.
            preview::page_reply,
            // The window's answer to one approval prompt: the press, sent to the call that asked.
            approve::resolve_approval,
            // What the page draws, as a PNG kept in the app's own folder. The window sends the panel's
            // rectangle because only the layout knows where the frame ended up.
            preview::page_capture,
            // The same picture, measured against one assertion instead of only kept: the answer a
            // run checks a page with when no model is looking at it.
            preview::page_check,
            // The development check behind `?pagecheck=1`; it refuses outside a probe launch.
            preview::agent_page_probe,
            chat::chat_send,
            chat::chat_stop,
            // The review's undo: the text each write replaced, held for the conversation that saw it
            // and put back only while the file still matches what the run left there.
            chat::revert_changes,
            chat::new_task,
            // The conversations the app has filed, and the controls the sidebar's rows have: reopen
            // one, or move one to the Trash. The palette searches all of them by what was said in
            // them, which is why the search reads the files rather than the list.
            chat::list_sessions,
            chat::search_sessions,
            chat::open_session,
            chat::delete_session,
            // The transcript a reopened conversation comes back to: the window hands the host the rows
            // it drew, so the run's steps and diff survive with its words. See `chat::file_rows`.
            chat::file_rows,
            // The three things a row's menu asks of a conversation it has already filed: a name the
            // user chose, a place at the head of the list, and a copy of the whole exchange written
            // out to a file they pick.
            chat::rename_session,
            chat::set_session_pinned,
            chat::export_session,
            // What the Trash holds, and the two presses it offers: put one back on the list, or take
            // its file off the machine. Only the second one is a loss.
            chat::list_trashed,
            chat::restore_trashed,
            chat::purge_trashed,
            // The terminal's own prompt: a shell in the project folder, whose every line is admitted
            // by the same dial a model's `terminal` call goes through before it runs. See `shell`.
            shell::shell_open,
            shell::shell_send,
            shell::shell_interrupt,
            shell::shell_resize,
            shell::shell_close
        ])
        .setup(|app| {
            // Before anything reads settings or the filed conversations: the rename moved the
            // directory they live in, and this is the one launch that has to carry them across.
            paths::migrate_legacy_app_dir();
            // Before this process spawns a server of its own, so a leftover's port is free to be
            // chosen again and its memory is back before the first load.
            // Free models answer through OpenCode's daemon, which may not be up yet on a machine
            // that has just installed this app. Starting it here means the picker's rows are live
            // by the time the window is drawn, instead of greyed out until the first send.
            tauri::async_runtime::spawn(opencode::ensure());
            watch_signals(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building FreeCode");

    app.run(|app_handle, event| match event {
        // The mascot goes on the Dock here rather than in `setup`, because in a dev build Tauri puts
        // the built icon there itself when the event loop reports Ready — after the setup hook and
        // before this callback. Whoever answers Ready last is the one whose face you see.
        tauri::RunEvent::Ready => brand::set_dock_tile(app_handle),
        tauri::RunEvent::Exit => {
            // The shell dies with the window: it is a process in the project folder, and one left
            // behind is one still holding the folder after the app is gone.
            shell::shutdown(app_handle);
        }
        _ => {}
    });
}
