//! The metadata of files on the system, as the optional metadata of an archive has it
//! ([`Extra`]): read of the files archived ([`Reader`]), and applied to those extracted
//! ([`Applier`]).
//!
//! What is not commonly kept is read and applied only where asked (`-p`, `privileged`): the
//! status change and backup times are read then, and the ownership, the superuser's flags and
//! the set-user-ID and set-group-ID bits applied then.
//!
//! On Windows: the times, the file attributes, the kinds of links and the alternate data
//! streams (as forks); not the extended attributes (EAs) of NTFS, read nor written.

#[cfg(unix)]
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use aqr_formula::aqr::{EntryNode, Extra, MAC_FLAGS_ALLOWED, Time, WIN_ATTRS_ALLOWED};

/// The name of the resource fork of macOS among forks (spec, "forks").
#[cfg(target_os = "macos")]
const RESOURCE_FORK: &[u8] = b"AFP_Resource";
/// The name of the resource fork of macOS among its extended attributes.
#[cfg(target_os = "macos")]
const RESOURCE_FORK_XATTR: &[u8] = b"com.apple.ResourceFork";
/// `UF_*` of `mac-flags`, which the owner can change; the rest (`SF_*`) the superuser alone.
#[cfg(target_os = "macos")]
const USER_FLAGS: u32 = 0xffff;

/// Reads the metadata of the files archived; the names of users and groups found once each.
#[derive(Default)]
pub struct Reader {
    /// (nothing of Windows read but where asked)
    #[cfg_attr(windows, allow(dead_code))]
    pub privileged: bool,
    #[cfg(unix)]
    users: HashMap<u32, Option<Vec<u8>>>,
    #[cfg(unix)]
    groups: HashMap<u32, Option<Vec<u8>>>,
}

/// A fork to archive: its name, and the path its content is read by.
pub struct Fork {
    pub name: Vec<u8>,
    pub path: PathBuf,
    pub size: u64,
}

impl Reader {
    pub fn new(privileged: bool) -> Reader {
        Reader { privileged, ..Reader::default() }
    }

