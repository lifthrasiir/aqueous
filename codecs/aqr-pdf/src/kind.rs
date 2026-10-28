//! What a stream holds and how it is encoded, from its dictionary.

#[cfg(feature = "encode")]
use aqr_formula::Kind;

use crate::lex::Obj;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// Page and form content, Type 3 glyphs, and anything else not recognised.
    Content,
    /// Embedded font programs (`/Length1`, or a CFF/OpenType `/Subtype`).
    Font,
    Image,
    /// Object streams.
    ObjStm,
    /// Cross-reference streams.
    XRef,
    /// XMP metadata, ICC profiles, embedded files and the like.
    Data,
}

pub const CLASSES: [Class; 6] = [Class::Content, Class::Font, Class::Image, Class::ObjStm, Class::XRef, Class::Data];

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Content => "content",
            Class::Font => "font",
            Class::Image => "image",
            Class::ObjStm => "objstm",
            Class::XRef => "xref",
            Class::Data => "data",
        }
    }

    /// What the decoded data of streams of the class is, for whoever recompresses it in turn:
    /// content is anything not recognised as well, and data anything an embedded file is.
    #[cfg(feature = "encode")]
    pub fn kind(self) -> Kind {
        match self {
            Class::Image => Kind::Pixels,
            Class::Font => Kind::Font,
            Class::ObjStm => Kind::Text,
            Class::XRef => Kind::Table,
            Class::Content | Class::Data => Kind::Unknown,
        }
    }
}

/// The first filter to undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Filter {
    None,
    Flate,
    Dct,
    Other,
}

pub fn class(d: &Obj) -> Class {
    let ty = d.get(b"Type").and_then(Obj::as_name);
    let sub = d.get(b"Subtype").and_then(Obj::as_name);
    match (ty, sub) {
        (Some(b"XRef"), _) => Class::XRef,
        (Some(b"ObjStm"), _) => Class::ObjStm,
        (_, Some(b"Image")) => Class::Image,
        (Some(b"Metadata" | b"EmbeddedFile"), _) => Class::Data,
        (_, Some(b"Type1C" | b"CIDFontType0C" | b"OpenType")) => Class::Font,
        _ if d.get(b"Length1").is_some() => Class::Font,
        // ICC profiles.
        _ if d.get(b"N").is_some() && d.get(b"BBox").is_none() => Class::Data,
        _ => Class::Content,
    }
}

/// What the data of a FlateDecode stream of class `c` and dictionary `d` is once inflated: what
/// the class is ([`Class::kind`]) where FlateDecode is its only filter, else what the next
/// filter decodes: a JPEG file for DCTDecode, compressed data for the other image codecs, and
/// anything for the rest.
#[cfg(feature = "encode")]
pub fn inflated_kind(c: Class, d: &Obj) -> Kind {
    let f = d.get(b"Filter").map(Obj::names).unwrap_or_default();
    match f.get(1) {
        None => c.kind(),
        Some(&(b"DCTDecode" | b"DCT")) => Kind::Format("jpeg"),
        Some(&(b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode" | b"CCF")) => Kind::Compressed,
        Some(_) => Kind::Unknown,
    }
}

pub fn filter(d: &Obj) -> Filter {
    let f = d.get(b"Filter").map(Obj::names).unwrap_or_default();
    match f.first() {
        None => Filter::None,
        Some(&(b"FlateDecode" | b"Fl")) => Filter::Flate,
        Some(&(b"DCTDecode" | b"DCT")) if f.len() == 1 => Filter::Dct,
        _ => Filter::Other,
    }
}
