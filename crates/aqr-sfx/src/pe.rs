//! What a self-extracting program's settings make of its stub's image ([`apply`],
//! docs/sfx-config-design.md, 5): its resources (an icon, `VERSIONINFO`, the manifest) and its
//! subsystem. The stub is packed by UPX, whose sections are not to be rewritten; the resources
//! are written as a section of their own after the last ([`with_resources`]), the data directory
//! of resources pointed at it: the resources the stub has are kept where they are (their data
//! not moved, where UPX may unpack it as the program runs), but those of the types written,
//! which go. The subsystem is the optional header's field (the PE checksum is not checked of a
//! program).

use crate::config::{Pe, VersionInfo};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// The language of the resources written: U.S. English, as tools write them.
const LANG: u16 = 0x0409;

/// A resource's name or type: a number, or a name (UTF-16).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Id {
    // (names before numbers, as a directory orders its entries)
    Name(Vec<u16>),
    Num(u16),
}

/// A resource: its type, name and language, and its data (bytes to write, or where it is in the
/// image already).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub kind: Id,
    pub name: Id,
    pub lang: u16,
    pub data: Data,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Bytes(Vec<u8>),
    /// At this RVA of the image, of this size, with this code page.
    At(u32, u32, u32),
}

/// The stub `exe` as `pe` tells: its subsystem, and the resources made of the icon, the version
/// and the manifest.
pub fn apply(exe: &[u8], pe: &Pe) -> Result<Vec<u8>, String> {
    let mut add = Vec::new();
    let res = |kind: u16, name: u16, data: Vec<u8>| Resource {
        kind: Id::Num(kind),
        name: Id::Num(name),
        lang: LANG,
        data: Data::Bytes(data),
    };
    if let Some(ico) = &pe.icon {
        let images = icon_images(ico)?;
        let mut group = vec![0, 0, 1, 0];
        group.extend_from_slice(&(images.len() as u16).to_le_bytes());
        for (i, (entry, image)) in images.iter().enumerate() {
            // (the entry of the .ico but its offset: the number of its RT_ICON instead)
            group.extend_from_slice(&entry[..12]);
            group.extend_from_slice(&(i as u16 + 1).to_le_bytes());
            add.push(res(RT_ICON, i as u16 + 1, image.to_vec()));
        }
        add.push(res(RT_GROUP_ICON, 1, group));
    }
    if let Some(v) = &pe.version {
        add.push(res(RT_VERSION, 1, version_info(v)));
    }
    add.push(res(RT_MANIFEST, 1, pe.manifest.clone()));
    let mut out = with_resources(exe, add)?;
    set_subsystem(&mut out, pe.gui)?;
    Ok(out)
}

/// An image of an icon file: its directory entry (16 bytes) and its data.
pub type IconImage<'a> = (&'a [u8], &'a [u8]);

/// The images of icon file `ico`, checked.
pub fn icon_images(ico: &[u8]) -> Result<Vec<IconImage<'_>>, String> {
    let u16_at = |o: usize| ico.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if u16_at(0) != Some(0) || u16_at(2) != Some(1) {
        return Err("not an icon (.ico)".into());
    }
    let n = u16_at(4).unwrap() as usize;
    if n == 0 {
        return Err("an icon of no images".into());
    }
    let mut images = Vec::with_capacity(n);
    for i in 0..n {
        let e = ico.get(6 + 16 * i..22 + 16 * i).ok_or("an icon cut short")?;
        let (len, at) =
            (u32::from_le_bytes(e[8..12].try_into().unwrap()), u32::from_le_bytes(e[12..16].try_into().unwrap()));
        let data = ico.get(at as usize..at as usize + len as usize).ok_or("an icon's image out of the file")?;
        // (a PNG, or a DIB of its BITMAPINFOHEADER)
        if !data.starts_with(b"\x89PNG\r\n\x1a\n") && !data.starts_with(&40u32.to_le_bytes()) {
            return Err(format!("an icon's image {} neither a PNG nor a bitmap", i + 1));
        }
        images.push((e, data));
    }
    Ok(images)
}