    /// The extra of `path` (of metadata `m`, not followed where it is a link), and its forks
    /// (whose contents the caller gives `Extra::forks`). What cannot be read of it is told by
    /// `warn`.
    pub fn extra(&mut self, path: &Path, m: &fs::Metadata, warn: &mut dyn FnMut(String)) -> (Extra, Vec<Fork>) {
        let time = |t: std::io::Result<std::time::SystemTime>| t.ok().map(time_of);
        let mut x = Extra {
            crtime: time(m.created()),
            mtime: time(m.modified()),
            atime: time(m.accessed()),
            ..Extra::default()
        };
        let mut forks = Vec::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let t = |secs: i64, nanos: i64| Some(Time { secs, nanos: nanos as u32 });
            x.atime = t(m.atime(), m.atime_nsec());
            x.mtime = t(m.mtime(), m.mtime_nsec());
            if self.privileged {
                x.ctime = t(m.ctime(), m.ctime_nsec());
            }
            x.mode = Some(m.mode() & 0o7777);
            x.uid = Some(m.uid() as u64);
            x.gid = Some(m.gid() as u64);
            x.uname = self.users.entry(m.uid()).or_insert_with(|| os::user_name(m.uid())).clone();
            x.gname = self.groups.entry(m.gid()).or_insert_with(|| os::group_name(m.gid())).clone();
            match os::xattrs(path) {
                Ok(a) => x.xattrs = a,
                Err(e) => warn(format!("{}: extended attributes not read: {e}", path.display())),
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt;
            if m.st_flags() & MAC_FLAGS_ALLOWED != 0 {
                x.mac_flags = Some(m.st_flags() & MAC_FLAGS_ALLOWED);
            }
            if self.privileged {
                x.arctime = os::backup_time(path);
            }
            // (the resource fork a fork, not an attribute)
            if let Some(i) = x.xattrs.iter().position(|a| a.0 == RESOURCE_FORK_XATTR) {
                let (_, rsrc) = x.xattrs.remove(i);
                if m.is_file() && !rsrc.is_empty() {
                    let path = fork_path(path);
                    forks.push(Fork { name: RESOURCE_FORK.to_vec(), path, size: rsrc.len() as u64 });
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{FileTypeExt, MetadataExt};
            let a = m.file_attributes() & WIN_ATTRS_ALLOWED;
            if a != 0 {
                x.win_attrs = Some(a);
            }
            // (a junction taken for a link to a directory)
            if m.is_symlink() {
                x.win_link_type = Some(m.file_type().is_symlink_dir() as u8);
            } else {
                match os::streams(path) {
                    Ok(f) => forks = f,
                    Err(e) => warn(format!("{}: alternate data streams not read: {e}", path.display())),
                }
            }
        }
        let _ = (path, &mut forks, MAC_FLAGS_ALLOWED, WIN_ATTRS_ALLOWED);
        (x, forks)
    }
}

/// The bytes of `s` as an entry name or a link target has them: as they are on Unix, WTF-8 on
/// Windows (`\` as `/`).
#[cfg(unix)]
pub fn os_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(windows)]
pub fn os_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = Vec::new();
    for c in char::decode_utf16(s.encode_wide()) {
        // (a lone surrogate as UTF-8 would have it, were it a scalar)
        let c = c.map_or_else(|e| e.unpaired_surrogate() as u32, |c| if c == '\\' { '/' as u32 } else { c as u32 });
        match c {
            0..0x80 => out.push(c as u8),
            0x80..0x800 => out.extend_from_slice(&[0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]),
            0x800..0x1_0000 => {
                out.extend_from_slice(&[0xe0 | (c >> 12) as u8, 0x80 | (c >> 6 & 0x3f) as u8, 0x80 | (c & 0x3f) as u8])
            }
            _ => out.extend_from_slice(&[
                0xf0 | (c >> 18) as u8,
                0x80 | (c >> 12 & 0x3f) as u8,
                0x80 | (c >> 6 & 0x3f) as u8,
                0x80 | (c & 0x3f) as u8,
            ]),
        }
    }
    out
}

/// The name or path of bytes `b` ([`os_bytes`]; on Windows, WTF-8, else replaced).
#[cfg(unix)]
pub fn os_from(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(b).to_owned()
}

#[cfg(windows)]
pub fn os_from(b: &[u8]) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    let mut wide = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let n = match b[i] {
            0..0x80 => 1,
            0xc0..0xe0 => 2,
            0xe0..0xf0 => 3,
            _ => 4,
        };
        let Some(seq) = b.get(i..i + n).filter(|s| n == 1 || s[1..].iter().all(|&c| c & 0xc0 == 0x80)) else {
            wide.push(0xfffd);
            i += 1;
            continue;
        };
        let mut c = if n == 1 { seq[0] as u32 } else { (seq[0] as u32) & (0x7f >> n) };
        seq[1..].iter().for_each(|&x| c = (c << 6) | (x & 0x3f) as u32);
        match c {
            0x1_0000.. => {
                wide.extend_from_slice(&[0xd800 | ((c - 0x1_0000) >> 10) as u16, 0xdc00 | (c & 0x3ff) as u16])
            }
            _ => wide.push(c as u16),
        }
        i += n;
    }
    OsString::from_wide(&wide)
}

/// The target of a link as it is archived: on Windows, a Win32 path rather than one of the
/// object manager's (`\\?\C:\x` as `C:\x`, `\\?\UNC\s\x` as `\\s\x`; spec, "Archive model").
pub fn link_target(target: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = target.as_os_str().to_string_lossy();
        for (prefix, to) in [(r"\\?\UNC\", r"\\"), (r"\??\UNC\", r"\\"), (r"\\?\", ""), (r"\??\", "")] {
            if let Some(rest) = s.strip_prefix(prefix)
                && !rest.starts_with("Volume{")
            {
                return format!("{to}{rest}").into();
            }
        }
    }
    target
}

