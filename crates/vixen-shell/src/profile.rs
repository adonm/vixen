//! App-ID scoped profile paths and Linux user-directory discovery.
//!
//! This module is intentionally GTK-free so the default workspace build can
//! expose the profile contract without pulling platform helper crates.

use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use vixen_store::{ClearDataSelection, MAX_SESSION_TABS, SessionRecord, Store};

const PROFILE_DB_FILENAME: &str = "profile.redb";
const PROFILE_DOWNLOADS_DIRNAME: &str = "downloads";
const REPORTS_DIRNAME: &str = "reports";

/// Resolved profile paths for one Vixen app ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePaths {
    pub app_id: String,
    /// App data directory, e.g. `$XDG_DATA_HOME/org.vixen.Vixen`.
    pub data_dir: PathBuf,
    /// Single redb profile database inside [`Self::data_dir`].
    pub database: PathBuf,
    /// Profile-scoped directory for accepted download state/artifacts.
    pub profile_downloads_dir: PathBuf,
    /// Optional host user Downloads directory from XDG user-dirs.
    pub user_downloads_dir: Option<PathBuf>,
    /// Optional diagnostics/smoke artifacts directory.
    pub reports_dir: PathBuf,
}

/// Deterministic shell tab restore state derived from the profile database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSession {
    pub tabs: Vec<String>,
    pub active_index: usize,
}

impl ProfilePaths {
    /// Directory new downloads should target. Prefer the host XDG Downloads
    /// directory when available; otherwise fall back to the profile directory so
    /// downloads remain app-ID scoped instead of spilling into the cwd.
    pub fn downloads_target_dir(&self) -> &Path {
        self.user_downloads_dir
            .as_deref()
            .unwrap_or(&self.profile_downloads_dir)
    }

    /// Resolve a safe destination for a newly accepted download.
    pub fn download_destination(
        &self,
        suggested_filename: &str,
    ) -> Result<PathBuf, DownloadPathError> {
        let filename = validate_download_filename(suggested_filename)?;
        Ok(self.downloads_target_dir().join(filename))
    }

    /// Return the directory that a shell may safely reveal for an existing
    /// download destination. The destination must stay inside the configured
    /// user/profile downloads root; arbitrary absolute paths are rejected.
    pub fn show_in_folder_dir(&self, destination: &Path) -> Result<PathBuf, DownloadPathError> {
        let parent = destination
            .parent()
            .ok_or(DownloadPathError::MissingParent)?;
        if path_is_under(parent, self.downloads_target_dir())
            || path_is_under(parent, &self.profile_downloads_dir)
        {
            Ok(parent.to_path_buf())
        } else {
            Err(DownloadPathError::OutsideDownloadsRoot)
        }
    }
}

/// Profile path resolution failures at the shell/profile trust boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfilePathError {
    InvalidAppId(String),
    MissingDataHome,
}

impl fmt::Display for ProfilePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAppId(app_id) => write!(f, "invalid app ID: {app_id}"),
            Self::MissingDataHome => write!(f, "XDG data home could not be resolved"),
        }
    }
}

impl Error for ProfilePathError {}

/// Download path validation failures at the shell/profile trust boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadPathError {
    EmptyFilename,
    UnsafeFilename,
    MissingParent,
    OutsideDownloadsRoot,
}

impl fmt::Display for DownloadPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFilename => write!(f, "download filename is empty"),
            Self::UnsafeFilename => write!(f, "download filename must be a plain file name"),
            Self::MissingParent => write!(f, "download destination has no parent directory"),
            Self::OutsideDownloadsRoot => {
                write!(f, "download destination is outside downloads roots")
            }
        }
    }
}

impl Error for DownloadPathError {}

/// Session persistence failures at the shell/profile trust boundary.
#[derive(Debug)]
pub enum ProfileSessionError {
    Io(std::io::Error),
    Store(vixen_store::StoreError),
}

impl fmt::Display for ProfileSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "profile directory error: {err}"),
            Self::Store(err) => write!(f, "profile store error: {err}"),
        }
    }
}

impl Error for ProfileSessionError {}

impl From<std::io::Error> for ProfileSessionError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<vixen_store::StoreError> for ProfileSessionError {
    fn from(err: vixen_store::StoreError) -> Self {
        Self::Store(err)
    }
}

/// Resolve profile paths for `app_id` using XDG base directories.
pub fn paths_for_app_id(app_id: &str) -> Result<ProfilePaths, ProfilePathError> {
    validate_app_id(app_id)?;
    let Some(data_home) = xdg_data_home() else {
        return Err(ProfilePathError::MissingDataHome);
    };
    Ok(paths_for_roots(app_id, data_home, xdg_download_dir()))
}