/// The manifest of a self-extracting program: run as the user (or as an administrator, `admin`),
/// for Windows 7 to 11, aware of each monitor's DPI, of Common Controls 6 (its message boxes);
/// where it is of the console (not `gui`), no console made for it where it is run from no
/// console (Windows 11 24H2 on: its messages are then of boxes).
pub fn manifest(admin: bool, gui: bool) -> String {
    let level = if admin { "requireAdministrator" } else { "asInvoker" };
    let console = match gui {
        true => "",
        false => {
            "\n      <consoleAllocationPolicy xmlns=\"http://schemas.microsoft.com/SMI/2024/WindowsSettings\">detached</consoleAllocationPolicy>"
        }
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="{level}" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{35138b9a-5d96-4fbd-8e2d-a2440225f93a}}"/>
      <supportedOS Id="{{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}}"/>
      <supportedOS Id="{{1f676c76-80e1-4239-95bb-83d0f6d0da78}}"/>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>{console}
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#
    )
}

/// `VS_VERSIONINFO` of `v`: its numbers as the file's and the product's, its strings in the
/// table of U.S. English and Unicode (040904B0).
pub fn version_info(v: &VersionInfo) -> Vec<u8> {
    let [a, b, c, d] = v.numbers.map(u32::from);
    let (ms, ls) = ((a << 16) | b, (c << 16) | d);
    let mut fixed = Vec::with_capacity(52);
    // signature, version of the structure, file's and product's versions, flags' mask and flags,
    // VOS_NT_WINDOWS32, VFT_APP, subtype, date
    for x in [0xfeef04bd, 0x10000, ms, ls, ms, ls, 0x3f, 0, 0x40004, 1, 0, 0, 0] {
        fixed.extend_from_slice(&u32::to_le_bytes(x));
    }
    let strings: Vec<Vec<u8>> = v
        .strings
        .iter()
        .map(|(k, s)| {
            let value: Vec<u8> = s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
            node(k, 1, &value, (value.len() / 2) as u16, &[])
        })
        .collect();
    let table = node("040904B0", 1, &[], 0, &strings);
    let string_info = node("StringFileInfo", 1, &[], 0, &[table]);
    let translation = node("Translation", 0, &[0x09, 0x04, 0xb0, 0x04], 4, &[]);
    let var_info = node("VarFileInfo", 1, &[], 0, &[translation]);
    node("VS_VERSION_INFO", 0, &fixed, 52, &[string_info, var_info])
}

/// A node of `VS_VERSIONINFO`: its length, the length of its value (`value_len`: in words of a
/// text, else bytes), its type (1 text, 0 binary), its key, its value and its children, each at
/// 4 bytes.
fn node(key: &str, ty: u16, value: &[u8], value_len: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut n = vec![0, 0];
    n.extend_from_slice(&value_len.to_le_bytes());
    n.extend_from_slice(&ty.to_le_bytes());
    key.encode_utf16().chain([0]).for_each(|w| n.extend_from_slice(&w.to_le_bytes()));
    let pad = |n: &mut Vec<u8>| n.resize(n.len().next_multiple_of(4), 0);
    pad(&mut n);
    n.extend_from_slice(value);
    for c in children {
        pad(&mut n);
        n.extend_from_slice(c);
    }
    let len = n.len() as u16;
    n[..2].copy_from_slice(&len.to_le_bytes());
    n
}

/// The facts of a PE image that writing it again needs.
struct Image {
    /// Where its optional header is, and its section table.
    optional: usize,
    table: usize,
    sections: usize,
    file_align: u32,
    section_align: u32,
    /// Where its first section's data is in its file (the headers end before).
    first_raw: usize,
    /// The end of its sections in memory.
    end_rva: u32,
    /// Of the optional header of PE32+, not PE32.
    plus: bool,
}

