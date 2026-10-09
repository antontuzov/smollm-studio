//! The Hugging Face access token: where it lives, and how it stays out of logs.
//!
//! A gated repository needs a token, and a token is the one piece of user data
//! this app must not scatter around. It goes to the OS credential store and
//! nowhere else: not into `settings.json`, which people paste into bug reports,
//! not into the log file, and not to any host but the one it was written for.
//! Where a platform has no credential store in this build, Hugging Face's own
//! environment variables are the documented path instead.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// The credential's service name, so the item is recognisable in the Keychain.
const SERVICE: &str = "SmolLLM Studio";
/// The account half: which service the token belongs to.
const ACCOUNT: &str = "huggingface";
/// Both names Hugging Face's own tooling accepts, tried in this order.
const ENV_KEYS: [&str; 2] = ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"];

/// A secret that cannot print itself.
///
/// `Debug` and `Display` both redact, so `tracing::debug!("{token}")` or
/// `{client:?}` writes stars rather than the token. Getting the real bytes out
/// takes `expose()`, which is named for what it does.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Blank and whitespace-only input is an empty field, not a secret.
    pub fn new(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    /// The token itself. Only the request that carries it should call this.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Enough to recognise a token without being one: its prefix and last four.
    pub fn masked(&self) -> String {
        let chars: Vec<char> = self.0.chars().collect();
        // Below this length the head and the tail would cover most of the value,
        // so showing them would be showing the secret.
        if chars.len() < 12 {
            return "*".repeat(chars.len());
        }
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Secret({})", self.masked())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.masked())
    }
}

/// Where the token the app is using came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenSource {
    /// The OS credential store, which is where Settings puts it.
    Keychain,
    /// `HF_TOKEN` or `HUGGING_FACE_HUB_TOKEN`, which this app reads but never writes.
    Environment,
    /// Nothing to send. Gated repositories will be refused.
    None,
}

/// What Settings shows about the token: enough to confirm which token is live,
/// and no way to read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenStatus {
    pub source: TokenSource,
    /// `hf_…wxyz` for the token in use, `None` when there is none.
    pub masked: Option<String>,
    /// Whether this build has a credential store for this platform at all.
    pub keychain: bool,
}

impl TokenStatus {
    pub fn is_set(&self) -> bool {
        self.masked.is_some()
    }
}

/// The token to send with a Hugging Face request, preferring the credential
/// store over the environment.
///
/// A store that cannot be read is not fatal: downloads of ungated models work
/// without any token, so the answer is "send nothing from the store" plus a
/// warning that says which store complained.
fn read_preferred() -> (Option<Secret>, TokenSource) {
    let from_store = match read_store() {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("the credential store could not be read: {error}");
            None
        }
    };
    match from_store.as_deref().and_then(Secret::new) {
        Some(secret) => (Some(secret), TokenSource::Keychain),
        None => match env_token() {
            Some(secret) => (Some(secret), TokenSource::Environment),
            None => (None, TokenSource::None),
        },
    }
}

/// The token to send with a Hugging Face request, if there is one.
pub fn hf_token() -> Option<Secret> {
    current_token().0
}

/// The token from Hugging Face's own environment variables, if either is set.
pub fn env_token() -> Option<Secret> {
    ENV_KEYS
        .iter()
        .filter_map(std::env::var_os)
        .find_map(|value| Secret::new(&value.to_string_lossy()))
}

/// The token the app would send, together with what may be shown about it.
///
/// One credential-store read answers both halves, because the desktop app needs
/// the secret for its client and the masked line for its settings screen at the
/// same moment.
pub fn current_token() -> (Option<Secret>, TokenStatus) {
    let (secret, source) = read_preferred();
    let status = TokenStatus {
        source,
        masked: secret.as_ref().map(Secret::masked),
        keychain: store_supported(),
    };
    (secret, status)
}

/// `source` and `masked` for the token in use, which is everything the UI shows.
pub fn token_status() -> TokenStatus {
    current_token().1
}

/// Save a token in the credential store. It is never written anywhere else.
pub fn store_hf_token(value: &str) -> AppResult<()> {
    let secret =
        Secret::new(value).ok_or_else(|| AppError::InvalidRequest("paste a token first".into()))?;
    write_store(secret.expose())
}

/// Remove the stored token. An environment variable is the user's own to unset.
pub fn clear_hf_token() -> AppResult<()> {
    erase_store()
}

/// macOS Keychain and Windows Credential Manager, through the platform's own
/// store and with nothing added to the dependency tree on any other platform.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod store {
    use super::*;

    pub fn supported() -> bool {
        true
    }

    fn entry(service: &str, account: &str) -> AppResult<keyring::Entry> {
        keyring::Entry::new(service, account).map_err(|source| {
            AppError::Config(format!("cannot open the credential store: {source}"))
        })
    }

    pub fn read(service: &str, account: &str) -> AppResult<Option<String>> {
        match entry(service, account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            // No item is the normal state before a token has ever been saved.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(AppError::Config(format!(
                "cannot read the stored token: {source}"
            ))),
        }
    }

    pub fn write(service: &str, account: &str, value: &str) -> AppResult<()> {
        entry(service, account)?
            .set_password(value)
            .map_err(|source| AppError::Config(format!("cannot store the token: {source}")))
    }

    /// A missing item is not a failure of a request to delete it.
    pub fn erase(service: &str, account: &str) -> AppResult<()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(AppError::Config(format!(
                "cannot remove the stored token: {source}"
            ))),
        }
    }
}