/// Resolve profile paths for the production app ID.
pub fn production_paths() -> Result<ProfilePaths, ProfilePathError> {
    paths_for_app_id(crate::config::APP_ID)
}

/// Resolve profile paths for the development app ID.
pub fn devel_paths() -> Result<ProfilePaths, ProfilePathError> {
    paths_for_app_id(crate::config::APP_ID_DEVEL)
}

/// Load the persisted shell session, creating the profile directory/database on
/// first run. Empty sessions are left empty so callers can choose their own
/// start-page fallback.
pub fn load_shell_session(paths: &ProfilePaths) -> Result<SessionRecord, ProfileSessionError> {
    let store = open_profile_store(paths)?;
    Ok(store.load_session_record()?)
}

/// Persist the shell's current tabs and active index in the profile database.
pub fn save_shell_session(
    paths: &ProfilePaths,
    record: &SessionRecord,
) -> Result<(), ProfileSessionError> {
    let store = open_profile_store(paths)?;
    Ok(store.save_session_record(record)?)
}

/// Clear selected profile data groups through the same app-ID scoped store the
/// shell uses for session restore. Callers choose whether session restore itself
/// is included via [`ClearDataSelection::session`].
pub fn clear_profile_data(
    paths: &ProfilePaths,
    selection: ClearDataSelection,
) -> Result<(), ProfileSessionError> {
    let store = open_profile_store(paths)?;
    Ok(store.clear_profile_data(selection)?)
}

/// Convert a store record into the actual tab set the shell should open.
///
/// The profile record has already been validated by `vixen-store`; this policy
/// only supplies the configured fallback start page for an empty session and
/// clamps defensively in case a caller constructed a record without validating.
pub fn shell_session_from_record(record: &SessionRecord, fallback_start_url: &str) -> ShellSession {
    if record.tabs.is_empty() {
        return ShellSession {
            tabs: vec![fallback_start_url.to_owned()],
            active_index: 0,
        };
    }
    ShellSession {
        tabs: record.tabs.clone(),
        active_index: record.active_index.min(record.tabs.len().saturating_sub(1)),
    }
}

pub fn shell_session_record(tabs: Vec<String>, active_index: usize) -> SessionRecord {
    let tabs = tabs.into_iter().take(MAX_SESSION_TABS).collect::<Vec<_>>();
    let active_index = if tabs.is_empty() {
        0
    } else {
        active_index.min(tabs.len().saturating_sub(1))
    };
    SessionRecord {
        tabs,
        active_index,
        tab_states: Vec::new(),
    }
}

/// Resolve the host user's XDG Downloads directory.
pub fn xdg_download_dir() -> Option<PathBuf> {
    if let Some(dir) = env_path("XDG_DOWNLOAD_DIR").filter(|dir| dir.is_absolute()) {
        return Some(dir);
    }

    let home = home_dir()?;
    let config_home = env_path("XDG_CONFIG_HOME")
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let user_dirs = config_home.join("user-dirs.dirs");
    if let Ok(contents) = std::fs::read_to_string(user_dirs)
        && let Some(dir) = parse_user_dirs_download_dir(&contents, &home)
    {
        return Some(dir);
    }

    Some(home.join("Downloads"))
}

fn paths_for_roots(
    app_id: &str,
    data_home: PathBuf,
    user_downloads_dir: Option<PathBuf>,
) -> ProfilePaths {
    let data_dir = data_home.join(app_id);
    ProfilePaths {
        app_id: app_id.to_owned(),
        database: data_dir.join(PROFILE_DB_FILENAME),
        profile_downloads_dir: data_dir.join(PROFILE_DOWNLOADS_DIRNAME),
        reports_dir: data_dir.join(REPORTS_DIRNAME),
        data_dir,
        user_downloads_dir,
    }
}

fn open_profile_store(paths: &ProfilePaths) -> Result<Store, ProfileSessionError> {
    std::fs::create_dir_all(&paths.data_dir)?;
    std::fs::create_dir_all(&paths.profile_downloads_dir)?;
    std::fs::create_dir_all(&paths.reports_dir)?;
    Ok(Store::open(&paths.database)?)
}

fn xdg_data_home() -> Option<PathBuf> {
    xdg_data_home_from(env_path("XDG_DATA_HOME"), home_dir())
}

fn xdg_data_home_from(data_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(data_home) = data_home.filter(|path| path.is_absolute()) {
        return Some(data_home);
    }
    home.map(|home| home.join(".local/share"))
}

