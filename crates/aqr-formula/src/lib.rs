//! Entry formulas (docs/xform-design.md §4.2): what a recompressor makes of a file, as an
//! expression over the data it extracted rather than one opaque byte string.
//!
//! A [`Formula`] is a tree of [`Node`]s over [`Stream`]s:
//! - `slice`: a byte range of a stream;
//! - `concat`: nodes one after another;
//! - `pack`: nodes as a [`packed`] byte sequence, for a module taking a list of them as one
//!   input;
//! - `apply`: a [`Module`] (a pure function of its inputs, resources and parameters) applied
//!   to nodes, declaring the size of its output;
//! - `output`: an output of an application of several outputs, [`Shared`], which the formula
//!   lists apart and whose every output is taken by exactly one `output` node: an output of
//!   the module, or one split from its only output by the sizes declared;
//! - `patch`: a [`patch`] of a node against another one, its source.
//!
//! Streams are not placed yet: the archive encoder lays them out into non-leaf blocks, and
//! [`Stream::class`] tells it which streams are alike (to share frames and a dictionary, §3),
//! and [`Stream::kind`] what the recompressor knows of the bytes: whether they may hold files
//! for another recompressor to look for, are one such file, or are not worth looking into.
//! What belongs to the archive is left out: `ref` to other entries, and the resource
//! declarations of `apply` (memory limit, capacities), which the archive encoder
//! measures while verifying (§6). [`Resource`]s (§5.6), the models that modules take, are
//! held by the applications, and the archive encoder stores each once.
//!
//! A formula is evaluated, and stored as an Aqueous archive, by aqr-archive (its `Evaluate` and
//! `encode`): this crate is what recompressors declare, with what they need to make it (the
//! [`store`] of large buffers, [`patch`]es, the [`probe`] of whether they pay).

mod bytes;
mod formula;
mod module;
mod node;
pub mod packed;
pub mod patch;
mod pieces;
pub mod probe;
pub mod store;
mod stream;

pub use bytes::*;
pub use formula::*;
pub use module::*;
pub use node::*;
pub use stream::*;

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
