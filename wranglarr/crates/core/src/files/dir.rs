// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes that a symbolic link can't redirect.
//!
//! The setup wizard often runs as root in folders other users can write to
//! (the stack folder, appdata). A path-based `open` or `chown` there follows
//! whatever link someone planted, and a truncate-then-write loses the old file
//! when it fails halfway. Everything here works on folder descriptors instead:
//!
//! - [`Dir::open`] opens the root the admin chose (links on the way are fine:
//!   that's where they told us to write);
//! - [`Dir::sub`] walks below it one name at a time and never through a link;
//! - [`Dir::write`] writes a temporary file made exclusively in the same
//!   folder with its mode and owner set before it has a name anyone reads,
//!   syncs it and renames it over the old one, so readers see the old file or
//!   the new one and a link in the way is refused, never followed;
//! - [`Dir::write_new`] creates a file only where nothing (not even a dangling
//!   link) has the name yet.

use std::ffi::{OsStr, OsString};
use std::fs::{File, FileTimes};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Gid, Mode, OFlags, Uid};
use rustix::io::Errno;

const DIR: OFlags = OFlags::DIRECTORY.union(OFlags::RDONLY).union(OFlags::CLOEXEC);
const NEW: OFlags =
    OFlags::CREATE.union(OFlags::EXCL).union(OFlags::WRONLY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// Who a new file or folder should belong to; `None` leaves that id alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Owner {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl Owner {
    /// From the wizard's PUID/PGID answers, where -1 (or anything that isn't
    /// an id) means "leave it".
    pub fn ids(uid: i64, gid: i64) -> Owner {
        let id = |v: i64| u32::try_from(v).ok().filter(|v| *v != u32::MAX);
        Owner { uid: id(uid), gid: id(gid) }
    }

    /// Best effort, like the wizard always was: without root this fails, and
    /// the file stays the wizard's.
    fn apply(self, fd: impl AsFd, what: &Path) {
        if self == Owner::default() {
            return;
        }
        if let Err(e) = rustix::fs::fchown(fd, self.uid.map(Uid::from_raw), self.gid.map(Gid::from_raw)) {
            tracing::debug!("chown {}: {e}", what.display());
        }
    }
}

/// How to create a file.
#[derive(Debug, Clone, Copy)]
pub struct NewFile {
    mode: u32,
    owner: Owner,
    times: Option<FileTimes>,
}

impl NewFile {
    /// Owner read/write only, whatever the umask: for anything holding a secret.
    pub fn private() -> NewFile {
        NewFile { mode: 0o600, owner: Owner::default(), times: None }
    }

    /// 0666 less the umask, as `fs::write` would make it.
    pub fn shared() -> NewFile {
        NewFile { mode: 0o666, owner: Owner::default(), times: None }
    }

    /// This mode (less the umask).
    pub fn mode(mut self, mode: u32) -> NewFile {
        self.mode = mode & 0o7777;
        self
    }

    pub fn owner(mut self, owner: Owner) -> NewFile {
        self.owner = owner;
        self
    }

    pub fn times(mut self, times: FileTimes) -> NewFile {
        self.times = Some(times);
        self
    }

    /// Fill a freshly created file: contents, owner, times, then sync.
    fn fill(&self, f: &mut File, data: &[u8], what: &Path) -> io::Result<()> {
        f.write_all(data)?;
        self.owner.apply(&*f, what);
        if let Some(t) = self.times {
            f.set_times(t)?;
        }
        f.sync_all()
    }
}

/// An open folder to write into.
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
    path: PathBuf,
}

/// `e`, saying which path and, for the errors a link causes, why.
fn err(e: Errno, path: &Path) -> io::Error {
    if e == Errno::LOOP || e == Errno::NOTDIR {
        return io::Error::other(format!(
            "{} is a symbolic link (or something else that isn't a folder on the way); wranglarr won't write through it",
            path.display()
        ));
    }
    io::Error::new(io::Error::from(e).kind(), format!("{}: {e}", path.display()))
}

/// One plain name: no `/`, no `..`.
fn plain(name: &str) -> io::Result<&OsStr> {
    match Path::new(name).components().collect::<Vec<_>>()[..] {
        [Component::Normal(n)] if n == OsStr::new(name) => Ok(n),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is not a plain file name"))),
    }
}

/// A temporary file that's removed unless it's been renamed into place.
struct Temp<'a> {
    dir: &'a Dir,
    name: OsString,
    file: File,
    armed: bool,
}

impl Drop for Temp<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = rustix::fs::unlinkat(&self.dir.fd, &self.name, AtFlags::empty());
        }
    }
}

