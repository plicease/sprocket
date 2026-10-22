// SPDX-License-Identifier: MIT
//! Read-only access to a file tree confined to a configured root.
//!
//! Every path is resolved through the filesystem (following symlinks) and
//! must stay inside the canonical root, so neither `..` components nor
//! symlinks can reach files outside it. Symlinks that point within the root
//! are allowed. Hidden names (starting with `.`) are never reachable,
//! whether named directly or reached through a symlink.
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
                Component::Normal(name) if is_hidden(name) => return Err(hidden(rel)),
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(outside(rel)),
            }
        }
        let full = self.path.join(rel).canonicalize()?;
        let Ok(inner) = full.strip_prefix(&self.path) else {
            return Err(outside(rel));
        };
        if inner.iter().any(is_hidden) {
            return Err(hidden(rel));
        }
        Ok(full)
    }

    /// List the directory `rel`, sorted alphabetically (ignoring case). Hidden entries, non-UTF-8 names, special files,
    /// and anything resolving outside the root are omitted.
    pub fn list(&self, rel: &Path) -> io::Result<Vec<Entry>> {
        let dir = self.resolve(rel)?;
        let mut entries = Vec::new();
        for item in fs::read_dir(dir)? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
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
        entries.sort_by(|a, b| {
            (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name))
        });
        Ok(entries)
    }

    /// Find `rel` ignoring case, as DOS users expect, and return it with the
    /// names as they are on disk. Each component matches its exact name if
    /// there is one, otherwise the one listed entry that differs only in
    /// case; several such entries are ambiguous and refused. Matching is
    /// against [`Root::list`], so hidden and escaping names are never found.
    pub fn find(&self, rel: &Path) -> io::Result<PathBuf> {
        if self.resolve(rel).is_ok() {
            return Ok(rel.to_path_buf());
        }
        let mut found = PathBuf::new();
        for component in rel.components() {
            let name = match component {
                Component::CurDir => continue,
                Component::Normal(name) if is_hidden(name) => return Err(hidden(rel)),
                Component::Normal(name) => name.to_string_lossy(),
                _ => return Err(outside(rel)),
            };
            let entries = self.list(&found)?;
            let lower = name.to_lowercase();
            let mut similar = entries.iter().filter(|e| e.name.to_lowercase() == lower);
            let entry = match entries.iter().find(|e| e.name == name) {
                Some(exact) => exact,
                None => match (similar.next(), similar.next()) {
                    (Some(only), None) => only,
                    (None, _) => return Err(hidden(rel)),
                    (Some(_), Some(_)) => {
                        return Err(io::Error::other(
                            "several names differ only in case; type the exact name",
                        ));
                    }
                },
            };
            found.push(&entry.name);
        }
        Ok(found)
    }

    /// Whether `rel` is a directory or a regular file (and its size).
    pub fn stat(&self, rel: &Path) -> io::Result<Kind> {
        let meta = fs::metadata(self.resolve(rel)?)?;
        if meta.is_dir() {
            Ok(Kind::Dir)
        } else if meta.is_file() {
            Ok(Kind::File { size: meta.len() })
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", rel.display()),
            ))
        }
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

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
}

/// A name that is absent, or hidden and so treated as absent.
fn hidden(rel: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} not found", rel.display()),
    )
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

    /// root/{a.txt, B.TXT, sub/c.txt, .hidden, .dot/d.txt, inside -> sub,
    /// peek -> .hidden, peekdir -> .dot, escape -> outside/,
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
        fs::create_dir(base.join("root/.dot")).unwrap();
        fs::write(base.join("root/.dot/d.txt"), b"d").unwrap();
        symlink(base.join("root/.hidden"), base.join("root/peek")).unwrap();
        symlink(base.join("root/.dot"), base.join("root/peekdir")).unwrap();
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
        assert_eq!(names, ["a.txt", "B.TXT", "inside", "sub"]);
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
    fn hidden_is_refused() {
        let (_tmp, root) = fixture();
        for rel in [
            ".hidden",
            ".dot",
            ".dot/d.txt",
            "peek",
            "peekdir/d.txt",
            "sub/../.hidden",
        ] {
            assert!(
                root.read_file(Path::new(rel)).is_err(),
                "{rel} was readable"
            );
        }
        assert!(root.list(Path::new(".dot")).is_err());
        assert!(root.list(Path::new("peekdir")).is_err());
    }

    #[test]
    fn find_ignores_case() {
        let (_tmp, root) = fixture();
        let found = |rel: &str| root.find(Path::new(rel)).map_err(|e| e.kind());
        assert_eq!(found("A.TXT"), Ok(PathBuf::from("a.txt")));
        assert_eq!(found("b.txt"), Ok(PathBuf::from("B.TXT")));
        assert_eq!(found("SUB/C.Txt"), Ok(PathBuf::from("sub/c.txt")));
        assert_eq!(found("./Inside/./C.TXT"), Ok(PathBuf::from("inside/c.txt")));
        assert_eq!(found("nope"), Err(io::ErrorKind::NotFound));
        assert_eq!(found("a.txt/x"), Err(io::ErrorKind::NotADirectory));
        assert_eq!(found("../OUTSIDE"), Err(io::ErrorKind::PermissionDenied));
        for rel in [
            ".HIDDEN",
            ".Dot/D.TXT",
            "PEEK",
            "PeekDir/d.txt",
            "ESCAPE.TXT",
            "Escape/secret.txt",
        ] {
            assert!(found(rel).is_err(), "{rel} was found");
        }
    }

    #[test]
    fn find_prefers_exact_case() {
        let (tmp, root) = fixture();
        fs::write(tmp.path().join("root/A.txt"), b"A").unwrap();
        fs::create_dir(tmp.path().join("root/Sub")).unwrap();
        let found = |rel: &str| root.find(Path::new(rel)).map_err(|e| e.kind());
        assert_eq!(found("a.txt"), Ok(PathBuf::from("a.txt")));
        assert_eq!(found("A.txt"), Ok(PathBuf::from("A.txt")));
        assert_eq!(found("Sub"), Ok(PathBuf::from("Sub")));
        assert_eq!(found("sub/C.TXT"), Ok(PathBuf::from("sub/c.txt")));
        assert_eq!(found("A.TXT"), Err(io::ErrorKind::Other));
        assert_eq!(found("SUB/c.txt"), Err(io::ErrorKind::Other));
    }

    #[test]
    fn directory_is_not_a_file() {
        let (_tmp, root) = fixture();
        assert!(root.read_file(Path::new("sub")).is_err());
    }
}
