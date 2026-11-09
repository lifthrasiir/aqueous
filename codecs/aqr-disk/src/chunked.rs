//! Disk images whose disk is stored in chunks, some of them compressed: each chunk a run of the
//! disk, where its bytes are in the image's file and how they are stored (`Chunked`). Of UDIF
//! images (`crate::dmg`), qcow2 images of compressed clusters (`crate::container::qcow2_chunks`),
//! streamOptimized VMDK extents (`crate::container::vmdk_stream_chunks`) and EnCase's images
//! (`crate::ewf`); aqr-compress rebuilds the compressed chunks from the disk.

use std::ops::Range;

/// How a chunk is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// Zeros (or sectors ignored: zeros as well), of no bytes.
    Zeros,
    Raw,
    /// A zlib stream (RFC 1950: its header, DEFLATE, its Adler-32).
    Zlib,
    /// A raw DEFLATE stream, what follows it to the chunk's end not of it (qcow2's padding).
    Deflate,
    Bzip2,
    Lzfse,
    Lzma,
    Adc,
}

/// A chunk: the bytes of the disk it gives, where its bytes are in the file (of a compressed one,
/// where they may be: its stream may end before), and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub disk: Range<u64>,
    pub file: Range<usize>,
    pub how: How,
}

/// An image of chunks: its disk's bytes, and its chunks (in no order).
#[derive(Debug, Clone)]
pub struct Chunked {
    pub len: u64,
    pub chunks: Vec<Chunk>,
}

/// The chunks of image `d`, where it is a UDIF image or a compressed container ([`container`]).
pub fn of(d: &[u8]) -> Option<Chunked> {
    crate::dmg::parse(d).map(|u| Chunked { len: u.len, chunks: u.chunks }).or_else(|| container(d))
}

/// The chunks of container `d`, where it is a qcow2 image of any compressed cluster, a
/// streamOptimized VMDK extent or an EWF-E01 segment file.
pub fn container(d: &[u8]) -> Option<Chunked> {
    crate::container::qcow2_chunks(d)
        .or_else(|| crate::container::vmdk_stream_chunks(d))
        .or_else(|| crate::ewf::chunks(d))
}
