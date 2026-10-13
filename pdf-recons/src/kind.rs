//! What a stream holds and how it is encoded, from its dictionary.

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

pub fn filter(d: &Obj) -> Filter {
    let f = d.get(b"Filter").map(Obj::names).unwrap_or_default();
    match f.first() {
        None => Filter::None,
        Some(&(b"FlateDecode" | b"Fl")) => Filter::Flate,
        Some(&(b"DCTDecode" | b"DCT")) if f.len() == 1 => Filter::Dct,
        _ => Filter::Other,
    }
}
