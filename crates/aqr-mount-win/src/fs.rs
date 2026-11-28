//! The volume: an aqr-mount [`Tree`] served through WinFsp.
//!
//! WinFsp's driver asks the dispatcher threads (in this process) for what the cache and its
//! own checks cannot answer. With a read-only volume and information that never times out
//! (`file_info_timeout(u32::MAX)`), what comes here is the opening of names, the first read of
//! each page (the cache manager's paging reads) and directory listings. Paths come whole
//! (`\a\b`) and are walked component by component, in any case.

use std::ffi::{OsStr, c_void};

use aqr_mount::{Attr, Case, Kind, NodeId, Tree};
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, U16CStr};

const STATUS_END_OF_FILE: i32 = 0xC000_0011_u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2_u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BA_u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103_u32 as i32;

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;

/// 1970-01-01 as a FILETIME (100 ns since 1601).
const UNIX_EPOCH: i64 = 11_644_473_600 * 10_000_000;

fn status(e: aqr_mount::Error) -> FspError {
    match e {
        aqr_mount::Error::NotFound => FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND),
        aqr_mount::Error::NotADirectory => FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY),
        aqr_mount::Error::IsADirectory => FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY),
        aqr_mount::Error::Io(e) => e.into(),
    }
}

fn attributes(attr: &Attr) -> u32 {
    match attr.kind {
        Kind::Dir => FILE_ATTRIBUTE_DIRECTORY,
        Kind::File => FILE_ATTRIBUTE_READONLY,
    }
}

fn fill(node: NodeId, attr: &Attr, info: &mut FileInfo) {
    let time = (UNIX_EPOCH + attr.mtime_ns.div_euclid(100)).max(0) as u64;
    info.file_attributes = attributes(attr);
    info.reparse_tag = 0;
    info.file_size = attr.size;
    info.allocation_size = attr.size.next_multiple_of(4096);
    info.creation_time = time;
    info.last_access_time = time;
    info.last_write_time = time;
    info.change_time = time;
    info.index_number = node;
    info.hard_links = 0;
    info.ea_size = 0;
}

/// An open handle: the node, and for a directory the listing WinFsp keeps between the calls
/// of one enumeration.
pub struct Handle {
    node: NodeId,
    kind: Kind,
    listing: DirBuffer,
}

struct Volume {
    tree: Box<dyn Tree>,
}

impl Volume {
    /// The node of a path; a name that is not UTF-16 is in no archive.
    fn lookup(&self, path: &U16CStr) -> winfsp::Result<(NodeId, Attr)> {
        let path = path.to_string().map_err(|_| FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;
        let node = aqr_mount::lookup_path(&*self.tree, &path, '\\', Case::Insensitive).map_err(status)?;
        Ok((node, self.tree.attr(node).map_err(status)?))
    }
}

impl FileSystemContext for Volume {
    type FileContext = Handle;

    /// No security descriptor: WinFsp then grants what the volume's flags allow (reading).
    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let (_, attr) = self.lookup(file_name)?;
        Ok(FileSecurity { reparse: false, sz_security_descriptor: 0, attributes: attributes(&attr) })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Handle> {
        let (node, attr) = self.lookup(file_name)?;
        match attr.kind {
            Kind::File if create_options & FILE_DIRECTORY_FILE != 0 => {
                return Err(FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY));
            }
            Kind::Dir if create_options & FILE_NON_DIRECTORY_FILE != 0 => {
                return Err(FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
            }
            _ => {}
        }
        fill(node, &attr, file_info.as_mut());
        Ok(Handle { node, kind: attr.kind, listing: DirBuffer::new() })
    }

    /// What would create reaches here despite the read-only volume (the driver turns away
    /// writes to what exists): said as a write-protected medium, not an unknown request.
    fn create(
        &self,
        _file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        _file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Handle> {
        Err(FspError::NTSTATUS(STATUS_MEDIA_WRITE_PROTECTED))
    }

    /// The same of renaming (opened for DELETE, which reading does not turn away).
    fn rename(
        &self,
        _handle: &Handle,
        _file_name: &U16CStr,
        _new_file_name: &U16CStr,
        _replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        Err(FspError::NTSTATUS(STATUS_MEDIA_WRITE_PROTECTED))
    }

    fn close(&self, _handle: Handle) {}

    fn get_file_info(&self, handle: &Handle, file_info: &mut FileInfo) -> winfsp::Result<()> {
        let attr = self.tree.attr(handle.node).map_err(status)?;
        fill(handle.node, &attr, file_info);
        Ok(())
    }

    fn read(&self, handle: &Handle, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        match self.tree.read(handle.node, offset, buffer).map_err(status)? {
            0 if !buffer.is_empty() => Err(FspError::NTSTATUS(STATUS_END_OF_FILE)),
            n => Ok(n as u32),
        }
    }

    /// The directory's entries, sorted by WinFsp as the buffer is released. The pattern
    /// (`*.txt`) is the driver's to apply (the volume does not ask for it). `.` and `..` are
    /// listed but in the root, as NTFS does.
    fn read_directory(
        &self,
        handle: &Handle,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if handle.kind != Kind::Dir {
            return Err(FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY));
        }
        // (filled at the first call of an enumeration, then read on from the marker)
        if let Ok(lock) = handle.listing.acquire(marker.is_none(), None) {
            let mut entry: DirInfo = DirInfo::new();
            let mut add = |name: &str, node: NodeId| -> winfsp::Result<()> {
                let attr = self.tree.attr(node).map_err(status)?;
                entry.reset();
                fill(node, &attr, entry.file_info_mut());
                entry.set_name(OsStr::new(name))?;
                lock.write(&mut entry)
            };
            if handle.node != aqr_mount::ROOT {
                add(".", handle.node)?;
                // (the parent is not known to the tree: the directory's own information)
                add("..", handle.node)?;
            }
            for e in self.tree.entries(handle.node).map_err(status)? {
                add(&e.name, e.node)?;
            }
        }
        Ok(handle.listing.read(marker, buffer))
    }

    fn get_volume_info(&self, info: &mut VolumeInfo) -> winfsp::Result<()> {
        info.total_size = 0;
        info.free_size = 0;
        info.set_volume_label("Aqueous");
        Ok(())
    }
}

/// Mounts the volume and serves it until the process ends.
pub fn serve(tree: Box<dyn Tree>, mount_point: Option<&OsStr>) -> winfsp::Result<()> {
    let mtime_ns = tree.attr(aqr_mount::ROOT).map(|a| a.mtime_ns).unwrap_or(0);
    let mut params = VolumeParams::new();
    params
        .filesystem_name("aqr")
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .volume_creation_time((UNIX_EPOCH + mtime_ns.div_euclid(100)).max(0) as u64)
        .volume_serial_number(0x4151_5231)
        // (nothing ever changes: the driver keeps what it was told)
        .file_info_timeout(u32::MAX)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .read_only_volume(true);
    // (the fine guard: callbacks of many dispatcher threads at once, Volume and Handle being Sync)
    let mut host: FileSystemHost<Volume> = FileSystemHost::new(params, Volume { tree })?;
    match mount_point {
        Some(point) => host.mount(point)?,
        None => host.mount(winfsp::host::MountPoint::NextFreeDrive)?,
    }
    host.start()?;
    eprintln!("aqr-mount: mounted; end the program to unmount");
    loop {
        std::thread::park();
    }
}