/// The inode of file `path` (of metadata `m`): its device and number, where it has more links
/// than one.
pub fn inode(path: &Path, m: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        (m.nlink() > 1).then(|| (m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        let _ = m;
        os::inode(path)
    }
}

/// The path the resource fork of file `path` is read and written by.
#[cfg(target_os = "macos")]
fn fork_path(path: &Path) -> PathBuf {
    let mut p = std::ffi::OsString::from(path);
    p.push("/..namedfork/rsrc");
    p.into()
}

/// The node of `path` of metadata `m`, where it is a FIFO or a device.
pub fn special(m: &fs::Metadata) -> Option<EntryNode> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let t = m.file_type();
        if t.is_fifo() {
            return Some(EntryNode::Fifo);
        }
        if t.is_char_device() || t.is_block_device() {
            let (major, minor) = os::device(m.rdev());
            return Some(EntryNode::Device { block: t.is_block_device(), major, minor });
        }
    }
    let _ = m;
    None
}

fn time_of(t: std::time::SystemTime) -> Time {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Time { secs: d.as_secs() as i64, nanos: d.subsec_nanos() },
        Err(e) => {
            let d = e.duration();
            let (s, n) = (d.as_secs() as i64, d.subsec_nanos());
            if n == 0 { Time { secs: -s, nanos: 0 } } else { Time { secs: -s - 1, nanos: 1_000_000_000 - n } }
        }
    }
}

/// What an entry being extracted is, as far as its metadata is applied.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link, its metadata applied to itself.
    Link,
    /// A FIFO or a device.
    Special,
}

/// Applies the metadata of the entries extracted; what cannot be applied but needs not fail the
/// extraction is told (but with `quiet`).
pub struct Applier {
    pub privileged: bool,
    pub quiet: bool,
}

impl Applier {
    pub fn tell(&self, path: &Path, what: &str) {
        if !self.quiet {
            eprintln!("{}: {what}", path.display());
        }
    }

    /// Writes fork `name` of `data` to `path`, a `kind` (told where it cannot be written).
    pub fn fork(&self, path: &Path, kind: Kind, name: &[u8], data: &[u8]) {
        let shown = String::from_utf8_lossy(name);
        if let Err(e) = os::fork(path, kind, name, data) {
            self.tell(path, &format!("fork {shown} not written: {e}"));
        }
    }

    /// Creates FIFO or device `node` at `path` (where asked).
    pub fn special(&self, path: &Path, node: &EntryNode) -> Result<bool, String> {
        if !self.privileged {
            self.tell(path, "a FIFO or a device, not created (-p to create it)");
            return Ok(false);
        }
        os::make_special(path, node).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(true)
    }

