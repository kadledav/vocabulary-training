//! Clip files: `<cache dir>/audio/<h0h1>/<hash>.mp3` (design §10.3 step 4),
//! owner-only (directories 0700, files 0600), written atomically (temporary
//! file, fsync, rename, fsync of the directory). The cache is regenerable
//! (R-85): a lost file only means the clip is made again.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::key::is_clip_hash;

const SUBDIR: &str = "audio";
const EXTENSION: &str = "mp3";
const TEMPORARY: &str = "tmp";

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

fn dir_builder() -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

impl Store {
    /// The store in `cache_dir` (its `audio` directory is created).
    pub fn open(cache_dir: &Path) -> io::Result<Self> {
        let dir = cache_dir.join(SUBDIR);
        dir_builder().create(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir
            .join(&hash[..2])
            .join(format!("{hash}.{EXTENSION}"))
    }

    /// Write a clip (replacing nothing: a clip's content is fixed by its hash).
    pub fn write(&self, hash: &str, bytes: &[u8]) -> io::Result<()> {
        if !is_clip_hash(hash) {
            return Err(io::Error::other("not a clip hash"));
        }
        let path = self.path(hash);
        let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
        dir_builder().create(parent)?;
        let temporary = parent.join(format!("{hash}.{TEMPORARY}"));
        let _ = fs::remove_file(&temporary);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        if let Err(err) = written {
            let _ = fs::remove_file(&temporary);
            return Err(err);
        }
        fs::rename(&temporary, &path)?;
        File::open(parent)?.sync_all()
    }

    /// The clip's file, if there is one.
    pub fn open_clip(&self, hash: &str) -> io::Result<Option<File>> {
        if !is_clip_hash(hash) {
            return Ok(None);
        }
        match File::open(self.path(hash)) {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn remove(&self, hash: &str) -> io::Result<()> {
        if !is_clip_hash(hash) {
            return Ok(());
        }
        match fs::remove_file(self.path(hash)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    /// The hashes of all clip files. Left-over temporary files (a write
    /// interrupted by a crash) and anything else that is not a clip file
    /// in a clip directory are deleted.
    pub fn scan(&self) -> io::Result<Vec<String>> {
        let mut hashes = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let bucket = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir() || bucket.len() != 2 {
                continue;
            }
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                let name = file.file_name().to_string_lossy().into_owned();
                match name.strip_suffix(&format!(".{EXTENSION}")) {
                    Some(hash) if is_clip_hash(hash) && hash.starts_with(&bucket) => {
                        hashes.push(hash.to_owned());
                    }
                    _ if file.file_type()?.is_file() => fs::remove_file(file.path())?,
                    _ => {}
                }
            }
        }
        hashes.sort();
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn r40_clips_are_written_atomically_and_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        store.write(HASH, b"ID3-not-really").expect("write");
        assert!(store.open_clip(HASH).expect("open").is_some());
        let path = dir.path().join("audio/01").join(format!("{HASH}.mp3"));
        assert_eq!(fs::read(&path).expect("read"), b"ID3-not-really");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = fs::metadata(path.parent().expect("parent"))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        assert!(store.open_clip(HASH).expect("open").is_some());
        store.remove(HASH).expect("remove");
        assert!(store.open_clip(HASH).expect("open").is_none());
        assert!(store.open_clip(HASH).expect("open").is_none());
    }

    #[test]
    fn r85_scan_lists_clips_and_removes_leftovers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        store.write(HASH, b"x").expect("write");
        let bucket = dir.path().join("audio/01");
        fs::write(bucket.join(format!("{HASH}.tmp")), b"half").expect("tmp");
        fs::write(bucket.join("junk.mp3"), b"?").expect("junk");
        assert_eq!(store.scan().expect("scan"), vec![HASH.to_owned()]);
        assert!(!bucket.join(format!("{HASH}.tmp")).exists());
        assert!(!bucket.join("junk.mp3").exists());
    }

    #[test]
    fn r83_hashes_that_are_not_clip_hashes_never_reach_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        assert!(store.write("../../etc/passwd", b"x").is_err());
        assert!(store.open_clip("../x").expect("open").is_none());
        assert!(store.remove("zz").is_ok());
    }
}
