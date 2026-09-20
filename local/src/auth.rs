//! Where the GitHub token lives on this machine, and how it stays fresh.
//!
//! `prmarmot-core` owns the device-flow protocol and stays filesystem-free;
//! this module owns the storage and the one piece of state the protocol needs
//! a home for. The iOS app replaces this file with a Keychain store written in
//! Swift — same `TokenSource` contract, different backing store.
//!
//! Two backends:
//!
//! * **Keychain** (macOS, the default there) — a generic password under the
//!   service `dev.prmarmot.auth`, one item per host.
//! * **File** (everywhere else, and available on macOS via
//!   `[auth] store = "file"`) — `0600` JSON under the state directory, the
//!   pattern `aws`, `npm` and `docker` use.
//!
//! macOS caveat worth knowing before you file a bug: keychain access control
//! is per-binary, so the first read from `prmarmot` and the first read from
//! `prmarmot-cli` each raise a system prompt ("Always Allow" settles it), and
//! an ad-hoc-signed development build raises it again after every rebuild
//! because its signature changed. `store = "file"` avoids that entirely.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prmarmot_core::github::device_flow::{DeviceFlow, TokenSet};
use prmarmot_core::github::{normalize_host, AuthTransport, GhError, TokenSource};
use serde::{Deserialize, Serialize};

/// Keychain service name, and the file name under the state directory.
pub const KEYCHAIN_SERVICE: &str = "dev.prmarmot.auth";
pub const AUTH_FILE: &str = "auth.json";

/// How the token was obtained. Only a device-flow token can be refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Device,
    Token,
}

/// One signed-in host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAuth {
    pub host: String,
    pub kind: TokenKind,
    /// The `(host, client_id)` pair the token was issued for; a refresh has to
    /// use the same client ID, and GHES instances have their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Remembered so `auth status` can name the account without a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(flatten)]
    pub token: TokenSet,
}

impl StoredAuth {
    pub fn device(host: &str, client_id: &str, token: TokenSet) -> Self {
        Self {
            host: normalize_host(host),
            kind: TokenKind::Device,
            client_id: Some(client_id.to_owned()),
            login: None,
            token,
        }
    }

    pub fn pat(host: &str, token: &str) -> Self {
        Self {
            host: normalize_host(host),
            kind: TokenKind::Token,
            client_id: None,
            login: None,
            token: TokenSet::from_pat(token),
        }
    }

    pub fn with_login(mut self, login: impl Into<String>) -> Self {
        self.login = Some(login.into());
        self
    }
}

/// Read, write and forget the token for one host. Errors are strings because
/// every caller renders them, and none can recover.
pub trait TokenStore: Send + Sync {
    fn load(&self, host: &str) -> Result<Option<StoredAuth>, String>;
    fn save(&self, auth: &StoredAuth) -> Result<(), String>;
    fn delete(&self, host: &str) -> Result<(), String>;
    /// Where the token is, in words, for `auth status`.
    fn describe(&self) -> String;
}

/// `[auth] store`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreKind {
    /// Keychain on macOS, file elsewhere.
    #[default]
    Auto,
    Keychain,
    File,
}

