//! Android resource tables (`resources.arsc`) rebuilt bit for bit (`FORMAT.md`): the string
//! pools' strings without their offsets and lengths, the type chunks' entries by field with
//! their offsets derived and their keys and values predicted by the configurations before, the
//! other chunks as they are. `formula` is the entry point (see aqr-formula).

pub mod arsc;
#[cfg(feature = "encode")]
pub mod encode;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

/// The streams' reads failing ([`aqr_common::cursor`]): corrupt input, as all of it.
impl From<aqr_common::Corrupt> for Error {
    fn from(_: aqr_common::Corrupt) -> Error {
        corrupt()
    }
}