fn image(exe: &[u8]) -> Result<Image, String> {
    let bad = || "the stub not a PE image".to_string();
    let u16_at = |o: usize| exe.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(bad);
    let u32_at = |o: usize| exe.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(bad);
    let pe = u32_at(0x3c)? as usize;
    if exe.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(bad());
    }
    let (sections, optional_len) = (u16_at(pe + 6)?, u16_at(pe + 20)?);
    let optional = pe + 24;
    let plus = match u16_at(optional)? {
        0x20b => true,
        0x10b => false,
        _ => return Err(bad()),
    };
    let table = optional + optional_len;
    let (mut first_raw, mut end_rva) = (exe.len(), 0);
    for s in (0..sections).map(|i| table + 40 * i) {
        let (vsize, rva, raw_len, raw) = (u32_at(s + 8)?, u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?);
        if raw_len > 0 {
            first_raw = first_raw.min(raw as usize);
        }
        end_rva = end_rva.max(rva + vsize.max(raw_len));
    }
    Ok(Image {
        optional,
        table,
        sections,
        file_align: u32_at(optional + 36)?,
        section_align: u32_at(optional + 32)?,
        first_raw,
        end_rva,
        plus,
    })
}

/// Where the data directory `i` of `im` is in the file.
fn directory(im: &Image, i: usize) -> usize {
    im.optional + if im.plus { 112 } else { 96 } + 8 * i
}

/// Sets the subsystem of `exe`: of windows (2) where `gui`, else of the console (3).
pub fn set_subsystem(exe: &mut [u8], gui: bool) -> Result<(), String> {
    let im = image(exe)?;
    exe[im.optional + 68..im.optional + 70].copy_from_slice(&(if gui { 2u16 } else { 3 }).to_le_bytes());
    Ok(())
}

/// The file offset of RVA `rva` of `exe` (in one of its sections' data).
fn offset(exe: &[u8], im: &Image, rva: u32) -> Option<usize> {
    let u32_at = |o: usize| exe.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    (0..im.sections).map(|i| im.table + 40 * i).find_map(|s| {
        let (vsize, at, raw_len, raw) = (u32_at(s + 8)?, u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?);
        (rva >= at && rva - at < raw_len.min(vsize.max(raw_len))).then(|| raw as usize + (rva - at) as usize)
    })
}

