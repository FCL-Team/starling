//! Starling's switchable Microsoft, Ely.by and offline profiles inside the game (crate starling-account; a phone has no
//! launcher to sign in for it): the secrets sealed by the platform's keystore (the Android
//! Keystore) in the private folder, the account list beside them, and the session handed to
//! [`crate::auth`] for online-mode servers, renewed in the background before it runs out. The
//! game's official files are fetched from Mojang's servers here too. Without a
//! keystore (a desktop build, `STARLING_ACCOUNTS=1`) the secrets stay in memory for the run.
use starling_account::files::{Pins, Progress, Sources, Wanted};
pub use starling_account::skins::ELY_SKINS;
pub use starling_account::{Account, Provider, SignInStage};
use starling_account::{Endpoints, MemoryStore, SecretStore, SignIn, SignInState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

/// How often the background renewal looks at the token (it renews in its last hour).
const RENEW_EVERY: Duration = Duration::from_secs(20 * 60);
/// Written beside the official files once every one of them is fetched and checked.
const FILES_DONE: &str = "starling-files.json";

/// Whether the game signs in itself: on Android, or on a desktop asked to
/// (`STARLING_ACCOUNTS=1`; the launcher signs in there otherwise).
pub fn in_game() -> bool {
    crate::platform::registered() || std::env::var_os("STARLING_ACCOUNTS").is_some_and(|v| v == "1")
}

/// The private folder holding `accounts.json` and the sealed secrets.
pub fn dir() -> PathBuf {
    crate::logging::private_home().join("accounts")
}

/// The host's own game folder when it launched from one (its `versions/`, `libraries/` and
/// `assets/` are the official launcher's layout); `None` for a standalone launch.
fn host_files_root() -> Option<PathBuf> {
    crate::host_env::get()
        .files_home()
        .filter(|dir| {
            dir.join("versions")
                .join(horizon_core::VANILLA_VERSION)
                .is_dir()
        })
        .cloned()
}

/// Where the official files are, in the official launcher's layout (`versions/`, `libraries/`,
/// `assets/`): the host's own game folder when it gave one, else the private folder.
pub fn files_root() -> PathBuf {
    host_files_root().unwrap_or_else(|| crate::logging::private_home().join("minecraft"))
}

/// The server jar the last complete download fetched, for the preparer (preparer.rs). A host
/// game folder supplies its own, beside the client's version or at the folder's root; the host
/// fetches it there when it cannot.
pub fn server_jar() -> Option<PathBuf> {
    if let Some(root) = host_files_root() {
        let version = horizon_core::VANILLA_VERSION;
        for candidate in [
            root.join("versions").join(version).join("server.jar"),
            root.join("server.jar"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        return None;
    }
    let done: serde_json::Value =
        serde_json::from_slice(&std::fs::read(files_root().join(FILES_DONE)).ok()?).ok()?;
    done["server"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Secrets sealed by the platform's keystore, one file each: what lands on the disk opens on
/// this device only.
struct SealedStore {
    dir: PathBuf,
}

impl SealedStore {
    fn path(&self, key: &str) -> Result<PathBuf, String> {
        // Keys are UUIDs with an optional provider prefix, never a path.
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("not an account key".into());
        }
        Ok(self.dir.join(format!("{key}.sealed")))
    }
}

impl SecretStore for SealedStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let path = self.path(key)?;
        let sealed = match std::fs::read(&path) {
            Ok(sealed) => sealed,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        // Sealed by another install's key (the app reinstalled): signed out, not an error.
        Ok(crate::platform::open(&sealed).and_then(|plain| String::from_utf8(plain).ok()))
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), String> {
        let path = self.path(key)?;
        let sealed = crate::platform::seal(secret.as_bytes())
            .ok_or("The device's keystore refused to keep the sign-in.")?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        std::fs::write(&part, sealed).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &path).map_err(|e| e.to_string())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn persistent(&self) -> bool {
        true
    }
}

/// The store for this run: sealed where the platform can seal, else memory.
pub fn store() -> Arc<dyn SecretStore> {
    static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            if crate::platform::can_seal() {
                Arc::new(SealedStore {
                    dir: dir().join("secrets"),
                })
            } else {
                Arc::new(MemoryStore::default())
            }
        })
        .clone()
}

/// Hand `account`'s session to the game: online-mode servers join with it, worlds open with its
/// name.
fn use_session(account: &Account, secrets: &starling_account::Secrets) -> Result<(), String> {
    let uuid = account
        .uuid_u128()
        .ok_or("The Minecraft profile has no valid UUID.")?;
    let mut session = match account.provider {
        starling_account::Provider::Microsoft => {
            crate::auth::AuthSession::new(&account.name, uuid, secrets.access_token())
        }
        starling_account::Provider::Ely => {
            crate::auth::AuthSession::ely(&account.name, uuid, secrets.access_token())
        }
    };
    session.xuid = secrets.xuid().map(str::to_owned);
    crate::auth::set_current(session);
    Ok(())
}

/// Where the game's account is: what the account screen shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    /// Not signed in: the game plays offline (single player, LAN, offline-mode servers).
    #[default]
    Offline,
    /// Renewing the saved sign-in.
    Resuming,
    /// Asking Microsoft for a code.
    Starting,
    Progress(starling_account::SignInStage),
    /// Waiting for the player at `link` with `code`.
    Code {
        code: String,
        link: String,
    },
    SignedIn(Account),
    OfflineProfile(String),
    /// The last attempt failed: the reason, for the player.
    Failed(String),
}

