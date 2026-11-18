//! File writes that never leave a file half-written.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `contents`: they go to a temporary file beside it,
/// which then takes the file's place in one rename, so a crash leaves either
/// the old contents or the new ones. A link is followed, so the file it
/// points to is replaced and the link stays; on Unix the file keeps its
/// permissions. When the rename is refused (Windows refuses it while another
/// program holds the file open), the file is written in place instead.
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let target = match path.canonicalize() {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let (Some(directory), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file path", path.display())));
    };
    let temporary =
        directory.join(format!(".{}.odg-{}-{}.tmp", name.to_string_lossy(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));

    let written = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()
    })();
    let result = written.and_then(|()| fs::rename(&temporary, &target).or_else(|_| fs::write(&target, contents)));
    if fs::symlink_metadata(&temporary).is_ok() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
}
