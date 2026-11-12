//! Files of tensors rebuilt bit for bit: GGUF model files (llama.cpp's), PyTorch's of
//! `torch.save` and safetensors ([`tensors`]). The tensor data is in segments, each an
//! application of the part module `tensors-recons` ([`module`]), in which the floats' exponents
//! and the quantised blocks' nibbles are symbols of a static rANS ([`rans`]) and the rest goes
//! as it is ([`layout`]). `formula` is the entry point (a module of aqr-formula); `FORMAT.md`
//! specifies it.

#[cfg(feature = "encode")]
pub mod formula;
#[cfg(feature = "encode")]
pub mod gguf;
pub mod layout;
pub mod module;
#[cfg(feature = "encode")]
pub mod pth;
pub mod rans;
#[cfg(feature = "encode")]
pub mod safetensors;
#[cfg(feature = "encode")]
pub mod synth;
#[cfg(feature = "encode")]
pub mod tensors;

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