/// Where the official files are.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Files {
    /// Not fetched (or not all of them).
    #[default]
    Missing,
    Checking,
    Downloading(Progress),
    /// Every file is there and checked.
    Ready,
    Failed(String),
}

#[derive(Default)]
struct Shared {
    profiles: Option<starling_account::Accounts>,
    generation: u64,
    state: State,
    files: Files,
    /// Set to stop the sign-in or the download in progress.
    cancel: Option<Arc<AtomicBool>>,
    files_cancel: Option<Arc<AtomicBool>>,
    /// Whether the files' folder was looked at for an earlier download.
    files_seen: bool,
    /// The last skin upload: the profile it went to, and how it went.
    skin_upload: Option<(String, crate::own_skin::Upload)>,
}

fn shared() -> MutexGuard<'static, Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The account now: what the account screen and the title's button show.
pub fn state() -> State {
    shared().state.clone()
}

/// The official files now.
pub fn files() -> Files {
    let mut s = shared();
    if !s.files_seen {
        s.files_seen = true;
        // A host game folder the launcher already filled: no sign-in, no download.
        let host_ready = host_files_root().is_some();
        if s.files == Files::Missing && (host_ready || files_root().join(FILES_DONE).exists()) {
            s.files = Files::Ready;
        }
    }
    s.files.clone()
}

/// Saved profiles, including offline names; metadata only, never credentials.
pub fn profiles() -> starling_account::Accounts {
    shared()
        .profiles
        .get_or_insert_with(|| starling_account::Accounts::load(&dir()))
        .clone()
}

pub fn player_name() -> Option<String> {
    if !in_game() {
        return None;
    }
    profiles().player().map(|p| p.name().to_owned())
}

/// The playing profile, read once and kept like `profiles`.
fn player() -> Option<starling_account::Player> {
    if !in_game() {
        return None;
    }
    shared()
        .profiles
        .get_or_insert_with(|| starling_account::Accounts::load(&dir()))
        .player()
}

/// The key of the playing profile (`Player::key`), under which its own skin is kept
/// (`own_skin::SkinChoice::for_profile`). None without profiles (a desktop and its launcher).
pub fn player_key() -> Option<String> {
    player().map(|p| p.key())
}

/// The playing Microsoft account's skin on its profile, as last read (at sign-in, on renewal, after
/// an upload): its URL and whether its arms are slim.
pub fn player_skin() -> Option<(String, bool)> {
    match player()? {
        starling_account::Player::Account(account) if account.provider == Provider::Microsoft => {
            account.skin.map(|(url, model)| (url, model == "slim"))
        }
        _ => None,
    }
}

/// The playing Ely.by account's skin as Ely.by named it at the sign-in or the last renewal
/// (`starling_account::ely`, kept with the account): its URL and whether its arms are slim. None
/// for another profile, or an Ely.by account Ely.by named no skin for.
pub fn ely_skin() -> Option<(String, bool)> {
    match player()? {
        starling_account::Player::Account(account) if account.provider == Provider::Ely => {
            account.skin.map(|(url, model)| (url, model == "slim"))
        }
        _ => None,
    }
}

/// Where the playing profile's skin can go besides this game: a Microsoft account's profile (and
/// how the last upload there went), Ely.by's website, or nowhere for an offline one.
pub fn skin_cloud() -> crate::own_skin::Cloud {
    use crate::own_skin::{Cloud, Upload};
    let Some(starling_account::Player::Account(account)) = player() else {
        return Cloud::Local;
    };
    match account.provider {
        Provider::Ely => Cloud::Ely,
        Provider::Microsoft => {
            let key = account.key();
            Cloud::Microsoft(match &shared().skin_upload {
                Some((to, upload)) if *to == key => upload.clone(),
                _ => Upload::None,
            })
        }
    }
}