fn home_dir() -> Option<PathBuf> {
    env_path("HOME").filter(|path| path.is_absolute())
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

fn validate_app_id(app_id: &str) -> Result<(), ProfilePathError> {
    if app_id.is_empty()
        || app_id.starts_with('.')
        || app_id.ends_with('.')
        || app_id.split('.').any(str::is_empty)
        || !app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(ProfilePathError::InvalidAppId(app_id.to_owned()));
    }
    Ok(())
}

fn validate_download_filename(value: &str) -> Result<&str, DownloadPathError> {
    if value.is_empty() {
        return Err(DownloadPathError::EmptyFilename);
    }
    if value.as_bytes().contains(&0) {
        return Err(DownloadPathError::UnsafeFilename);
    }
    let path = Path::new(value);
    let mut components = path.components();
    let Some(Component::Normal(_)) = components.next() else {
        return Err(DownloadPathError::UnsafeFilename);
    };
    if components.next().is_some() {
        return Err(DownloadPathError::UnsafeFilename);
    }
    if matches!(value, "." | "..") || value.contains('/') || value.contains('\\') {
        return Err(DownloadPathError::UnsafeFilename);
    }
    Ok(value)
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn parse_user_dirs_download_dir(contents: &str, home: &Path) -> Option<PathBuf> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(raw_value) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let value = parse_user_dirs_value(raw_value.trim())?;
        let path = expand_user_dirs_home(&value, home);
        if path.is_absolute() {
            return Some(path);
        }
    }
    None
}

fn parse_user_dirs_value(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(quoted) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut escaped = false;
        for ch in quoted.chars() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Some(out);
            } else {
                out.push(ch);
            }
        }
        return None;
    }
    value.split_whitespace().next().map(ToOwned::to_owned)
}

