// SPDX-License-Identifier: MIT
//! Read-only access to a file tree confined to a configured root.
//!
//! Every path is resolved through the filesystem (following symlinks) and
//! must stay inside the canonical root, so neither `..` components nor
//! symlinks can reach files outside it. Symlinks that point within the root
//! are allowed.
//!
//! The check and the subsequent read are not atomic: an attacker with write
//! access to the tree on the host could swap in a symlink between them. The
//! root is assumed to be under the operator's control.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// A browsable directory tree.
pub struct Root {
    path: PathBuf,
}

/// One listed directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File { size: u64 },
}

impl Root {
    pub fn new(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        if !path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a directory", path.display()),
            ));
        }
        Ok(Self { path })
    }

    /// The canonical root directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve `rel`, a path relative to the root, to a real path inside it.
    pub fn resolve(&self, rel: &Path) -> io::Result<PathBuf> {
        for component in rel.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(outside(rel)),
            }
        }
        let full = self.path.join(rel).canonicalize()?;
        if !full.starts_with(&self.path) {
            return Err(outside(rel));
        }
        Ok(full)
    }

    /// List the directory `rel`: directories first, then files, each sorted
    /// case-insensitively. Hidden entries, non-UTF-8 names, special files,
    /// and anything resolving outside the root are omitted.
    pub fn list(&self, rel: &Path) -> io::Result<Vec<Entry>> {
        let dir = self.resolve(rel)?;
        let mut entries = Vec::new();
        for item in fs::read_dir(dir)? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(real) = self.resolve(&rel.join(&name)) else {
                continue;
            };
            let Ok(meta) = fs::metadata(&real) else {
                continue;
            };
            let kind = if meta.is_dir() {
                Kind::Dir
            } else if meta.is_file() {
                Kind::File { size: meta.len() }
            } else {
                continue;
            };
            entries.push(Entry { name, kind });
        }
        entries.sort_by_key(|e| (e.kind != Kind::Dir, e.name.to_lowercase()));
        Ok(entries)
    }

    /// Read the regular file `rel` and its modification time.
    pub fn read_file(&self, rel: &Path) -> io::Result<Contents> {
        let real = self.resolve(rel)?;
        let meta = fs::metadata(&real)?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", rel.display()),
            ));
        }
        Ok(Contents {
            data: fs::read(real)?,
            modified: meta.modified().ok(),
        })
    }
}

/// A file read through [`Root::read_file`].
pub struct Contents {
    pub data: Vec<u8>,
    pub modified: Option<SystemTime>,
}

fn outside(rel: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is outside the served directory", rel.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// root/{a.txt, B.TXT, sub/c.txt, .hidden, inside -> sub, escape -> outside/,
    /// escape.txt -> outside/secret.txt}
    fn fixture() -> (tempfile::TempDir, Root) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fs::create_dir_all(base.join("root/sub")).unwrap();
        fs::create_dir(base.join("outside")).unwrap();
        fs::write(base.join("outside/secret.txt"), b"secret").unwrap();
        fs::write(base.join("root/a.txt"), b"aaa").unwrap();
        fs::write(base.join("root/B.TXT"), b"bb").unwrap();
        fs::write(base.join("root/.hidden"), b"h").unwrap();
        fs::write(base.join("root/sub/c.txt"), b"c").unwrap();
        symlink(base.join("root/sub"), base.join("root/inside")).unwrap();
        symlink(base.join("outside"), base.join("root/escape")).unwrap();
        symlink(
            base.join("outside/secret.txt"),
            base.join("root/escape.txt"),
        )
        .unwrap();
        let root = Root::new(&base.join("root")).unwrap();
        (tmp, root)
    }

    #[test]
    fn lists_sorted_and_filtered() {
        let (_tmp, root) = fixture();
        let names: Vec<_> = root
            .list(Path::new(""))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["inside", "sub", "a.txt", "B.TXT"]);
        let sub = root.list(Path::new("sub")).unwrap();
        assert_eq!(
            sub,
            [Entry {
                name: "c.txt".into(),
                kind: Kind::File { size: 1 }
            }]
        );
    }

    #[test]
    fn symlink_inside_root_is_allowed() {
        let (_tmp, root) = fixture();
        assert_eq!(
            root.read_file(Path::new("inside/c.txt")).unwrap().data,
            b"c"
        );
    }

    #[test]
    fn traversal_is_refused() {
        let (_tmp, root) = fixture();
        for rel in [
            "..",
            "../outside/secret.txt",
            "sub/../../outside",
            "/etc/passwd",
        ] {
            let Err(err) = root.read_file(Path::new(rel)) else {
                panic!("{rel} was readable");
            };
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{rel}");
        }
    }

    #[test]
    fn symlink_escape_is_refused() {
        let (_tmp, root) = fixture();
        for rel in ["escape.txt", "escape/secret.txt"] {
            let Err(err) = root.read_file(Path::new(rel)) else {
                panic!("{rel} was readable");
            };
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{rel}");
        }
        assert!(root.list(Path::new("escape")).is_err());
    }

    #[test]
    fn directory_is_not_a_file() {
        let (_tmp, root) = fixture();
        assert!(root.read_file(Path::new("sub")).is_err());
    }
}
