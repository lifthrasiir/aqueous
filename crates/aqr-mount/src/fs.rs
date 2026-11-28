//! The volume: a root directory holding `hello.txt`, of fixed contents.
//!
//! WinFsp's driver asks the dispatcher threads (in this process) for what the cache and its
//! own checks cannot answer. With a read-only volume and information that never times out
//! (`file_info_timeout(u32::MAX)`), what comes here is the opening of names, the first read of
//! each page (the cache manager's paging reads) and directory listings.

use std::ffi::{OsStr, c_void};

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

/// 2026-01-01 00:00 UTC as a FILETIME (100 ns since 1601).
const TIME: u64 = (1_767_225_600 + 11_644_473_600) * 10_000_000;

const NAME: &str = "hello.txt";
const DATA: &[u8] = b"Hello from Aqueous, through WinFsp.\r\n";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Node {
    Root,
    File,
}

impl Node {
    /// `\` or `\hello.txt`, in any case (the volume is case-insensitive).
    fn lookup(name: &U16CStr) -> Option<Node> {
        let name = name.to_string_lossy();
        match name.strip_prefix('\\') {
            Some("") => Some(Node::Root),
            Some(rest) if rest.eq_ignore_ascii_case(NAME) => Some(Node::File),
            _ => None,
        }
    }

    fn attributes(self) -> u32 {
        match self {
            Node::Root => FILE_ATTRIBUTE_DIRECTORY,
            Node::File => FILE_ATTRIBUTE_READONLY,
        }
    }

    fn fill(self, info: &mut FileInfo) {
        let size = match self {
            Node::Root => 0,
            Node::File => DATA.len() as u64,
        };
        info.file_attributes = self.attributes();
        info.reparse_tag = 0;
        info.file_size = size;
        info.allocation_size = size.next_multiple_of(4096);
        info.creation_time = TIME;
        info.last_access_time = TIME;
        info.last_write_time = TIME;
        info.change_time = TIME;
        info.index_number = self as u64 + 1;
        info.hard_links = 0;
        info.ea_size = 0;
    }
}

/// An open handle: the node, and for the root the listing WinFsp keeps between the calls of
/// one enumeration.
pub struct Handle {
    node: Node,
    listing: DirBuffer,
}

struct Volume;

impl FileSystemContext for Volume {
    type FileContext = Handle;

    /// No security descriptor: WinFsp then grants what the volume's flags allow (reading).
    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let node = Node::lookup(file_name).ok_or(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;
        Ok(FileSecurity { reparse: false, sz_security_descriptor: 0, attributes: node.attributes() })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Handle> {
        let node = Node::lookup(file_name).ok_or(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;
        match node {
            Node::File if create_options & FILE_DIRECTORY_FILE != 0 => {
                return Err(FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY));
            }
            Node::Root if create_options & FILE_NON_DIRECTORY_FILE != 0 => {
                return Err(FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
            }
            _ => {}
        }
        node.fill(file_info.as_mut());
        Ok(Handle { node, listing: DirBuffer::new() })
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
        handle.node.fill(file_info);
        Ok(())
    }

    fn read(&self, handle: &Handle, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        let data = match handle.node {
            Node::Root => return Err(FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY)),
            Node::File => DATA,
        };
        let Some(rest) = usize::try_from(offset).ok().and_then(|o| data.get(o..)).filter(|r| !r.is_empty()) else {
            return Err(FspError::NTSTATUS(STATUS_END_OF_FILE));
        };
        let n = rest.len().min(buffer.len());
        buffer[..n].copy_from_slice(&rest[..n]);
        Ok(n as u32)
    }

    /// The root's one entry. The pattern (`*.txt`) is the driver's to apply (the volume does
    /// not ask for it), and the root of a volume lists neither `.` nor `..`.
    fn read_directory(
        &self,
        handle: &Handle,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if handle.node != Node::Root {
            return Err(FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY));
        }
        // (filled at the first call of an enumeration, then read on from the marker)
        if let Ok(lock) = handle.listing.acquire(marker.is_none(), None) {
            let mut entry: DirInfo = DirInfo::new();
            Node::File.fill(entry.file_info_mut());
            entry.set_name(OsStr::new(NAME))?;
            lock.write(&mut entry)?;
        }
        Ok(handle.listing.read(marker, buffer))
    }

    fn get_volume_info(&self, info: &mut VolumeInfo) -> winfsp::Result<()> {
        info.total_size = DATA.len().next_multiple_of(4096) as u64;
        info.free_size = 0;
        info.set_volume_label("Aqueous");
        Ok(())
    }
}

/// Mounts the volume and serves it until the process ends.
pub fn serve(mount_point: Option<&OsStr>) -> winfsp::Result<()> {
    let mut params = VolumeParams::new();
    params
        .filesystem_name("aqr")
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .volume_creation_time(TIME)
        .volume_serial_number(0x4151_5231)
        // (nothing ever changes: the driver keeps what it was told)
        .file_info_timeout(u32::MAX)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .read_only_volume(true);
    // (the fine guard: callbacks of many dispatcher threads at once, Volume and Handle being Sync)
    let mut host: FileSystemHost<Volume> = FileSystemHost::new(params, Volume)?;
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