fn expand_user_dirs_home(value: &str, home: &Path) -> PathBuf {
    if value == "$HOME" || value == "${HOME}" {
        return home.to_path_buf();
    }
    if let Some(suffix) = value.strip_prefix("$HOME/") {
        return home.join(suffix);
    }
    if let Some(suffix) = value.strip_prefix("${HOME}/") {
        return home.join(suffix);
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_validation_rejects_path_like_values() {
        assert!(validate_app_id(crate::config::APP_ID).is_ok());
        assert!(validate_app_id(crate::config::APP_ID_DEVEL).is_ok());
        assert!(validate_app_id("org.vixen/evil").is_err());
        assert!(validate_app_id(".org.vixen.Vixen").is_err());
        assert!(validate_app_id("org..vixen").is_err());
    }

    #[test]
    fn data_home_prefers_absolute_xdg_value_and_falls_back_to_home() {
        assert_eq!(
            xdg_data_home_from(
                Some(PathBuf::from("/xdg/data")),
                Some(PathBuf::from("/home/v"))
            ),
            Some(PathBuf::from("/xdg/data"))
        );
        assert_eq!(
            xdg_data_home_from(
                Some(PathBuf::from("relative")),
                Some(PathBuf::from("/home/v"))
            ),
            Some(PathBuf::from("/home/v/.local/share"))
        );
        assert_eq!(xdg_data_home_from(None, None), None);
    }

    #[test]
    fn profile_paths_are_app_id_scoped() {
        let paths = paths_for_roots(
            crate::config::APP_ID_DEVEL,
            PathBuf::from("/data"),
            Some(PathBuf::from("/home/v/Downloads")),
        );

        assert_eq!(paths.data_dir, PathBuf::from("/data/org.vixen.Vixen.Devel"));
        assert_eq!(
            paths.database,
            PathBuf::from("/data/org.vixen.Vixen.Devel/profile.redb")
        );
        assert_eq!(
            paths.profile_downloads_dir,
            PathBuf::from("/data/org.vixen.Vixen.Devel/downloads")
        );
        assert_eq!(
            paths.user_downloads_dir,
            Some(PathBuf::from("/home/v/Downloads"))
        );
    }

    #[test]
    fn download_destination_prefers_user_downloads_dir() {
        let paths = paths_for_roots(
            crate::config::APP_ID,
            PathBuf::from("/data"),
            Some(PathBuf::from("/home/v/Downloads")),
        );

        assert_eq!(
            paths.download_destination("report.pdf").unwrap(),
            PathBuf::from("/home/v/Downloads/report.pdf")
        );
    }

    #[test]
    fn download_destination_falls_back_to_profile_downloads_dir() {
        let paths = paths_for_roots(crate::config::APP_ID, PathBuf::from("/data"), None);

        assert_eq!(
            paths.download_destination("archive.tar").unwrap(),
            PathBuf::from("/data/org.vixen.Vixen/downloads/archive.tar")
        );
    }

    #[test]
    fn download_destination_rejects_path_traversal() {
        let paths = paths_for_roots(
            crate::config::APP_ID,
            PathBuf::from("/data"),
            Some(PathBuf::from("/home/v/Downloads")),
        );

        assert_eq!(
            paths.download_destination("../secret").unwrap_err(),
            DownloadPathError::UnsafeFilename
        );
        assert_eq!(
            paths.download_destination("nested/file.txt").unwrap_err(),
            DownloadPathError::UnsafeFilename
        );
        assert_eq!(
            paths.download_destination("bad\0name").unwrap_err(),
            DownloadPathError::UnsafeFilename
        );
    }

    #[test]
    fn show_in_folder_is_limited_to_download_roots() {
        let paths = paths_for_roots(
            crate::config::APP_ID,
            PathBuf::from("/data"),
            Some(PathBuf::from("/home/v/Downloads")),
        );

        assert_eq!(
            paths
                .show_in_folder_dir(Path::new("/home/v/Downloads/report.pdf"))
                .unwrap(),
            PathBuf::from("/home/v/Downloads")
        );
        assert_eq!(
            paths
                .show_in_folder_dir(Path::new("/data/org.vixen.Vixen/downloads/report.pdf"))
                .unwrap(),
            PathBuf::from("/data/org.vixen.Vixen/downloads")
        );
        assert_eq!(
            paths
                .show_in_folder_dir(Path::new("/etc/passwd"))
                .unwrap_err(),
            DownloadPathError::OutsideDownloadsRoot
        );
    }

    #[test]
    fn user_dirs_download_parser_expands_home() {
        let contents = r#"
            # comment
            XDG_DESKTOP_DIR="$HOME/Desktop"
            XDG_DOWNLOAD_DIR="$HOME/Downloads"
        "#;

        assert_eq!(
            parse_user_dirs_download_dir(contents, Path::new("/home/v")),
            Some(PathBuf::from("/home/v/Downloads"))
        );
    }

    #[test]
    fn user_dirs_download_parser_accepts_absolute_paths() {
        assert_eq!(
            parse_user_dirs_download_dir(
                "XDG_DOWNLOAD_DIR=\"/mnt/downloads\"",
                Path::new("/home/v")
            ),
            Some(PathBuf::from("/mnt/downloads"))
        );
    }

    #[test]
    fn shell_session_from_record_uses_fallback_for_empty_profiles() {
        assert_eq!(
            shell_session_from_record(&SessionRecord::default(), "about:vixen"),
            ShellSession {
                tabs: vec!["about:vixen".to_owned()],
                active_index: 0,
            }
        );
    }

    #[test]
    fn shell_session_record_clamps_active_index() {
        assert_eq!(
            shell_session_record(
                vec![
                    "https://one.test/".to_owned(),
                    "https://two.test/".to_owned()
                ],
                5,
            ),
            SessionRecord {
                tabs: vec![
                    "https://one.test/".to_owned(),
                    "https://two.test/".to_owned()
                ],
                active_index: 1,
                tab_states: Vec::new(),
            }
        );
    }

    #[test]
    fn shell_session_record_truncates_to_store_tab_limit() {
        let record = shell_session_record(
            (0..=MAX_SESSION_TABS)
                .map(|index| format!("https://{index}.test/"))
                .collect(),
            MAX_SESSION_TABS,
        );

        assert_eq!(record.tabs.len(), MAX_SESSION_TABS);
        assert_eq!(record.active_index, MAX_SESSION_TABS - 1);
    }

    #[test]
    fn shell_session_round_trips_through_profile_store() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_for_roots(crate::config::APP_ID_DEVEL, dir.path().to_path_buf(), None);
        let record = shell_session_record(
            vec!["about:vixen".to_owned(), "https://example.test/".to_owned()],
            1,
        );

        save_shell_session(&paths, &record).unwrap();

        assert_eq!(load_shell_session(&paths).unwrap(), record);
        assert!(paths.database.exists());
        assert!(paths.profile_downloads_dir.is_dir());
        assert!(paths.reports_dir.is_dir());
    }

    #[test]
    fn clear_profile_data_can_preserve_or_clear_shell_session() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_for_roots(crate::config::APP_ID_DEVEL, dir.path().to_path_buf(), None);
        let record = shell_session_record(
            vec!["about:vixen".to_owned(), "https://example.test/".to_owned()],
            1,
        );
        save_shell_session(&paths, &record).unwrap();

        clear_profile_data(&paths, ClearDataSelection::browsing_data()).unwrap();
        assert_eq!(load_shell_session(&paths).unwrap(), record);

        clear_profile_data(
            &paths,
            ClearDataSelection {
                session: true,
                ..ClearDataSelection::default()
            },
        )
        .unwrap();
        assert_eq!(
            load_shell_session(&paths).unwrap(),
            SessionRecord::default()
        );
    }
}
