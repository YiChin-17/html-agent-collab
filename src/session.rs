//! Session registry：`.collab/session.json` 的產生與讀取。
//! design「axum loopback control service 與單一 CLI contract」：session file 必須
//! 限制為目前使用者可讀寫（0600），token 必須來自 cryptographically secure
//! random source；CLI 之後（task 2.2）從此檔案發現 port 與 token。

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const SESSION_DIR: &str = ".collab";
pub const SESSION_FILE: &str = "session.json";
const STARTUP_LOCK_FILE: &str = "startup.lock";

#[derive(Debug)]
pub struct StartupLock {
    _file: fs::File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    pub entry_file: PathBuf,
    pub project_root: PathBuf,
}

#[derive(Debug)]
pub struct EntryError {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for EntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EntryError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionFile {
    pub session_id: String,
    pub project_root: PathBuf,
    pub entry_file: PathBuf,
    pub port: u16,
    /// control operations 的 bearer token。不得出現在任何正常命令輸出或 log。
    pub token: String,
    pub pid: u32,
    pub created_at_epoch_secs: u64,
}

impl SessionFile {
    pub fn new(project_root: PathBuf, entry_file: PathBuf, port: u16, token: String) -> Self {
        SessionFile {
            session_id: generate_session_id(),
            project_root,
            entry_file,
            port,
            token,
            pid: std::process::id(),
            created_at_epoch_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

pub fn resolve_entry(input: &Path) -> Result<ResolvedEntry, EntryError> {
    let candidate = if input.is_dir() {
        input.join("index.html")
    } else {
        input.to_path_buf()
    };
    let entry_file = candidate.canonicalize().map_err(|error| EntryError {
        code: "invalid-entry",
        message: format!(
            "cannot resolve preview entry {}: {error}",
            candidate.display()
        ),
    })?;
    let metadata = fs::metadata(&entry_file).map_err(|error| EntryError {
        code: "invalid-entry",
        message: format!(
            "cannot inspect preview entry {}: {error}",
            entry_file.display()
        ),
    })?;
    let is_html = entry_file
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
    if !metadata.is_file() || !is_html {
        return Err(EntryError {
            code: "invalid-entry",
            message: format!(
                "preview entry {} must be a readable HTML file",
                entry_file.display()
            ),
        });
    }
    fs::File::open(&entry_file).map_err(|error| EntryError {
        code: "invalid-entry",
        message: format!(
            "preview entry {} is not readable: {error}",
            entry_file.display()
        ),
    })?;
    let project_root = entry_file
        .parent()
        .expect("canonical file path always has a parent")
        .to_path_buf();
    Ok(ResolvedEntry {
        entry_file,
        project_root,
    })
}

/// 以 CSPRNG 產生 64 hex 字元（32 bytes）的 control token。
pub fn generate_token() -> String {
    random_hex(32)
}

pub fn generate_session_id() -> String {
    random_hex(8)
}

fn random_hex(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    getrandom::fill(&mut buf).expect("secure random source unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn session_file_path(project_root: &Path) -> PathBuf {
    project_root.join(SESSION_DIR).join(SESSION_FILE)
}

fn prepare_session_dir(project_root: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let dir = project_root.join(SESSION_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session registry directory must not be a symlink",
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session registry path must be a directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&dir)?,
        Err(error) => return Err(error),
    }
    if fs::symlink_metadata(&dir)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session registry directory must not be a symlink",
        ));
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

pub(crate) fn prepare_private_subdir(project_root: &Path, name: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let session_dir = prepare_session_dir(project_root)?;
    let dir = session_dir.join(name);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private artifact path {} must be a real directory",
                    dir.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&dir)?,
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(&dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "private artifact path {} must be a real directory",
                dir.display()
            ),
        ));
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

pub(crate) fn write_artifact_bytes(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    write_artifact_bytes_with_tokens(path, bytes, (0..16).map(|_| random_hex(8)))
}

fn write_artifact_bytes_with_tokens<I, S>(
    path: &Path,
    bytes: &[u8],
    tokens: I,
) -> io::Result<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact destination must have a parent directory",
        )
    })?;
    let dir_metadata = fs::symlink_metadata(dir)?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact parent {} must be a real directory", dir.display()),
        ));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact destination must have a UTF-8 file name",
            )
        })?;
    let mut opened = None;
    for token in tokens {
        let temporary = dir.join(format!(".{file_name}.{}.tmp", token.as_ref()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
        {
            Ok(file) => {
                opened = Some((temporary, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let Some((temporary, mut file)) = opened else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate a unique artifact temporary file",
        ));
    };

    let write_result = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o600)))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(path.to_path_buf())
}