impl StoreKind {
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "keychain" => Some(Self::Keychain),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// True for a fine-grained personal access token, which GitHub prefixes
/// `github_pat_`. Its reach is one owner's repositories, so a board opened
/// with one can be missing an organization's private repositories.
pub fn is_fine_grained(token: &str) -> bool {
    token.starts_with("github_pat_")
}

/// The store this platform and configuration ask for. A keychain asked for on
/// a platform without one falls back to the file store rather than failing:
/// losing the token store must never make the app unusable.
pub fn token_store(kind: StoreKind) -> Arc<dyn TokenStore> {
    #[cfg(target_os = "macos")]
    {
        if matches!(kind, StoreKind::Auto | StoreKind::Keychain) {
            return Arc::new(KeychainTokenStore);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
    Arc::new(FileTokenStore::default())
}

/// `$XDG_STATE_HOME/prmarmot/auth.json`, mode `0600`.
pub struct FileTokenStore {
    pub path: PathBuf,
}

impl Default for FileTokenStore {
    fn default() -> Self {
        Self {
            path: crate::config::state_root().join(AUTH_FILE),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthFile {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    hosts: std::collections::BTreeMap<String, StoredAuth>,
}

fn one() -> u32 {
    1
}

impl FileTokenStore {
    fn read(&self) -> Result<AuthFile, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("could not read {}: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AuthFile::default()),
            Err(e) => Err(format!("could not read {}: {e}", self.path.display())),
        }
    }

    fn write(&self, file: &AuthFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(file)
            .map_err(|e| format!("could not encode the token file: {e}"))?;
        // Write the credential to a private temporary file first, so the token
        // is never briefly world-readable and a crash never truncates the file.
        let temp = self.path.with_extension("json.tmp");
        write_private(&temp, text.as_bytes())?;
        std::fs::rename(&temp, &self.path)
            .map_err(|e| format!("could not write {}: {e}", self.path.display()))
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

impl TokenStore for FileTokenStore {
    fn load(&self, host: &str) -> Result<Option<StoredAuth>, String> {
        Ok(self.read()?.hosts.remove(&normalize_host(host)))
    }

    fn save(&self, auth: &StoredAuth) -> Result<(), String> {
        let mut file = self.read()?;
        file.version = 1;
        file.hosts.insert(normalize_host(&auth.host), auth.clone());
        self.write(&file)
    }

    fn delete(&self, host: &str) -> Result<(), String> {
        let mut file = self.read()?;
        if file.hosts.remove(&normalize_host(host)).is_none() {
            return Ok(());
        }
        if file.hosts.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("could not remove {}: {e}", self.path.display())),
            };
        }
        self.write(&file)
    }

    fn describe(&self) -> String {
        format!("{} (mode 0600)", self.path.display())
    }
}

/// The macOS login keychain, one generic password per host.
#[cfg(target_os = "macos")]
pub struct KeychainTokenStore;

#[cfg(target_os = "macos")]
impl TokenStore for KeychainTokenStore {
    fn load(&self, host: &str) -> Result<Option<StoredAuth>, String> {
        let host = normalize_host(host);
        match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &host) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("the keychain item for {host} is unreadable: {e}")),
            // errSecItemNotFound — nothing stored for this host yet.
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => Err(format!("could not read the keychain: {e}")),
        }
    }

    fn save(&self, auth: &StoredAuth) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(auth).map_err(|e| format!("could not encode the token: {e}"))?;
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            &normalize_host(&auth.host),
            &bytes,
        )
        .map_err(|e| format!("could not write to the keychain: {e}"))
    }

    fn delete(&self, host: &str) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            &normalize_host(host),
        ) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(format!("could not remove the keychain item: {e}")),
        }
    }

    fn describe(&self) -> String {
        format!("the macOS keychain (service {KEYCHAIN_SERVICE})")
    }
}

/// A [`TokenSource`] backed by the store, refreshing a device-flow token when
/// it is within [`REFRESH_SKEW_SECS`] of expiry and writing the new pair back.
///
/// [`REFRESH_SKEW_SECS`]: prmarmot_core::github::device_flow::REFRESH_SKEW_SECS
pub struct StoredTokenSource {
    store: Arc<dyn TokenStore>,
    /// Unauthenticated transport for the refresh endpoint; `None` disables
    /// refreshing (a PAT never needs it).
    auth: Option<Arc<dyn AuthTransport>>,
    host: String,
    now: fn() -> i64,
    state: Mutex<Option<StoredAuth>>,
}

impl StoredTokenSource {
    pub fn new(
        store: Arc<dyn TokenStore>,
        host: &str,
        auth: Option<Arc<dyn AuthTransport>>,
    ) -> Self {
        Self {
            store,
            auth,
            host: normalize_host(host),
            now: now_epoch,
            state: Mutex::new(None),
        }
    }

    /// Drive the clock from a test instead of the system.
    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    /// The stored record as it is right now, without refreshing. For
    /// `auth status`.
    pub fn stored(&self) -> Result<Option<StoredAuth>, String> {
        self.store.load(&self.host)
    }
}

fn now_epoch() -> i64 {
    chrono::Utc::now().timestamp()
}