    /// Applies `x` to the `kind` at `path` (its forks written already): its extended
    /// attributes, its owner (where asked), its permissions, its times and its flags, in this
    /// order (spec, "Extraction"). Fails where its permissions or times cannot be set, or its
    /// owner where asked.
    pub fn apply(&self, path: &Path, kind: Kind, x: &Extra) -> Result<(), String> {
        let err = |e: std::io::Error| format!("{}: {e}", path.display());
        // (the attributes the forks are written as)
        #[cfg(unix)]
        {
            let forks: Vec<&[u8]> = x.forks.iter().filter_map(|f| os::fork_xattr(kind, &f.0)).collect();
            for (name, value) in &x.xattrs {
                let shown = String::from_utf8_lossy(name);
                if forks.contains(&&name[..]) {
                    self.tell(path, &format!("extended attribute {shown} not written: a fork of the same name"));
                } else if let Err(e) = os::set_xattr(path, name, value) {
                    self.tell(path, &format!("extended attribute {shown} not written: {e}"));
                }
            }
        }
        #[cfg(unix)]
        if self.privileged && (x.uid.is_some() || x.uname.is_some() || x.gid.is_some() || x.gname.is_some()) {
            let uid = x.uname.as_deref().and_then(os::user_id).or(x.uid.and_then(|u| u32::try_from(u).ok()));
            let gid = x.gname.as_deref().and_then(os::group_id).or(x.gid.and_then(|g| u32::try_from(g).ok()));
            std::os::unix::fs::lchown(path, uid, gid).map_err(err)?;
        }
        #[cfg(unix)]
        if let Some(mode) = x.mode {
            let mode = if self.privileged { mode } else { mode & 0o1777 };
            os::set_mode(path, kind, mode).map_err(err)?;
        }
        os::set_times(path, kind, x).map_err(err)?;
        // (the attributes after the times, as read-only forbids them; from the mode where there
        // are none, read-only where the owner cannot write)
        #[cfg(windows)]
        if let Some(a) = x.win_attrs.or(x.mode.map(|m| (m & 0o200 == 0) as u32)) {
            os::set_attrs(path, a).map_err(err)?;
        }
        #[cfg(target_os = "macos")]
        if let Some(t) = x.crtime.filter(|_| kind != Kind::Link)
            && let Err(e) = os::set_attr_time(path, libc::ATTR_CMN_CRTIME, t)
        {
            self.tell(path, &format!("creation time not set: {e}"));
        }
        #[cfg(target_os = "macos")]
        if let Some(t) = x.arctime.filter(|_| self.privileged && kind != Kind::Link)
            && let Err(e) = os::set_attr_time(path, libc::ATTR_CMN_BKUPTIME, t)
        {
            self.tell(path, &format!("backup time not set: {e}"));
        }
        #[cfg(target_os = "macos")]
        if let Some(f) = x.mac_flags {
            let f = if self.privileged { f } else { f & USER_FLAGS };
            if f != 0
                && let Err(e) = os::set_flags(path, f)
            {
                self.tell(path, &format!("flags not set: {e}"));
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
mod os {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use aqr_formula::aqr::{EntryNode, Extra, Time};

    use super::Kind;

    fn c_path(p: &Path) -> io::Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::other("a path of a zero byte"))
    }

    fn c_name(n: &[u8]) -> io::Result<CString> {
        CString::new(n).map_err(|_| io::Error::other("a name of a zero byte"))
    }

    fn check(r: libc::c_int) -> io::Result<()> {
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// A buffer of `n` bytes filled by `f` (its size, or below zero for an error), grown where
    /// the bytes became more since they were counted (`ERANGE`).
    fn sized(mut f: impl FnMut(*mut u8, usize) -> isize) -> io::Result<Vec<u8>> {
        loop {
            let n = f(std::ptr::null_mut(), 0);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut b = vec![0u8; n as usize];
            let got = f(b.as_mut_ptr(), b.len());
            if got >= 0 {
                b.truncate(got as usize);
                return Ok(b);
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ERANGE) {
                return Err(e);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn list(p: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: `b` is of `n` bytes, or null with `n` zero
        unsafe { libc::listxattr(p.as_ptr(), b.cast(), n, libc::XATTR_NOFOLLOW) }
    }

    #[cfg(target_os = "macos")]
    fn get(p: &CStr, name: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as `list`
        unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), b.cast(), n, 0, libc::XATTR_NOFOLLOW) }
    }

    #[cfg(target_os = "linux")]
    fn list(p: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as above
        unsafe { libc::llistxattr(p.as_ptr(), b.cast(), n) }
    }

    #[cfg(target_os = "linux")]
    fn get(p: &CStr, name: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as above
        unsafe { libc::lgetxattr(p.as_ptr(), name.as_ptr(), b.cast(), n) }
    }

    /// The extended attributes of `path` (not followed), by name.
    pub fn xattrs(path: &Path) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let p = c_path(path)?;
        let names = match sized(|b, n| list(&p, b, n)) {
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(Vec::new()),
            r => r?,
        };
        let mut out = Vec::new();
        for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
            let c = c_name(name)?;
            match sized(|b, n| get(&p, &c, b, n)) {
                Ok(v) => out.push((name.to_vec(), v)),
                // (gone since it was listed)
                Err(e) if e.raw_os_error() == Some(NO_ATTR) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// The error of an attribute not there.
    #[cfg(target_os = "macos")]
    const NO_ATTR: libc::c_int = libc::ENOATTR;
    #[cfg(target_os = "linux")]
    const NO_ATTR: libc::c_int = libc::ENODATA;

    pub fn set_xattr(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
        let (p, n) = (c_path(path)?, c_name(name)?);
        let (v, len) = (value.as_ptr().cast(), value.len());
        // SAFETY: `value` is of `len` bytes
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), v, len, 0, libc::XATTR_NOFOLLOW) };
        // SAFETY: as above
        #[cfg(target_os = "linux")]
        let r = unsafe { libc::lsetxattr(p.as_ptr(), n.as_ptr(), v, len, 0) };
        check(r)
    }

    /// The extended attribute that fork `name` of a `kind` is written as (the resource fork
    /// too), where it can be written (`None`: a name reserved here).
    pub fn fork_xattr(kind: Kind, name: &[u8]) -> Option<&[u8]> {
        #[cfg(target_os = "macos")]
        match name {
            super::RESOURCE_FORK if kind == Kind::File => return Some(super::RESOURCE_FORK_XATTR),
            super::RESOURCE_FORK | super::RESOURCE_FORK_XATTR => return None,
            _ => {}
        }
        let _ = kind;
        Some(name)
    }

    pub fn fork(path: &Path, kind: Kind, name: &[u8], data: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if name == super::RESOURCE_FORK && kind == Kind::File {
            return std::fs::write(super::fork_path(path), data);
        }
        match fork_xattr(kind, name) {
            Some(n) => set_xattr(path, n, data),
            None => Err(io::Error::other("a name reserved here")),
        }
    }

    pub fn set_mode(path: &Path, kind: Kind, mode: u32) -> io::Result<()> {
        let p = c_path(path)?;
        let flags = if kind == Kind::Link { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
        // (Linux has no modes of links)
        if cfg!(target_os = "linux") && kind == Kind::Link {
            return Ok(());
        }
        // SAFETY: a path of a C string
        check(unsafe { libc::fchmodat(libc::AT_FDCWD, p.as_ptr(), mode as libc::mode_t, flags) })
    }

    /// Sets the access and modification times of `path` (not followed) where `x` has them.
    pub fn set_times(path: &Path, _: Kind, x: &Extra) -> io::Result<()> {
        if x.atime.is_none() && x.mtime.is_none() {
            return Ok(());
        }
        let spec = |t: Option<Time>| match t {
            Some(t) => libc::timespec { tv_sec: t.secs as libc::time_t, tv_nsec: t.nanos as _ },
            None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT as _ },
        };
        let times = [spec(x.atime), spec(x.mtime)];
        let p = c_path(path)?;
        // SAFETY: two times, as it takes
        check(unsafe { libc::utimensat(libc::AT_FDCWD, p.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) })
    }

    pub fn make_special(path: &Path, node: &EntryNode) -> io::Result<()> {
        let p = c_path(path)?;
        let r = match *node {
            // SAFETY: a path of a C string
            EntryNode::Fifo => unsafe { libc::mkfifo(p.as_ptr(), 0o600) },
            EntryNode::Device { block, major, minor } => {
                let (Ok(major), Ok(minor)) = (major.try_into(), minor.try_into()) else {
                    return Err(io::Error::other("a device number beyond this system's"));
                };
                let kind = if block { libc::S_IFBLK } else { libc::S_IFCHR };
                let dev = libc::makedev(major, minor);
                // SAFETY: as above
                unsafe { libc::mknod(p.as_ptr(), kind | 0o600, dev) }
            }
            _ => unreachable!("not a FIFO nor a device"),
        };
        check(r)
    }

    /// The major and minor numbers of device `dev`.
    pub fn device(dev: u64) -> (u64, u64) {
        let dev = dev as libc::dev_t;
        (libc::major(dev) as u64, libc::minor(dev) as u64)
    }

    fn name_of(id: u32, group: bool) -> Option<Vec<u8>> {
        let mut buf = vec![0 as libc::c_char; 16 << 10];
        let mut out = std::ptr::null_mut();
        // SAFETY: the entries and the buffer are ours, and the name read while they live
        unsafe {
            if group {
                let mut g: libc::group = std::mem::zeroed();
                libc::getgrgid_r(id, &mut g, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then(|| CStr::from_ptr(g.gr_name).to_bytes().to_vec())
            } else {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out_pw = std::ptr::null_mut();
                libc::getpwuid_r(id, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out_pw);
                (!out_pw.is_null()).then(|| CStr::from_ptr(pw.pw_name).to_bytes().to_vec())
            }
        }
    }

    pub fn user_name(uid: u32) -> Option<Vec<u8>> {
        name_of(uid, false)
    }

    pub fn group_name(gid: u32) -> Option<Vec<u8>> {
        name_of(gid, true)
    }

    fn id_of(name: &[u8], group: bool) -> Option<u32> {
        let name = CString::new(name).ok()?;
        let mut buf = vec![0 as libc::c_char; 16 << 10];
        // SAFETY: as `name_of`
        unsafe {
            if group {
                let mut g: libc::group = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                libc::getgrnam_r(name.as_ptr(), &mut g, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then_some(g.gr_gid)
            } else {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                libc::getpwnam_r(name.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then_some(pw.pw_uid)
            }
        }
    }

    pub fn user_id(name: &[u8]) -> Option<u32> {
        id_of(name, false)
    }

    pub fn group_id(name: &[u8]) -> Option<u32> {
        id_of(name, true)
    }

    #[cfg(target_os = "macos")]
    fn attrs(common: libc::attrgroup_t) -> libc::attrlist {
        // SAFETY: plain data, zero where not set
        let mut a: libc::attrlist = unsafe { std::mem::zeroed() };
        a.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        a.commonattr = common;
        a
    }

    /// The backup time of `path` (not followed), where it has one (not zero).
    #[cfg(target_os = "macos")]
    pub fn backup_time(path: &Path) -> Option<Time> {
        let p = c_path(path).ok()?;
        let mut a = attrs(libc::ATTR_CMN_BKUPTIME);
        // (the length of the attributes, then the time)
        #[repr(C)]
        struct Buf {
            len: u32,
            t: libc::timespec,
        }
        // SAFETY: as `attrs`
        let mut b: Buf = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<Buf>();
        // SAFETY: the buffer is of `size` bytes
        let r = unsafe {
            libc::getattrlist(p.as_ptr(), (&raw mut a).cast(), (&raw mut b).cast(), size, libc::FSOPT_NOFOLLOW)
        };
        (r == 0 && (b.t.tv_sec, b.t.tv_nsec) != (0, 0)).then_some(Time { secs: b.t.tv_sec, nanos: b.t.tv_nsec as u32 })
    }

    /// Sets common attribute `attr` (a time) of `path` (not followed) to `t`.
    #[cfg(target_os = "macos")]
    pub fn set_attr_time(path: &Path, attr: libc::attrgroup_t, t: Time) -> io::Result<()> {
        let p = c_path(path)?;
        let mut a = attrs(attr);
        let mut b = libc::timespec { tv_sec: t.secs, tv_nsec: t.nanos as _ };
        let size = std::mem::size_of::<libc::timespec>();
        // SAFETY: the buffer is of `size` bytes
        check(unsafe {
            libc::setattrlist(p.as_ptr(), (&raw mut a).cast(), (&raw mut b).cast(), size, libc::FSOPT_NOFOLLOW)
        })
    }

    #[cfg(target_os = "macos")]
    pub fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
        let p = c_path(path)?;
        // SAFETY: a path of a C string
        check(unsafe { libc::lchflags(p.as_ptr(), flags) })
    }
}

#[cfg(windows)]
mod os {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};

    use aqr_formula::aqr::{EntryNode, Extra, Time};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES, FindClose, FindFirstStreamW, FindNextStreamW,
        FindStreamInfoStandard, GetFileInformationByHandle, SetFileAttributesW, WIN32_FIND_STREAM_DATA,
    };

    use super::{Fork, Kind};

    /// `ERROR_HANDLE_EOF`: no more streams.
    const NO_MORE: i32 = 38;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain([0]).collect()
    }

