//! File writes that never leave a file half-written, and private `0700`/`0600` runtime temp storage.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static NEXT: AtomicU64 = AtomicU64::new(0);
const MAX_PRUNE_DEPTH: usize = 8;

/// Resolve `path` for an atomic write, following valid symlinks to existing files while
/// refusing dangling symlinks so a fallback or create never escapes through an uncreated target.
fn resolve_atomic_target(path: &Path) -> io::Result<PathBuf> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return path.canonicalize().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to write through dangling symlink: {}", path.display()),
            )
        });
    }
    match path.canonicalize() {
        Ok(target) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(error),
    }
}

fn write_in_place_fallback(target: &Path, contents: &[u8], _private: bool) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to write through symlink: {}", target.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    if _private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(target)?;
    #[cfg(unix)]
    if _private {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.set_len(0)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn write_atomically_inner(path: &Path, contents: &[u8], private: bool) -> io::Result<()> {
    let target = resolve_atomic_target(path)?;
    let (Some(directory), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file path", path.display())));
    };
    let temporary =
        directory.join(format!(".{}.odg-{}-{}.tmp", name.to_string_lossy(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));

    let written = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if private {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            } else if let Ok(metadata) = fs::metadata(&target) {
                file.set_permissions(metadata.permissions())?;
            }
        }
        file.write_all(contents)?;
        file.sync_all()
    })();
    let result = written.and_then(|()| match fs::rename(&temporary, &target) {
        Ok(()) => {
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        Err(_) => write_in_place_fallback(&target, contents, private),
    });
    if fs::symlink_metadata(&temporary).is_ok() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Replace `path` with `contents`: they go to a temporary file beside it,
/// which then takes the file's place in one rename, so a crash leaves either
/// the old contents or the new ones. A link to an existing file is followed,
/// while a dangling symlink is refused; on Unix the file keeps its permissions.
/// When the rename is refused (Windows refuses it while another program holds
/// the file open), the file is written in place instead.
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomically_inner(path, contents, false)
}

/// Atomically write sensitive configuration to `path`, enforcing `0o600` on both the
/// temporary staging file and the final target on Unix, and refusing dangling symlinks.
pub fn write_private_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomically_inner(path, contents, true)
}

fn validate_single_subdir(subdir: &str) -> io::Result<()> {
    if subdir.is_empty() || subdir == "." || subdir == ".." || subdir.contains(['/', '\\', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid private temp subdir: {subdir:?}"),
        ));
    }
    let mut components = Path::new(subdir).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid private temp subdir: {subdir:?}"),
        ));
    }
    Ok(())
}

fn ensure_base_home_dir(base_home: &Path) -> io::Result<()> {
    if fs::symlink_metadata(base_home).is_err() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().recursive(true).mode(0o700).create(base_home)?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(base_home)?;
        }
    }
    #[cfg(unix)]
    if let Ok(meta) = fs::symlink_metadata(base_home)
        && !meta.file_type().is_symlink()
        && meta.is_dir()
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(base_home, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_strict_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing to use symlink as private temp directory: {}", dir.display()),
                ));
            }
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private temp path is not a directory: {}", dir.display()),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(dir)
            };
            #[cfg(not(unix))]
            let created = fs::create_dir(dir);
            match created {
                Ok(()) => {}
                Err(race) if race.kind() == io::ErrorKind::AlreadyExists => {}
                Err(race) => return Err(race),
            }
        }
        Err(error) => return Err(error),
    }

    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to use symlink as private temp directory: {}", dir.display()),
        ));
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("private temp path is not a directory: {}", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn private_temp_dir_in(base_home: &Path, subdir: &str) -> io::Result<PathBuf> {
    validate_single_subdir(subdir)?;
    ensure_base_home_dir(base_home)?;
    let tmp_root = base_home.join("tmp");
    ensure_strict_private_dir(&tmp_root)?;
    let target = tmp_root.join(subdir);
    ensure_strict_private_dir(&target)?;
    Ok(target)
}

/// Ensure and return a private `0700` temporary directory at `<ODG_HOME>/tmp/<subdir>`,
/// rejecting traversal components and symlink squatting on `tmp` or `<subdir>`.
pub fn private_temp_dir(subdir: &str) -> io::Result<PathBuf> {
    private_temp_dir_in(&crate::settings::home(), subdir)
}

pub(crate) fn create_private_subdir_in(base_home: &Path, subdir: &str) -> io::Result<PathBuf> {
    let parent = private_temp_dir_in(base_home, subdir)?;
    let child = parent.join(uuid::Uuid::now_v7().to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().mode(0o700).create(&child)?;
        let meta = fs::symlink_metadata(&child)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to use non-directory private subdir: {}", child.display()),
            ));
        }
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(&child)?;
    }
    Ok(child)
}

/// Create a fresh empty `0700` UUIDv7 subdirectory inside `private_temp_dir(subdir)`
/// so per-turn directory mounts (such as `agy --add-dir`) never expose sibling files.
pub fn create_private_subdir(subdir: &str) -> io::Result<PathBuf> {
    create_private_subdir_in(&crate::settings::home(), subdir)
}