pub fn acquire_startup_lock(project_root: &Path) -> io::Result<StartupLock> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = prepare_session_dir(project_root)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(STARTUP_LOCK_FILE))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        let contended = error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN);
        return Err(if contended {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "another preview startup owns this project",
            )
        } else {
            error
        });
    }
    Ok(StartupLock { _file: file })
}

/// atomic write（temp file + rename），目錄 0700、檔案 0600。
pub fn write_session_file(session: &SessionFile) -> io::Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;

    let dir = prepare_session_dir(&session.project_root)?;

    let path = dir.join(SESSION_FILE);
    let body = serde_json::to_string_pretty(session)?;
    let mut opened = None;
    for _ in 0..16 {
        let tmp = dir.join(format!(".{SESSION_FILE}.{}.tmp", random_hex(8)));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
        {
            Ok(file) => {
                opened = Some((tmp, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let Some((tmp, mut file)) = opened else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate a unique session temporary file",
        ));
    };

    if let Err(error) = file.write_all(body.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    // OpenOptions mode 受 umask 影響，rename 前再強制一次 0600。
    if let Err(error) = file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(path)
}

pub fn read_session_file(project_root: &Path) -> io::Result<SessionFile> {
    let raw = fs::read_to_string(session_file_path(project_root))?;
    serde_json::from_str(&raw).map_err(io::Error::from)
}

pub fn remove_session_file(project_root: &Path) -> io::Result<()> {
    fs::remove_file(session_file_path(project_root))
}

pub fn remove_session_file_if_owned(project_root: &Path, session_id: &str) -> io::Result<bool> {
    let _startup_lock = acquire_startup_lock(project_root)?;
    let current = match read_session_file(project_root) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if current.session_id != session_id {
        return Ok(false);
    }
    fs::remove_file(session_file_path(project_root))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "collab-session-entry-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn token_is_64_hex_chars_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_is_16_hex_chars() {
        let id = generate_session_id();
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolves_html_file_and_directory_index_to_the_same_canonical_entry() {
        let root = temp_root("canonical");
        let entry = root.join("index.html");
        fs::write(&entry, "<!doctype html>").unwrap();

        let from_file = resolve_entry(&root.join("./index.html")).unwrap();
        let from_directory = resolve_entry(&root).unwrap();

        assert_eq!(from_file, from_directory);
        assert_eq!(from_file.entry_file, entry.canonicalize().unwrap());
        assert_eq!(from_file.project_root, root);
    }

    #[test]
    fn rejects_missing_non_html_and_directory_without_index() {
        let root = temp_root("invalid");
        let text = root.join("notes.txt");
        fs::write(&text, "not html").unwrap();
        let empty_directory = root.join("empty");
        fs::create_dir(&empty_directory).unwrap();

        for input in [root.join("missing.html"), text, empty_directory] {
            let error = resolve_entry(&input).unwrap_err();
            assert_eq!(error.code, "invalid-entry");
        }
    }

    #[test]
    fn rejects_unreadable_html_entry() {
        let root = temp_root("unreadable");
        let entry = root.join("index.html");
        fs::write(&entry, "<!doctype html>").unwrap();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o000)).unwrap();

        let error = resolve_entry(&entry).unwrap_err();

        fs::set_permissions(&entry, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(error.code, "invalid-entry");
    }

    #[test]
    fn session_json_round_trip_includes_canonical_entry_file() {
        let root = temp_root("round-trip");
        let entry = root.join("landing.html");
        fs::write(&entry, "<!doctype html>").unwrap();
        let entry = entry.canonicalize().unwrap();
        let session = SessionFile::new(root.clone(), entry.clone(), 43123, "secret-token".into());

        let json = serde_json::to_value(&session).unwrap();
        assert_eq!(json["entryFile"], entry.to_str().unwrap());
        assert_eq!(
            serde_json::from_value::<SessionFile>(json)
                .unwrap()
                .entry_file,
            entry
        );
    }

    #[test]
    fn session_write_does_not_follow_preplaced_fixed_temp_symlink() {
        let root = temp_root("temp-symlink");
        let dir = root.join(SESSION_DIR);
        fs::create_dir_all(&dir).unwrap();
        let victim = root.join("victim.txt");
        let original = b"victim must not be truncated";
        fs::write(&victim, original).unwrap();
        symlink(&victim, dir.join(format!("{SESSION_FILE}.tmp"))).unwrap();
        let session = SessionFile::new(
            root.clone(),
            root.join("index.html"),
            43123,
            "secret-token".into(),
        );

        write_session_file(&session).unwrap();

        assert_eq!(fs::read(victim).unwrap(), original);
        assert_eq!(read_session_file(&root).unwrap(), session);
    }

    #[test]
    fn session_write_rejects_symlinked_registry_directory() {
        let root = temp_root("registry-symlink");
        let registry_target = root.join("registry-target");
        fs::create_dir_all(&registry_target).unwrap();
        symlink(&registry_target, root.join(SESSION_DIR)).unwrap();
        let session = SessionFile::new(
            root.clone(),
            root.join("index.html"),
            43123,
            "secret-token".into(),
        );

        let error = write_session_file(&session).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!registry_target.join(SESSION_FILE).exists());
    }

    #[test]
    fn private_subdirectory_rejects_symlink_and_non_directory() {
        let root = temp_root("private-subdirectory-invalid");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(root.join(SESSION_DIR)).unwrap();
        symlink(&target, root.join(SESSION_DIR).join("feedback")).unwrap();
        fs::write(
            root.join(SESSION_DIR).join("screenshots"),
            b"not a directory",
        )
        .unwrap();

        assert_eq!(
            prepare_private_subdir(&root, "feedback")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            prepare_private_subdir(&root, "screenshots")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn private_subdirectory_enforces_owner_only_permissions() {
        let root = temp_root("private-subdirectory-permissions");
        let dir = root.join(SESSION_DIR).join("feedback");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();

        assert_eq!(prepare_private_subdir(&root, "feedback").unwrap(), dir);
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn artifact_writer_skips_symlink_candidate_and_preserves_victim() {
        let root = temp_root("artifact-candidate-symlink");
        let dir = prepare_private_subdir(&root, "feedback").unwrap();
        let destination = dir.join("artifact.json");
        let victim = root.join("victim.txt");
        let original = b"candidate symlink victim";
        fs::write(&victim, original).unwrap();
        let collision = dir.join(".artifact.json.collision.tmp");
        symlink(&victim, &collision).unwrap();

        assert_eq!(
            write_artifact_bytes_with_tokens(
                &destination,
                b"complete artifact",
                ["collision", "safe"],
            )
            .unwrap(),
            destination
        );

        assert_eq!(fs::read(&victim).unwrap(), original);
        assert_eq!(fs::read(&destination).unwrap(), b"complete artifact");
        assert!(
            fs::symlink_metadata(collision)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn artifact_writer_bounds_collisions_without_removing_foreign_entries() {
        let root = temp_root("artifact-candidate-exhaustion");
        let dir = prepare_private_subdir(&root, "feedback").unwrap();
        let destination = dir.join("artifact.json");
        let victim = root.join("victim.txt");
        let original = b"all candidate victims stay unchanged";
        fs::write(&victim, original).unwrap();
        for token in ["first", "second"] {
            symlink(&victim, dir.join(format!(".artifact.json.{token}.tmp"))).unwrap();
        }

        assert_eq!(
            write_artifact_bytes_with_tokens(
                &destination,
                b"must not publish",
                ["first", "second"],
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );

        assert_eq!(fs::read(&victim).unwrap(), original);
        assert!(!destination.exists());
        for token in ["first", "second"] {
            assert!(
                fs::symlink_metadata(dir.join(format!(".artifact.json.{token}.tmp")))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn artifact_writer_cleans_only_its_temporary_file_after_rename_failure() {
        let root = temp_root("artifact-rename-failure");
        let dir = prepare_private_subdir(&root, "feedback").unwrap();
        let destination = dir.join("artifact.json");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep.txt"), b"keep").unwrap();
        let unrelated = dir.join(".unrelated.tmp");
        fs::write(&unrelated, b"unrelated").unwrap();

        assert!(
            write_artifact_bytes_with_tokens(&destination, b"must not publish", ["owned"],)
                .is_err()
        );

        assert!(!dir.join(".artifact.json.owned.tmp").exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated");
        assert_eq!(fs::read(destination.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn startup_lock_allows_only_one_project_owner() {
        let root = temp_root("startup-lock");

        let first = acquire_startup_lock(&root).unwrap();
        let contender_root = root.clone();
        let second = std::thread::spawn(move || acquire_startup_lock(&contender_root).unwrap_err())
            .join()
            .unwrap();

        assert_eq!(second.kind(), io::ErrorKind::WouldBlock);
        drop(first);
        acquire_startup_lock(&root).unwrap();
    }

    #[test]
    fn cleanup_does_not_remove_another_sessions_registry() {
        let root = temp_root("session-scoped-cleanup");
        let mut current = SessionFile::new(
            root.clone(),
            root.join("index.html"),
            43123,
            "new-token".into(),
        );
        current.session_id = "new-session".into();
        write_session_file(&current).unwrap();

        assert!(!remove_session_file_if_owned(&root, "old-session").unwrap());
        assert_eq!(read_session_file(&root).unwrap(), current);
        assert!(remove_session_file_if_owned(&root, "new-session").unwrap());
        assert!(read_session_file(&root).is_err());
    }
}