    /// The path of stream `name` of `path`.
    fn stream(path: &Path, name: &std::ffi::OsStr) -> PathBuf {
        let mut p = path.as_os_str().to_owned();
        p.push(":");
        p.push(name);
        p.into()
    }

    /// The alternate data streams of `path`, as forks (the unnamed one, the content, left out).
    pub fn streams(path: &Path) -> io::Result<Vec<Fork>> {
        let w = wide(path);
        let mut d = WIN32_FIND_STREAM_DATA::default();
        // SAFETY: a path of a NUL, and data of the level asked
        let h = unsafe { FindFirstStreamW(w.as_ptr(), FindStreamInfoStandard, (&raw mut d).cast(), 0) };
        if h == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            // (none, or a file system of no streams)
            return if matches!(e.raw_os_error(), Some(NO_MORE) | Some(1)) { Ok(Vec::new()) } else { Err(e) };
        }
        let mut out = Vec::new();
        let r = loop {
            let n = d.cStreamName.iter().position(|&c| c == 0).unwrap_or(d.cStreamName.len());
            let full = OsString::from_wide(&d.cStreamName[..n]);
            // `:NAME:$DATA`
            let s = full.to_string_lossy();
            if let Some(name) = d.cStreamName[..n].strip_prefix(&[b':' as u16])
                && s.ends_with(":$DATA")
                && name.len() > 6
            {
                let name = OsString::from_wide(&name[..name.len() - 6]);
                let size = d.StreamSize as u64;
                out.push(Fork { name: super::os_bytes(&name), path: stream(path, &name), size });
            }
            // SAFETY: as above
            if unsafe { FindNextStreamW(h, (&raw mut d).cast()) } == 0 {
                let e = io::Error::last_os_error();
                break if e.raw_os_error() == Some(NO_MORE) { Ok(out) } else { Err(e) };
            }
        };
        // SAFETY: the handle found
        unsafe { FindClose(h) };
        r
    }

    /// The volume and the index of file `path`, where it has more links than one.
    pub fn inode(path: &Path) -> Option<(u64, u64)> {
        let f = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()?;
        let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: an open handle, and the information of its type
        if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut i) } == 0 || i.nNumberOfLinks < 2 {
            return None;
        }
        Some((i.dwVolumeSerialNumber as u64, (i.nFileIndexHigh as u64) << 32 | i.nFileIndexLow as u64))
    }

    pub fn fork(path: &Path, kind: Kind, name: &[u8], data: &[u8]) -> io::Result<()> {
        if kind == Kind::Link {
            return Err(io::Error::other("no streams written to links here"));
        }
        fs::write(stream(path, &super::os_from(name)), data)
    }

    fn system(t: Time) -> std::time::SystemTime {
        match t.secs {
            s if s >= 0 => std::time::UNIX_EPOCH + std::time::Duration::new(s as u64, t.nanos),
            s => {
                std::time::UNIX_EPOCH - std::time::Duration::from_secs(s.unsigned_abs())
                    + std::time::Duration::from_nanos(t.nanos as u64)
            }
        }
    }

    /// Sets the creation, access and modification times of `path` (not followed) where `x` has
    /// them.
    pub fn set_times(path: &Path, kind: Kind, x: &Extra) -> io::Result<()> {
        if x.crtime.is_none() && x.atime.is_none() && x.mtime.is_none() {
            return Ok(());
        }
        let link = if kind == Kind::Link { FILE_FLAG_OPEN_REPARSE_POINT } else { 0 };
        let f = fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | link)
            .open(path)?;
        let mut t = fs::FileTimes::new();
        if let Some(c) = x.crtime {
            t = t.set_created(system(c));
        }
        if let Some(a) = x.atime {
            t = t.set_accessed(system(a));
        }
        if let Some(m) = x.mtime {
            t = t.set_modified(system(m));
        }
        f.set_times(t)
    }

    pub fn set_attrs(path: &Path, attrs: u32) -> io::Result<()> {
        let attrs = if attrs == 0 { FILE_ATTRIBUTE_NORMAL } else { attrs };
        // SAFETY: a path of a NUL
        if unsafe { SetFileAttributesW(wide(path).as_ptr(), attrs) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn make_special(_: &Path, _: &EntryNode) -> io::Result<()> {
        Err(io::Error::other("not made on this system"))
    }
}