/// TODO adapter: no credential store is compiled in for other platforms, because
/// the ones available there either need a D-Bus session server or a C library the
/// app cannot assume. `HF_TOKEN` is the supported path there, and it is read, not
/// written, so a token never lands on disk in plain text.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod store {
    use super::*;

    pub fn supported() -> bool {
        false
    }

    pub fn read(_service: &str, _account: &str) -> AppResult<Option<String>> {
        Ok(None)
    }

    pub fn write(_service: &str, _account: &str, _value: &str) -> AppResult<()> {
        Err(AppError::Config(
            "this platform has no credential store in this build; set HF_TOKEN instead".into(),
        ))
    }

    pub fn erase(_service: &str, _account: &str) -> AppResult<()> {
        Ok(())
    }
}

fn store_supported() -> bool {
    store::supported()
}

fn read_store() -> AppResult<Option<String>> {
    store::read(SERVICE, ACCOUNT)
}

fn write_store(value: &str) -> AppResult<()> {
    store::write(SERVICE, ACCOUNT, value)
}

fn erase_store() -> AppResult<()> {
    store::erase(SERVICE, ACCOUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_field_is_not_a_token() {
        assert!(Secret::new("").is_none());
        assert!(Secret::new("   \n ").is_none());
        assert!(Secret::new("hf_abcd").is_some());
        assert_eq!(
            Secret::new("  hf_abcd  ").expect("trimmed").expose(),
            "hf_abcd",
            "a pasted token keeps the whitespace around it out of the header"
        );
    }

    #[test]
    fn a_secret_redacts_itself_in_debug_and_display() {
        let secret = Secret::new("hf_abcdefghijklmnopqrstuvwxyz").expect("token");
        assert_eq!(secret.masked(), "hf_…wxyz");
        let debug = format!("{secret:?}");
        let display = secret.to_string();
        assert!(debug.contains("hf_…wxyz"), "{debug}");
        assert_eq!(display, "hf_…wxyz");
        for text in [debug, display] {
            assert!(
                !text.contains("cdefgh"),
                "the middle of a token never reaches a log line: {text}"
            );
        }
    }

    #[test]
    fn a_short_value_is_all_stars() {
        // An 8-character value would otherwise print 7 of its 8 characters.
        assert_eq!(Secret::new("abcdefgh").expect("token").masked(), "********");
        assert_eq!(Secret::new("hf_ab").expect("token").masked(), "*****");
        assert_eq!(
            Secret::new("hf_0123456789").expect("token").masked(),
            "hf_…6789",
            "a real token is long enough to show both ends"
        );
    }

    #[test]
    fn the_environment_is_read_in_the_documented_order() {
        // One test for both variables: they are process-wide, so two tests
        // setting them in parallel would race rather than fail.
        std::env::remove_var("HF_TOKEN");
        std::env::remove_var("HUGGING_FACE_HUB_TOKEN");
        assert!(env_token().is_none(), "nothing set is nothing sent");

        std::env::set_var("HUGGING_FACE_HUB_TOKEN", "hf_second");
        assert_eq!(env_token().expect("fallback").expose(), "hf_second");

        std::env::set_var("HF_TOKEN", " hf_first ");
        assert_eq!(
            env_token().expect("preferred").expose(),
            "hf_first",
            "HF_TOKEN is the name Hugging Face documents first"
        );

        std::env::set_var("HF_TOKEN", "   ");
        assert_eq!(
            env_token().expect("skipped blank").expose(),
            "hf_second",
            "a blank HF_TOKEN does not shadow the other variable"
        );

        std::env::remove_var("HF_TOKEN");
        std::env::remove_var("HUGGING_FACE_HUB_TOKEN");
    }

    #[test]
    fn saving_a_blank_token_is_refused_before_the_store_is_touched() {
        let error = store_hf_token("  ").expect_err("not a token");
        assert!(matches!(error, AppError::InvalidRequest(_)), "{error:?}");
    }

    #[test]
    fn a_status_never_carries_the_token() {
        let status = TokenStatus {
            source: TokenSource::Keychain,
            masked: Some("hf_…wxyz".into()),
            keychain: true,
        };
        assert!(status.is_set());
        let json = serde_json::to_string(&status).expect("serialises");
        assert!(json.contains("\"source\":\"keychain\""), "{json}");
        assert!(!json.contains("abcdefghijklmnopqrstuvwxyz"), "{json}");
    }

    /// Opt-in because it opens the real Keychain of whoever runs it, and macOS
    /// asks that person to allow access the first time an unsigned build does.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to and reads from the developer's real keychain"]
    fn the_credential_store_round_trips_a_token() {
        let account = format!("{ACCOUNT}-self-test-{}", uuid::Uuid::new_v4().simple());
        let token = "hf_selftest_not_a_real_token";
        store::write(SERVICE, &account, token).expect("stored");
        assert_eq!(
            store::read(SERVICE, &account).expect("read").as_deref(),
            Some(token)
        );
        store::erase(SERVICE, &account).expect("erased");
        assert_eq!(
            store::read(SERVICE, &account).expect("read after erase"),
            None,
            "a deleted item stays deleted"
        );
        store::erase(SERVICE, &account).expect("erasing twice is not an error");
    }
}
