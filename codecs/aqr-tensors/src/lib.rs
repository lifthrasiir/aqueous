//! Files of tensors rebuilt bit for bit: GGUF model files (llama.cpp's), PyTorch's of
//! `torch.save` and safetensors ([`tensors`]). The tensor data is in segments, each an
//! application of the part module `tensors-recons` ([`module`]), in which the floats' exponents
//! (and the top bits of their mantissas, where a sample says they pay), the quantised blocks'
//! nibbles, the lanes of integers and the indices of grids (weights of a lower precision
//! converted: [`grid`]) are symbols of a static rANS ([`rans`]), and the rest goes as it is
//! ([`layout`]). `formula` is the entry point (a module of aqr-formula); `FORMAT.md`
//! specifies it.

#[cfg(feature = "encode")]
pub mod formula;
#[cfg(feature = "encode")]
pub mod gguf;
#[cfg(feature = "encode")]
pub mod grid;
pub mod gridval;
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