fn sanitize_affix(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch == '/' || ch == '\\' || ch == '\0' || ch.is_control() { '_' } else { ch })
        .collect::<String>()
        .replace("..", "_")
}

pub(crate) fn write_private_new_in(
    base_home: &Path,
    subdir: &str,
    prefix: &str,
    suffix: &str,
    contents: &[u8],
) -> io::Result<PathBuf> {
    let dir = private_temp_dir_in(base_home, subdir)?;
    let safe_prefix = sanitize_affix(prefix);
    let safe_suffix = sanitize_affix(suffix);
    let filename = format!(
        "{safe_prefix}{}-{}{safe_suffix}",
        uuid::Uuid::now_v7(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = dir.join(filename);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    let written = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(contents)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Create a brand-new `0600` file with an unguessable UUIDv7 name inside `private_temp_dir(subdir)`,
/// write `contents` to it, and return its full path.
pub fn write_private_new(subdir: &str, prefix: &str, suffix: &str, contents: &[u8]) -> io::Result<PathBuf> {
    write_private_new_in(&crate::settings::home(), subdir, prefix, suffix, contents)
}

fn is_stale(meta: &fs::Metadata, now: SystemTime, max_age: Duration) -> bool {
    meta.modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > max_age)
}

fn prune_item(path: &Path, now: SystemTime, max_age: Duration, depth: usize) {
    if depth > MAX_PRUNE_DEPTH {
        return;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        let dir_stale = is_stale(&meta, now, max_age);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                prune_item(&entry.path(), now, max_age, depth + 1);
            }
        }
        if dir_stale {
            let _ = fs::remove_dir(path);
        }
    } else if is_stale(&meta, now, max_age) {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn prune_stale_temp_files_in(base_home: &Path, max_age: Duration) {
    let tmp_root = base_home.join("tmp");
    let Ok(root_meta) = fs::symlink_metadata(&tmp_root) else {
        return;
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return;
    }
    let Ok(subdirs) = fs::read_dir(&tmp_root) else {
        return;
    };
    let now = SystemTime::now();
    for subdir in subdirs.flatten() {
        let subdir_path = subdir.path();
        let Ok(sub_meta) = fs::symlink_metadata(&subdir_path) else {
            continue;
        };
        if sub_meta.file_type().is_symlink() || !sub_meta.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&subdir_path) else {
            continue;
        };
        for entry in entries.flatten() {
            prune_item(&entry.path(), now, max_age, 1);
        }
    }
}

/// Silently sweep `<ODG_HOME>/tmp/<subdir>/*`, skipping symlinks and removing files or
/// empty subdirectories whose `modified()` timestamp is older than `max_age`.
pub fn prune_stale_temp_files(max_age: Duration) {
    prune_stale_temp_files_in(&crate::settings::home(), max_age);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftovers(directory: &Path) -> Vec<String> {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn creates_and_replaces_a_file_without_leaving_temporaries() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.txt");
        write_atomically(&file, b"one").unwrap();
        write_atomically(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        assert!(leftovers(directory.path()).is_empty());
        assert!(write_atomically(&directory.path().join("missing/a.txt"), b"x").is_err());
        assert!(leftovers(directory.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permissions_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("run.sh");
        fs::write(&script, "echo 1\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let link = directory.path().join("link.sh");
        std::os::unix::fs::symlink(&script, &link).unwrap();

        write_atomically(&link, b"echo 2\n").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&script).unwrap(), b"echo 2\n");
        assert_eq!(fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomically_rejects_dangling_symlink_without_creating_target() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let missing_external = outside.path().join("escaped.txt");
        let dangling = ws.path().join("dangling.txt");
        std::os::unix::fs::symlink(&missing_external, &dangling).unwrap();

        let err = write_atomically(&dangling, b"secret").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!missing_external.exists());
        assert!(leftovers(ws.path()).is_empty());

        let priv_err = write_private_atomically(&dangling, b"secret").unwrap_err();
        assert_eq!(priv_err.kind(), io::ErrorKind::InvalidInput);
        assert!(!missing_external.exists());
        assert!(leftovers(ws.path()).is_empty());
    }

    #[test]
    fn private_temp_dir_rejects_invalid_subdir_names() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".odg");
        for bad in ["", ".", "..", "a/b", "a\\b", "../escape", "a/../b", "mcp\0bad"] {
            let err = private_temp_dir_in(&home, bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "expected InvalidInput for {bad:?}");
        }
    }

    #[test]
    fn private_temp_dir_and_files_enforce_permissions_and_sanitize_affixes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".odg");

        let mcp_dir = private_temp_dir_in(&home, "mcp").unwrap();
        assert_eq!(mcp_dir, home.join("tmp").join("mcp"));

        let prompt_subdir = create_private_subdir_in(&home, "prompts").unwrap();
        assert_eq!(prompt_subdir.parent(), Some(home.join("tmp").join("prompts").as_path()));
        assert!(prompt_subdir.is_dir());

        let secret_file = write_private_new_in(&home, "mcp", "mcp-", ".json", br#"{"token":"secret"}"#).unwrap();
        assert_eq!(secret_file.parent(), Some(mcp_dir.as_path()));
        let name = secret_file.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("mcp-") && name.ends_with(".json"));
        assert_eq!(fs::read(&secret_file).unwrap(), br#"{"token":"secret"}"#);

        let sanitized = write_private_new_in(&home, "mcp", "../evil/", "\\..\\.json", b"safe").unwrap();
        assert_eq!(sanitized.parent(), Some(mcp_dir.as_path()));
        assert_eq!(fs::read(&sanitized).unwrap(), b"safe");

        let cfg_path = root.path().join("mcp.json");
        fs::write(&cfg_path, b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private_atomically(&cfg_path, br#"{"mcpServers":{}}"#).unwrap();
        assert_eq!(fs::read(&cfg_path).unwrap(), br#"{"mcpServers":{}}"#);
        assert!(leftovers(root.path()).is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::symlink_metadata(&home).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::symlink_metadata(home.join("tmp")).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::symlink_metadata(&mcp_dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::symlink_metadata(&prompt_subdir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::symlink_metadata(&secret_file).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::symlink_metadata(&sanitized).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::symlink_metadata(&cfg_path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_temp_dir_rejects_symlinked_tmp_or_subdir() {
        use std::os::unix::fs::PermissionsExt;

        // Case 1: `<home>/tmp` pre-created as a symlink.
        let root1 = tempfile::tempdir().unwrap();
        let home1 = root1.path().join(".odg");
        fs::create_dir_all(&home1).unwrap();
        let outside1 = tempfile::tempdir().unwrap();
        fs::set_permissions(outside1.path(), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(outside1.path(), home1.join("tmp")).unwrap();

        let err1 = private_temp_dir_in(&home1, "mcp").unwrap_err();
        assert_eq!(err1.kind(), io::ErrorKind::InvalidInput);
        assert!(!outside1.path().join("mcp").exists());
        assert_eq!(fs::metadata(outside1.path()).unwrap().permissions().mode() & 0o777, 0o755);

        // Case 2: `<home>/tmp/<subdir>` pre-created as a symlink.
        let root2 = tempfile::tempdir().unwrap();
        let home2 = root2.path().join(".odg");
        let _ = private_temp_dir_in(&home2, "images").unwrap();
        let outside2 = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside2.path(), home2.join("tmp").join("mcp")).unwrap();

        let err2 = private_temp_dir_in(&home2, "mcp").unwrap_err();
        assert_eq!(err2.kind(), io::ErrorKind::InvalidInput);
        let write_err = write_private_new_in(&home2, "mcp", "mcp-", ".json", b"secret").unwrap_err();
        assert_eq!(write_err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_dir(outside2.path()).unwrap().count(), 0);
    }

    #[test]
    fn prune_stale_temp_files_removes_expired_items_and_skips_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".odg");
        let old_time = SystemTime::now() - Duration::from_secs(7200);

        let stale_file = write_private_new_in(&home, "mcp", "old-", ".json", b"old").unwrap();
        OpenOptions::new().write(true).open(&stale_file).unwrap().set_modified(old_time).unwrap();

        let fresh_file = write_private_new_in(&home, "mcp", "new-", ".json", b"new").unwrap();

        let stale_subdir = create_private_subdir_in(&home, "prompts").unwrap();
        let stale_prompt = stale_subdir.join("prompt.txt");
        fs::write(&stale_prompt, b"old prompt").unwrap();
        OpenOptions::new().write(true).open(&stale_prompt).unwrap().set_modified(old_time).unwrap();
        #[cfg(unix)]
        fs::File::open(&stale_subdir).unwrap().set_modified(old_time).unwrap();

        let fresh_subdir = create_private_subdir_in(&home, "prompts").unwrap();
        let fresh_prompt = fresh_subdir.join("prompt.txt");
        fs::write(&fresh_prompt, b"fresh prompt").unwrap();

        #[cfg(unix)]
        let (outside, symlink_file) = {
            let outside = tempfile::tempdir().unwrap();
            let ext_file = outside.path().join("keep.txt");
            fs::write(&ext_file, b"keep").unwrap();
            OpenOptions::new().write(true).open(&ext_file).unwrap().set_modified(old_time).unwrap();
            let link_in_subdir = home.join("tmp").join("mcp").join("keep-link.json");
            std::os::unix::fs::symlink(&ext_file, &link_in_subdir).unwrap();
            let link_subdir = home.join("tmp").join("linked-dir");
            std::os::unix::fs::symlink(outside.path(), &link_subdir).unwrap();
            (outside, link_in_subdir)
        };

        prune_stale_temp_files_in(&home, Duration::from_secs(3600));

        assert!(!stale_file.exists());
        assert!(fresh_file.exists());
        assert!(!stale_prompt.exists());
        #[cfg(unix)]
        assert!(!stale_subdir.exists());
        assert!(fresh_prompt.exists());
        assert!(fresh_subdir.exists());

        #[cfg(unix)]
        {
            assert!(outside.path().join("keep.txt").exists());
            assert!(fs::symlink_metadata(&symlink_file).is_ok());
        }
    }
}