impl TokenSource for StoredTokenSource {
    fn token(&self) -> Result<String, GhError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GhError::Network("the token store lock was poisoned".into()))?;
        if state.is_none() {
            *state = self
                .store
                .load(&self.host)
                .map_err(GhError::Network)?
                .or(None);
        }
        let Some(auth) = state.as_mut() else {
            return Err(GhError::NotAuthenticated);
        };
        let now = (self.now)();
        if !auth.token.needs_refresh(now) {
            return Ok(auth.token.access_token.clone());
        }
        // Expired, and nothing can renew it: say so rather than sending a
        // token GitHub will reject.
        if !auth.token.can_refresh(now) {
            return Err(GhError::NotAuthenticated);
        }
        let (Some(transport), Some(client_id)) = (self.auth.as_ref(), auth.client_id.clone())
        else {
            return Err(GhError::NotAuthenticated);
        };
        let refresh_token = auth
            .token
            .refresh_token
            .clone()
            .ok_or(GhError::NotAuthenticated)?;
        let flow = DeviceFlow::new(&auth.host, client_id);
        let refreshed = flow.refresh(transport.as_ref(), &refresh_token, now)?;
        auth.token = refreshed;
        // A store that cannot be written still leaves this process signed in;
        // the next launch would ask for a new sign-in, which is honest.
        self.store.save(auth).map_err(GhError::Network)?;
        Ok(auth.token.access_token.clone())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_githubs_fine_grained_prefix_counts() {
        assert!(super::is_fine_grained("github_pat_11ABCDEFG0abcdefg"));
        assert!(!super::is_fine_grained("ghp_16CharactersOfClassic"));
        assert!(!super::is_fine_grained("gho_device_flow_token"));
        assert!(!super::is_fine_grained(""));
    }

    use super::*;
    use prmarmot_core::github::device_flow::REFRESH_SKEW_SECS;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_store() -> (FileTokenStore, tempdir::TempDir) {
        let dir = tempdir::TempDir::new();
        (
            FileTokenStore {
                path: dir.path().join(AUTH_FILE),
            },
            dir,
        )
    }

    /// Minimal scratch directory; the crate has no dev-dependency budget for
    /// one and this is four lines.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        // macOS clocks have microsecond resolution, so two test threads can
        // read the same nanosecond; a counter is what actually makes the name
        // unique.
        static NEXT: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new() -> Self {
                let unique = format!(
                    "prmarmot-auth-{}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                );
                let path = std::env::temp_dir().join(unique);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    struct Replies {
        bodies: Mutex<Vec<serde_json::Value>>,
        calls: AtomicUsize,
    }

    impl AuthTransport for Replies {
        fn post_form(
            &self,
            _url: &str,
            _fields: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.bodies
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| GhError::Network("no recorded reply".into()))
        }
    }

    fn replies(bodies: Vec<serde_json::Value>) -> Arc<Replies> {
        Arc::new(Replies {
            bodies: Mutex::new(bodies.into_iter().rev().collect()),
            calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn a_pat_round_trips_through_the_file_store() {
        let (store, _dir) = temp_store();
        assert_eq!(store.load("github.com").unwrap(), None);
        let auth = StoredAuth::pat("https://GitHub.com/", "ghp_secret").with_login("octo");
        store.save(&auth).unwrap();
        let loaded = store.load("api.github.com").unwrap().unwrap();
        assert_eq!(loaded.token.access_token, "ghp_secret");
        assert_eq!(loaded.kind, TokenKind::Token);
        assert_eq!(loaded.login.as_deref(), Some("octo"));
        store.delete("github.com").unwrap();
        assert_eq!(store.load("github.com").unwrap(), None);
        // Deleting what is not there is not an error.
        store.delete("github.com").unwrap();
    }

    #[test]
    fn several_hosts_coexist_and_the_file_is_private() {
        let (store, _dir) = temp_store();
        store.save(&StoredAuth::pat("github.com", "a")).unwrap();
        store
            .save(&StoredAuth::device(
                "ghe.acme.test",
                "ghes-client",
                TokenSet::from_pat("b"),
            ))
            .unwrap();
        assert_eq!(
            store.load("ghe.acme.test").unwrap().unwrap().client_id,
            Some("ghes-client".into())
        );
        assert_eq!(
            store
                .load("github.com")
                .unwrap()
                .unwrap()
                .token
                .access_token,
            "a"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store.path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the token file must not be readable by others"
            );
        }

        store.delete("github.com").unwrap();
        assert!(store.load("ghe.acme.test").unwrap().is_some());
    }

    #[test]
    fn a_live_token_is_returned_without_touching_the_network() {
        let (store, _dir) = temp_store();
        let mut token = TokenSet::from_pat("ghu_live");
        token.expires_at = Some(10_000);
        token.refresh_token = Some("ghr_x".into());
        store
            .save(&StoredAuth::device("github.com", "Iv1.abc", token))
            .unwrap();
        let transport = replies(vec![]);
        let source = StoredTokenSource::new(Arc::new(store), "github.com", Some(transport.clone()))
            .with_clock(|| 1_000);
        assert_eq!(source.token().unwrap(), "ghu_live");
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_token_inside_the_skew_refreshes_and_the_new_pair_is_persisted() {
        let (store, dir) = temp_store();
        let mut token = TokenSet::from_pat("ghu_old");
        token.expires_at = Some(1_000 + REFRESH_SKEW_SECS);
        token.refresh_token = Some("ghr_old".into());
        token.refresh_expires_at = Some(9_999_999);
        store
            .save(&StoredAuth::device("github.com", "Iv1.abc", token))
            .unwrap();
        let transport = replies(vec![json!({
            "access_token": "ghu_new",
            "expires_in": 28800,
            "refresh_token": "ghr_new",
            "refresh_token_expires_in": 15_897_600
        })]);
        let path = store.path.clone();
        let source = StoredTokenSource::new(Arc::new(store), "github.com", Some(transport.clone()))
            .with_clock(|| 1_000);
        assert_eq!(source.token().unwrap(), "ghu_new");
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
        // Cached: a second call does not poll again.
        assert_eq!(source.token().unwrap(), "ghu_new");
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);

        let persisted = FileTokenStore { path }.load("github.com").unwrap().unwrap();
        assert_eq!(persisted.token.access_token, "ghu_new");
        assert_eq!(persisted.token.refresh_token.as_deref(), Some("ghr_new"));
        drop(dir);
    }

    #[test]
    fn an_expired_refresh_token_asks_for_a_new_sign_in_without_a_request() {
        let (store, _dir) = temp_store();
        let mut token = TokenSet::from_pat("ghu_old");
        token.expires_at = Some(500);
        token.refresh_token = Some("ghr_old".into());
        token.refresh_expires_at = Some(900);
        store
            .save(&StoredAuth::device("github.com", "Iv1.abc", token))
            .unwrap();
        let transport = replies(vec![]);
        let source = StoredTokenSource::new(Arc::new(store), "github.com", Some(transport.clone()))
            .with_clock(|| 1_000);
        assert_eq!(source.token().unwrap_err(), GhError::NotAuthenticated);
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn nothing_stored_reads_as_signed_out() {
        let (store, _dir) = temp_store();
        let source = StoredTokenSource::new(Arc::new(store), "github.com", None);
        assert_eq!(source.token().unwrap_err(), GhError::NotAuthenticated);
    }

    #[test]
    fn a_dead_refresh_token_reported_by_github_reads_as_signed_out() {
        let (store, _dir) = temp_store();
        let mut token = TokenSet::from_pat("ghu_old");
        token.expires_at = Some(1_000);
        token.refresh_token = Some("ghr_old".into());
        store
            .save(&StoredAuth::device("github.com", "Iv1.abc", token))
            .unwrap();
        let transport = replies(vec![json!({"error": "bad_refresh_token"})]);
        let source = StoredTokenSource::new(Arc::new(store), "github.com", Some(transport))
            .with_clock(|| 2_000);
        assert_eq!(source.token().unwrap_err(), GhError::NotAuthenticated);
    }

    #[test]
    fn store_kinds_parse_the_documented_words() {
        assert_eq!(StoreKind::parse("keychain"), Some(StoreKind::Keychain));
        assert_eq!(StoreKind::parse(" File "), Some(StoreKind::File));
        assert_eq!(StoreKind::parse("auto"), Some(StoreKind::Auto));
        assert_eq!(StoreKind::parse("vault"), None);
    }
}