/// Send `png` (checked and written anew: `own_skin::upload_png`) as the skin of the Microsoft
/// profile `key`, with slim or classic arms, off the window thread; [`skin_cloud`] tells how it
/// goes. One upload at a time.
pub fn upload_skin(key: String, png: Vec<u8>, slim: bool) {
    use crate::own_skin::Upload;
    {
        let mut s = shared();
        if matches!(s.skin_upload, Some((_, Upload::Sending))) {
            return;
        }
        s.skin_upload = Some((key.clone(), Upload::Sending));
    }
    std::thread::spawn(move || {
        let result = starling_account::skins::upload_skin(
            &Endpoints::live(),
            store().as_ref(),
            &dir(),
            &key,
            &png,
            slim,
        );
        let mut s = shared();
        s.profiles = Some(starling_account::Accounts::load(&dir()));
        let upload = match result {
            Ok(account) => {
                if let State::SignedIn(playing) = &mut s.state
                    && playing.key() == account.key()
                {
                    *playing = account;
                }
                Upload::Sent
            }
            Err(e) => {
                log::warn!("Starling: the skin was not uploaded: {e}");
                Upload::Failed(e)
            }
        };
        s.skin_upload = Some((key, upload));
    });
}

fn begin(state: State) -> (u64, Arc<AtomicBool>) {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut s = shared();
    if let Some(old) = s.cancel.replace(cancel.clone()) {
        old.store(true, Ordering::Relaxed);
    }
    s.generation = s.generation.wrapping_add(1);
    s.state = state;
    (s.generation, cancel)
}

fn renew(generation: u64) -> Result<State, String> {
    let session = starling_account::session(&Endpoints::live(), store().as_ref(), &dir())?;
    let s = shared();
    if s.generation != generation {
        return Err("starling.account.error.cancelled".into());
    }
    if let Some((account, secrets)) = session {
        use_session(&account, &secrets)?;
        Ok(State::SignedIn(account))
    } else {
        crate::auth::clear_current();
        Ok(match starling_account::Accounts::load(&dir()).player() {
            Some(starling_account::Player::Offline(name)) => State::OfflineProfile(name),
            _ => State::Offline,
        })
    }
}
fn finish(generation: u64, result: Result<State, String>) {
    let mut s = shared();
    if s.generation == generation {
        s.profiles = Some(starling_account::Accounts::load(&dir()));
        s.state = result.unwrap_or_else(State::Failed);
        s.cancel = None;
    }
}
pub fn resume() {
    if profiles().player().is_none() {
        return;
    }
    let (generation, _) = begin(State::Resuming);
    std::thread::spawn(move || finish(generation, renew(generation)));
    keep_renewing();
}
fn keep_renewing() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(RENEW_EVERY);
            let (generation, signed_in) = {
                let s = shared();
                (s.generation, matches!(s.state, State::SignedIn(_)))
            };
            if signed_in {
                match renew(generation) {
                    Ok(state) => finish(generation, Ok(state)),
                    Err(_) => log::warn!(
                        "Starling: background account renewal failed; open Accounts to sign in again."
                    ),
                }
            }
        }
    });
}
/// Profile changes happen off the window thread; a later choice always wins.
pub fn select(key: String) {
    let (generation, cancel) = begin(State::Resuming);
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let result = starling_account::activate_profile(
            &Endpoints::live(),
            store().as_ref(),
            &dir(),
            &key,
            &|| cancel.load(Ordering::Relaxed),
        )
        .and_then(|()| renew(generation));
        finish(generation, result);
    });
}
pub fn add_offline(name: String) {
    let (generation, cancel) = begin(State::Resuming);
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let result =
            starling_account::add_offline_if(&dir(), &name, &|| cancel.load(Ordering::Relaxed))
                .and_then(|()| renew(generation));
        finish(generation, result);
    });
}
pub fn remove(key: String) {
    let (generation, cancel) = begin(State::Resuming);
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let result = starling_account::remove_profile_if(store().as_ref(), &dir(), &key, &|| {
            cancel.load(Ordering::Relaxed)
        })
        .and_then(|()| renew(generation));
        finish(generation, result);
    });
}
pub fn sign_in_ely(username: String, password: String, otp: String) {
    let (generation, cancel) = begin(State::Starting);
    let working = KeepWorking::new(
        &format!("sign-in-{generation}"),
        &crate::lang::get("starling.account.ely_working"),
    );
    std::thread::spawn(move || {
        let _working = working;
        let result = starling_account::keep_ely(
            store().as_ref(),
            &dir(),
            &username,
            &password,
            &otp,
            &|| cancel.load(Ordering::Relaxed),
        )
        .and_then(|_| renew(generation));
        finish(generation, result);
    });
    keep_renewing();
}