/// The resources of `exe` (their data where it is: [`Data::At`]).
pub fn resources(exe: &[u8]) -> Result<Vec<Resource>, String> {
    let im = image(exe)?;
    let dir = directory(&im, 2);
    let rva = u32::from_le_bytes(exe[dir..dir + 4].try_into().unwrap());
    if rva == 0 {
        return Ok(Vec::new());
    }
    let bad = || "the stub's resources not valid".to_string();
    let base = offset(exe, &im, rva).ok_or_else(bad)?;
    let u16_at = |o: usize| exe.get(base + o..base + o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(bad);
    let u32_at =
        |o: usize| exe.get(base + o..base + o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(bad);
    // the entries of the directory at `o`: each its id, and the offset of what it points at
    // (a directory where its top bit is set)
    let entries = |o: usize| -> Result<Vec<(Id, u32)>, String> {
        let n = u16_at(o + 12)? as usize + u16_at(o + 14)? as usize;
        (0..n)
            .map(|i| {
                let (name, to) = (u32_at(o + 16 + 8 * i)?, u32_at(o + 20 + 8 * i)?);
                let id = match name & 0x8000_0000 {
                    0 => Id::Num(name as u16),
                    _ => {
                        let s = (name & 0x7fff_ffff) as usize;
                        let len = u16_at(s)? as usize;
                        Id::Name((0..len).map(|k| u16_at(s + 2 + 2 * k)).collect::<Result<_, _>>()?)
                    }
                };
                Ok((id, to))
            })
            .collect()
    };
    let sub = |to: u32| if to & 0x8000_0000 != 0 { Ok((to & 0x7fff_ffff) as usize) } else { Err(bad()) };
    let mut out = Vec::new();
    for (kind, to) in entries(0)? {
        for (name, to) in entries(sub(to)?)? {
            for (lang, to) in entries(sub(to)?)? {
                if to & 0x8000_0000 != 0 {
                    return Err(bad());
                }
                let o = to as usize;
                let data = Data::At(u32_at(o)?, u32_at(o + 4)?, u32_at(o + 8)?);
                let lang = match lang {
                    Id::Num(l) => l,
                    Id::Name(_) => return Err(bad()),
                };
                out.push(Resource { kind: kind.clone(), name: name.clone(), lang, data });
            }
        }
    }
    Ok(out)
}

/// `exe` with `add` in its resources (in place of all those of the types of `add`), written as
/// a section of its own after its last ([module](self)).
pub fn with_resources(exe: &[u8], add: Vec<Resource>) -> Result<Vec<u8>, String> {
    let im = image(exe)?;
    let mut all: Vec<Resource> =
        resources(exe)?.into_iter().filter(|r| !add.iter().any(|a| a.kind == r.kind)).collect();
    all.extend(add);
    all.sort_by(|a, b| (&a.kind, &a.name, a.lang).cmp(&(&b.kind, &b.name, b.lang)));
    if all.windows(2).any(|w| (&w[0].kind, &w[0].name, w[0].lang) == (&w[1].kind, &w[1].name, w[1].lang)) {
        return Err("resources of the same type, name and language".into());
    }
    let header = im.table + 40 * (im.sections + 1);
    if header > im.first_raw {
        return Err("no room in the stub's headers for one more section".into());
    }
    let rva = im.end_rva.next_multiple_of(im.section_align);
    let section = resource_section(&all, rva);
    let raw_len = (section.len() as u32).next_multiple_of(im.file_align);
    let raw = exe.len().next_multiple_of(im.file_align as usize);

    let mut out = exe.to_vec();
    out.resize(raw, 0);
    out.extend_from_slice(&section);
    out.resize(raw + raw_len as usize, 0);
    let s = im.table + 40 * im.sections;
    let mut h = [0u8; 40];
    h[..8].copy_from_slice(b".rsrc\0\0\0");
    if (0..im.sections).any(|i| &exe[im.table + 40 * i..im.table + 40 * i + 8] == b".rsrc\0\0\0") {
        h[..8].copy_from_slice(b".rsrc2\0\0");
    }
    for (o, v) in [(8, section.len() as u32), (12, rva), (16, raw_len), (20, raw as u32), (36, 0x4000_0040)] {
        h[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    out[s..s + 40].copy_from_slice(&h);
    let pe = im.optional - 24;
    out[pe + 6..pe + 8].copy_from_slice(&(im.sections as u16 + 1).to_le_bytes());
    let image_size = (rva + section.len() as u32).next_multiple_of(im.section_align);
    out[im.optional + 56..im.optional + 60].copy_from_slice(&image_size.to_le_bytes());
    let init = im.optional + 8;
    let n = u32::from_le_bytes(out[init..init + 4].try_into().unwrap()).saturating_add(raw_len);
    out[init..init + 4].copy_from_slice(&n.to_le_bytes());
    let dir = directory(&im, 2);
    out[dir..dir + 4].copy_from_slice(&rva.to_le_bytes());
    out[dir + 4..dir + 8].copy_from_slice(&(section.len() as u32).to_le_bytes());
    Ok(out)
}

/// The resource section of `all` (sorted by type, name and language), at `rva`: the
/// directories, then the data entries, the names, and the data written.
fn resource_section(all: &[Resource], rva: u32) -> Vec<u8> {
    // the tree: types, their names, their languages (indices into `all`)
    // (each type's names, each name's languages)
    type Names<'a> = Vec<(&'a Id, Vec<usize>)>;
    let mut types: Vec<(&Id, Names)> = Vec::new();
    for (i, r) in all.iter().enumerate() {
        if types.last().is_none_or(|t| t.0 != &r.kind) {
            types.push((&r.kind, Vec::new()));
        }
        let names = &mut types.last_mut().unwrap().1;
        if names.last().is_none_or(|n| n.0 != &r.name) {
            names.push((&r.name, Vec::new()));
        }
        names.last_mut().unwrap().1.push(i);
    }
    let dir_len = |n: usize| 16 + 8 * n;
    let mut dirs = dir_len(types.len());
    for (_, names) in &types {
        dirs += dir_len(names.len()) + names.iter().map(|(_, l)| dir_len(l.len())).sum::<usize>();
    }
    let entries_at = dirs;
    let mut strings_at = entries_at + 16 * all.len();
    let mut strings = Vec::new();
    let string_of = |id: &Id, strings: &mut Vec<u8>| -> u32 {
        match id {
            Id::Num(n) => *n as u32,
            Id::Name(s) => {
                let at = strings_at + strings.len();
                strings.extend_from_slice(&(s.len() as u16).to_le_bytes());
                s.iter().for_each(|w| strings.extend_from_slice(&w.to_le_bytes()));
                0x8000_0000 | at as u32
            }
        }
    };
    // (the names' offsets known once all are written: two passes, names first)
    let mut names_of: Vec<u32> = Vec::new();
    for (kind, names) in &types {
        names_of.push(string_of(kind, &mut strings));
        for (name, _) in names {
            names_of.push(string_of(name, &mut strings));
        }
    }
    strings_at += strings.len();
    let data_at = strings_at.next_multiple_of(8);

    let mut out = vec![0u8; data_at];
    let put = |out: &mut Vec<u8>, at: usize, v: u32| out[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let header = |out: &mut Vec<u8>, at: usize, ids: &mut dyn Iterator<Item = &Id>| {
        let (mut named, mut numbered) = (0u16, 0u16);
        for id in ids {
            match id {
                Id::Name(_) => named += 1,
                Id::Num(_) => numbered += 1,
            }
        }
        out[at + 12..at + 14].copy_from_slice(&named.to_le_bytes());
        out[at + 14..at + 16].copy_from_slice(&numbered.to_le_bytes());
    };
    let string_len = strings.len();
    out[entries_at + 16 * all.len()..entries_at + 16 * all.len() + string_len].copy_from_slice(&strings);
    let mut next_dir = dir_len(types.len());
    let mut names_iter = names_of.into_iter();
    header(&mut out, 0, &mut types.iter().map(|t| t.0));
    for (ti, (_, names)) in types.iter().enumerate() {
        let type_dir = next_dir;
        next_dir += dir_len(names.len());
        put(&mut out, 16 + 8 * ti, names_iter.next().unwrap());
        put(&mut out, 20 + 8 * ti, 0x8000_0000 | type_dir as u32);
        header(&mut out, type_dir, &mut names.iter().map(|n| n.0));
        for (ni, (_, langs)) in names.iter().enumerate() {
            let name_dir = next_dir;
            next_dir += dir_len(langs.len());
            put(&mut out, type_dir + 16 + 8 * ni, names_iter.next().unwrap());
            put(&mut out, type_dir + 20 + 8 * ni, 0x8000_0000 | name_dir as u32);
            header(&mut out, name_dir, &mut std::iter::empty());
            out[name_dir + 14..name_dir + 16].copy_from_slice(&(langs.len() as u16).to_le_bytes());
            for (li, &i) in langs.iter().enumerate() {
                put(&mut out, name_dir + 16 + 8 * li, all[i].lang as u32);
                put(&mut out, name_dir + 20 + 8 * li, (entries_at + 16 * i) as u32);
            }
        }
    }
    for (i, r) in all.iter().enumerate() {
        let e = entries_at + 16 * i;
        let (at, len, page) = match &r.data {
            Data::At(at, len, page) => (*at, *len, *page),
            Data::Bytes(d) => {
                out.resize(out.len().next_multiple_of(8), 0);
                let at = rva + out.len() as u32;
                out.extend_from_slice(d);
                (at, d.len() as u32, 0)
            }
        };
        put(&mut out, e, at);
        put(&mut out, e + 4, len);
        put(&mut out, e + 8, page);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PE32+ image of one section of code and no resources, as a linker would lay it out.
    fn stub() -> Vec<u8> {
        let mut e = vec![0u8; 0x600];
        e[..2].copy_from_slice(b"MZ");
        e[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        e[0x80..0x84].copy_from_slice(b"PE\0\0");
        e[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        e[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        e[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        let o = 0x98;
        e[o..o + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        e[o + 32..o + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        e[o + 36..o + 40].copy_from_slice(&0x200u32.to_le_bytes());
        e[o + 56..o + 60].copy_from_slice(&0x2000u32.to_le_bytes());
        e[o + 60..o + 64].copy_from_slice(&0x400u32.to_le_bytes());
        e[o + 68..o + 70].copy_from_slice(&3u16.to_le_bytes());
        e[o + 108..o + 112].copy_from_slice(&16u32.to_le_bytes());
        let s = o + 240;
        e[s..s + 8].copy_from_slice(b".text\0\0\0");
        for (k, v) in [(8, 0x123u32), (12, 0x1000), (16, 0x200), (20, 0x400), (36, 0x6000_0020)] {
            e[s + k..s + k + 4].copy_from_slice(&v.to_le_bytes());
        }
        e
    }

    fn png_ico() -> Vec<u8> {
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        let images: [&[u8]; 2] = [b"\x89PNG\r\n\x1a\nfirst", b"\x89PNG\r\n\x1a\nsecond!"];
        let mut at = 6 + 16 * images.len() as u32;
        for (i, d) in images.iter().enumerate() {
            ico.extend_from_slice(&[16 << i, 16 << i, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&(d.len() as u32).to_le_bytes());
            ico.extend_from_slice(&at.to_le_bytes());
            at += d.len() as u32;
        }
        images.iter().for_each(|d| ico.extend_from_slice(d));
        ico
    }

    fn pe() -> Pe {
        Pe {
            gui: true,
            icon: Some(png_ico()),
            manifest: manifest(true, true).into_bytes(),
            version: Some(VersionInfo {
                numbers: [1, 2, 3, 4],
                strings: vec![("ProductName".into(), "Example".into())],
            }),
        }
    }

    /// The resources written are read back, the image's sizes and the section after the last.
    #[test]
    fn resources_written_and_read() {
        let s = stub();
        let out = apply(&s, &pe()).unwrap();
        let im = image(&out).unwrap();
        assert_eq!(im.sections, 2);
        let new = im.table + 40;
        assert_eq!(&out[new + 12..new + 16], &0x2000u32.to_le_bytes());
        assert_eq!(&out[new + 20..new + 24], &0x600u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([out[im.optional + 68], out[im.optional + 69]]), 2);
        // (the image's sections as they were)
        assert_eq!(&out[0x400..0x600], &s[0x400..0x600]);
        let r = resources(&out).unwrap();
        let kinds: Vec<(Id, Id)> = r.iter().map(|r| (r.kind.clone(), r.name.clone())).collect();
        use Id::Num;
        let want = [(Num(3), Num(1)), (Num(3), Num(2)), (Num(14), Num(1)), (Num(16), Num(1)), (Num(24), Num(1))];
        assert_eq!(kinds, want);
        let data = |i: usize| match r[i].data {
            Data::At(at, len, _) => {
                let o = offset(&out, &im, at).unwrap();
                &out[o..o + len as usize]
            }
            Data::Bytes(_) => unreachable!(),
        };
        assert_eq!(data(1), b"\x89PNG\r\n\x1a\nsecond!");
        assert_eq!(&data(2)[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(&data(2)[6 + 14 + 12..6 + 14 + 14], &2u16.to_le_bytes());
        assert!(String::from_utf8_lossy(data(4)).contains("requireAdministrator"));
        let v = data(3);
        assert_eq!(u16::from_le_bytes([v[0], v[1]]) as usize, v.len());
        assert_eq!(&v[40..44], &0xfeef04bdu32.to_le_bytes());

        // written again (another icon), the resources of the image kept where they are but those
        // of the types written
        let again = with_resources(
            &out,
            vec![Resource { kind: Num(14), name: Id::Name(vec![b'X' as u16]), lang: 0, data: Data::Bytes(vec![1]) }],
        )
        .unwrap();
        let r2 = resources(&again).unwrap();
        assert_eq!(r2.len(), 5);
        assert_eq!(r2.iter().filter(|x| x.data == r[3].data || x.data == r[4].data).count(), 2);
        assert!(r2.iter().any(|x| x.name == Id::Name(vec![b'X' as u16])));
        assert_eq!(image(&again).unwrap().sections, 3);
    }

    #[test]
    fn icons_checked() {
        assert!(icon_images(&png_ico()).is_ok_and(|i| i.len() == 2));
        for bad in [&b""[..], b"\0\0\x02\0\x01\0", b"\0\0\x01\0\0\0", &png_ico()[..30]] {
            assert!(icon_images(bad).is_err(), "{bad:?}");
        }
        let mut not_image = png_ico();
        // (the second image's signature)
        let n = not_image.len();
        not_image[n - 15] = b'x';
        assert!(icon_images(&not_image).unwrap_err().contains("neither"));
    }
}