impl Dir {
    /// Open the folder at `path`. Links on the way are followed: this is the
    /// root the admin pointed us at.
    pub fn open(path: &Path) -> io::Result<Dir> {
        let fd = rustix::fs::open(path, DIR, Mode::empty()).map_err(|e| err(e, path))?;
        Ok(Dir { fd, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder `rel` below this one, opened one name at a time and never
    /// through a link. With `create`, missing folders are made with that mode
    /// (less the umask) and owner.
    pub fn sub(&self, rel: impl AsRef<Path>, create: Option<(u32, Owner)>) -> io::Result<Dir> {
        let rel = rel.as_ref();
        let mut cur: Option<Dir> = None;
        for c in rel.components() {
            let name = match c {
                Component::Normal(n) => n,
                Component::CurDir => continue,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} is not a plain relative path", rel.display()),
                    ));
                }
            };
            let parent = cur.as_ref().unwrap_or(self);
            let path = parent.path.join(name);
            let mut made = false;
            if let Some((mode, _)) = create {
                match rustix::fs::mkdirat(&parent.fd, name, Mode::from_bits_truncate(mode)) {
                    Ok(()) => made = true,
                    Err(Errno::EXIST) => {}
                    Err(e) => return Err(err(e, &path)),
                }
            }
            let fd = rustix::fs::openat(&parent.fd, name, DIR | OFlags::NOFOLLOW, Mode::empty())
                .map_err(|e| err(e, &path))?;
            if let (true, Some((_, owner))) = (made, create) {
                owner.apply(&fd, &path);
            }
            cur = Some(Dir { fd, path });
        }
        match cur {
            Some(d) => Ok(d),
            None => Ok(Dir { fd: self.fd.try_clone()?, path: self.path.clone() }),
        }
    }

    /// Hand the folder itself to `owner` (best effort).
    pub fn chown(&self, owner: Owner) {
        owner.apply(&self.fd, &self.path);
    }

    /// Is something (a link counts) called `name` here?
    pub fn exists(&self, name: &str) -> io::Result<bool> {
        match rustix::fs::statat(&self.fd, plain(name)?, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(e) => Err(err(e, &self.path.join(name))),
        }
    }

    /// The regular file `name` (not a link to one) and its metadata.
    pub fn read(&self, name: &str) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
        let path = self.path.join(name);
        let fd = rustix::fs::openat(
            &self.fd,
            plain(name)?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| err(e, &path))?;
        let mut f = File::from(fd);
        let m = f.metadata()?;
        if !m.is_file() {
            return Err(io::Error::other(format!("{} is not a regular file", path.display())));
        }
        let mut data = Vec::with_capacity(usize::try_from(m.len()).unwrap_or(0));
        f.read_to_end(&mut data)?;
        Ok((data, m))
    }

    /// Put `data` at `name`, replacing what's there atomically. A link (or a
    /// folder) at `name` is refused.
    pub fn write(&self, name: &str, data: &[u8], how: NewFile) -> io::Result<()> {
        let target = plain(name)?;
        let path = self.path.join(name);
        match rustix::fs::statat(&self.fd, target, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::RegularFile => {}
            Ok(_) => return Err(err(Errno::LOOP, &path)),
            Err(Errno::NOENT) => {}
            Err(e) => return Err(err(e, &path)),
        }
        let mut tmp = self.temp(name, how.mode)?;
        how.fill(&mut tmp.file, data, &path)?;
        // rename replaces the name itself: if a link was swapped in since the
        // check above, it's the link that goes, not what it points to
        rustix::fs::renameat(&self.fd, &tmp.name, &self.fd, target).map_err(|e| err(e, &path))?;
        tmp.armed = false;
        self.sync();
        Ok(())
    }

    /// Create `name` with `data` only if nothing has that name yet (not even
    /// a dangling link). False when something does.
    pub fn write_new(&self, name: &str, data: &[u8], how: NewFile) -> io::Result<bool> {
        let target = plain(name)?;
        let path = self.path.join(name);
        let fd = match rustix::fs::openat(&self.fd, target, NEW, Mode::from_bits_truncate(how.mode)) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => return Ok(false),
            Err(e) => return Err(err(e, &path)),
        };
        let mut new = Temp { dir: self, name: target.to_os_string(), file: File::from(fd), armed: true };
        how.fill(&mut new.file, data, &path)?;
        new.armed = false;
        self.sync();
        Ok(true)
    }

    /// An exclusively created `.<name>.<random>.tmp` here.
    fn temp(&self, name: &str, mode: u32) -> io::Result<Temp<'_>> {
        for _ in 0..16 {
            let tmp = OsString::from(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
            match rustix::fs::openat(&self.fd, &tmp, NEW, Mode::from_bits_truncate(mode)) {
                Ok(fd) => return Ok(Temp { dir: self, name: tmp, file: File::from(fd), armed: true }),
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(err(e, &self.path.join(&tmp))),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: no free temporary name", self.path.display())))
    }

    /// Make the new name durable (some filesystems can't sync a folder: fine).
    fn sync(&self) {
        if let Err(e) = rustix::fs::fsync(&self.fd) {
            tracing::debug!("fsync {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn mode(p: &Path) -> u32 {
        fs::symlink_metadata(p).expect("stat").permissions().mode() & 0o777
    }

    fn names(p: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(p)
            .expect("ls")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn write_replaces_the_file_with_its_mode() {
        let t = tempfile::tempdir().expect("tmp");
        let p = t.path().join(".env");
        fs::write(&p, "old").expect("w");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).expect("chmod");
        let d = Dir::open(t.path()).expect("open");
        d.write(".env", b"new", NewFile::private()).expect("write");
        assert_eq!(fs::read_to_string(&p).expect("r"), "new");
        assert_eq!(mode(&p), 0o600);
        assert_eq!(names(t.path()), [".env"], "no temporary file left behind");
        d.write("plan.json", b"{}", NewFile::shared().mode(0o640)).expect("write");
        assert_eq!(mode(&t.path().join("plan.json")) & !0o640, 0);
    }

    #[test]
    fn write_never_goes_through_a_link() {
        let t = tempfile::tempdir().expect("tmp");
        let outside = t.path().join("shadow");
        fs::write(&outside, "keep").expect("w");
        let stack = t.path().join("stack");
        fs::create_dir(&stack).expect("mkdir");
        symlink(&outside, stack.join(".env")).expect("ln");
        let d = Dir::open(&stack).expect("open");
        let e = d.write(".env", b"pwned", NewFile::private()).expect_err("a link is refused");
        assert!(e.to_string().contains("symbolic link"), "{e}");
        assert_eq!(fs::read_to_string(&outside).expect("r"), "keep");
        assert!(fs::symlink_metadata(stack.join(".env")).expect("stat").file_type().is_symlink());
        assert_eq!(names(&stack), [".env"]);
        // nor read through one
        assert!(d.read(".env").is_err());
        // a folder isn't a file to replace either
        fs::create_dir(stack.join("conf")).expect("mkdir");
        assert!(d.write("conf", b"x", NewFile::private()).is_err());
        assert_eq!(names(&stack), [".env", "conf"]);
    }

    #[test]
    fn write_new_creates_only_where_nothing_is() {
        let t = tempfile::tempdir().expect("tmp");
        let d = Dir::open(t.path()).expect("open");
        assert!(d.write_new("wranglarr.toml", b"a", NewFile::private()).expect("new"));
        assert!(!d.write_new("wranglarr.toml", b"b", NewFile::private()).expect("exists"));
        assert_eq!(fs::read_to_string(t.path().join("wranglarr.toml")).expect("r"), "a");
        assert_eq!(mode(&t.path().join("wranglarr.toml")), 0o600);
        // a dangling link has the name: nothing is created where it points
        let target = t.path().join("elsewhere");
        symlink(&target, t.path().join("config.xml")).expect("ln");
        assert!(!d.write_new("config.xml", b"secret", NewFile::private()).expect("exists"));
        assert!(!target.exists());
        assert!(d.exists("config.xml").expect("lstat"));
        assert!(!d.exists("nothing").expect("lstat"));
    }

    #[test]
    fn sub_folders_are_walked_without_following_links() {
        let t = tempfile::tempdir().expect("tmp");
        let appdata = t.path().join("appdata");
        let outside = t.path().join("etc");
        fs::create_dir_all(&appdata).expect("mkdir");
        fs::create_dir_all(&outside).expect("mkdir");
        let d = Dir::open(&appdata).expect("open");
        let made = d.sub("bazarr/config", Some((0o700, Owner::default()))).expect("made");
        assert_eq!(made.path(), appdata.join("bazarr/config"));
        assert_eq!(mode(&appdata.join("bazarr")), 0o700);
        assert!(made.write_new("config.yaml", b"k", NewFile::private()).expect("new"));
        // an existing folder is opened as it is
        assert!(d.sub("bazarr", None).expect("open").exists("config").expect("lstat"));
        // a link where a folder should be is refused, made or not
        symlink(&outside, appdata.join("radarr")).expect("ln");
        let e = d.sub("radarr", Some((0o700, Owner::default()))).expect_err("refused");
        assert!(e.to_string().contains("symbolic link"), "{e}");
        assert!(d.sub("radarr/deeper", Some((0o700, Owner::default()))).is_err());
        assert!(names(&outside).is_empty());
        // and so is anything that climbs out
        assert!(d.sub("../etc", None).is_err());
        assert!(d.sub("/etc", None).is_err());
        assert!(d.write("../x", b"x", NewFile::private()).is_err());
        assert!(d.write("a/b", b"x", NewFile::private()).is_err());
    }

    #[test]
    fn owners_from_the_wizards_answers() {
        assert_eq!(Owner::ids(-1, -1), Owner::default());
        assert_eq!(Owner::ids(1000, -1), Owner { uid: Some(1000), gid: None });
        assert_eq!(Owner::ids(1000, 1001), Owner { uid: Some(1000), gid: Some(1001) });
        assert_eq!(Owner::ids(i64::from(u32::MAX), 5_000_000_000), Owner::default());
    }
}
