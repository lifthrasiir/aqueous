//! Quoted-printable bodies of MIME messages (mbox, EML, MHTML), the second module of the crate:
//! `b2t-recons-quopri` ([`module`]) writes a message again from its text without the bodies,
//! the bytes they decode to and a layout of the encoders' choices that differ from a canonical
//! encoder's; [`formula`] is the encoder that finds the bodies. `FORMAT.md` specifies it.

#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