/// The platform keeps the game in the foreground while this lives (Android's WorkService): the
/// sign-in waits on the browser, a download on the network, and a phone may cut a background
/// app's internet or end it meanwhile.
struct KeepWorking(String);
impl KeepWorking {
    fn new(reason: &str, line: &str) -> Self {
        crate::platform::keep_working(reason, line, true);
        Self(reason.to_owned())
    }
}
impl Drop for KeepWorking {
    fn drop(&mut self) {
        crate::platform::keep_working(&self.0, "", false);
    }
}

/// Start signing in: the device code arrives in [`state`] as [`State::Code`]; when the player
/// is done there, the account is kept and the game plays as it.
pub fn sign_in() {
    let (generation, cancel) = begin(State::Starting);
    let sign_in = SignIn::start_cancellable(Endpoints::live(), store(), dir(), cancel.clone());
    // Asked for now, while the game is in front: Android refuses it from the background.
    let working = KeepWorking::new(
        &format!("sign-in-{generation}"),
        "Waiting for your Microsoft sign-in",
    );
    std::thread::spawn(move || {
        let _working = working;
        let result = loop {
            if cancel.load(Ordering::Relaxed) {
                sign_in.cancel();
                return;
            }
            match sign_in.state() {
                SignInState::Starting => {}
                SignInState::Progress(stage) => {
                    let mut s = shared();
                    if s.generation == generation && !cancel.load(Ordering::Relaxed) {
                        s.state = State::Progress(stage);
                    }
                }
                SignInState::Code(code) => {
                    let to = State::Code {
                        code: code.user_code,
                        link: code.verification_uri,
                    };
                    let mut s = shared();
                    if s.generation == generation
                        && !cancel.load(Ordering::Relaxed)
                        && s.state != to
                    {
                        s.state = to;
                    }
                }
                SignInState::Done(result) => break result,
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let state = result.and_then(|_| renew(generation));
        finish(generation, state);
    });
    keep_renewing();
}

/// Stop a sign-in in progress.
pub fn cancel() {
    let (generation, _) = begin(State::Resuming);
    std::thread::spawn(move || finish(generation, renew(generation)));
}

/// Sign out the active profile only, preserving other saved profiles.
pub fn sign_out() -> Result<(), String> {
    if let Some(player) = profiles().player() {
        remove(player.key());
    }
    Ok(())
}

/// Fetch the official files of the version Starling plays from Mojang's servers, as the
/// official launcher does: after a sign-in, or for a player who chose to play offline, each
/// file checked against its hash, into [`files_root`]. A second call resumes what an
/// interrupted one left.
pub fn download_files() {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut s = shared();
        if matches!(s.files, Files::Checking | Files::Downloading(_)) {
            return;
        }
        s.files = Files::Checking;
        s.files_cancel = Some(cancel.clone());
    }
    let working = KeepWorking::new("files", "Downloading the game files");
    std::thread::spawn(move || {
        let _working = working;
        let result = (|| {
            let root = files_root();
            let _ = std::fs::remove_file(root.join(FILES_DONE));
            let progress = |p: Progress| {
                let mut s = shared();
                if matches!(s.files, Files::Checking | Files::Downloading(_)) {
                    s.files = Files::Downloading(p);
                }
            };
            let wanted = Wanted {
                server: true,
                libraries: true,
                assets: true,
            };
            let pins = Pins::starling();
            let installed = starling_account::files::download(
                &Sources::live(),
                &pins,
                &root,
                wanted,
                &progress,
                &|| cancel.load(Ordering::Relaxed),
            )?;
            let done = serde_json::json!({
                "version": pins.version,
                "client": installed.client_jar,
                "server": installed.server_jar,
            });
            std::fs::write(root.join(FILES_DONE), done.to_string()).map_err(|e| e.to_string())
        })();
        let mut s = shared();
        s.files_cancel = None;
        s.files = match result {
            Ok(()) => Files::Ready,
            Err(_) if cancel.load(Ordering::Relaxed) => Files::Missing,
            Err(e) => {
                log::warn!("Horizon: the game files were not fetched: {e}");
                Files::Failed(e)
            }
        };
    });
}

/// Stop the download in progress (what it fetched stays for the next one).
pub fn cancel_download() {
    if let Some(cancel) = shared().files_cancel.take() {
        cancel.store(true, Ordering::Relaxed);
    }
}
