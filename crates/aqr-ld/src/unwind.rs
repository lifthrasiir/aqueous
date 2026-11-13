//! The exception data of a module merged into a stub ([`crate::pemerge`]), moved to where the
//! module lands. The system's unwinder looks a function up in the exception directory of the
//! image its address is in, and in the tables registered at run time (`RtlAddFunctionTable`) only
//! for addresses in no image: a merged module's functions must be in the stub's own directory,
//! their RVAs the stub's. So must every RVA that the language-specific handlers read from the
//! module's unwind data, which are the module's own: for each function entry, its unwind info
//! (chained ones followed), the handler and its data —
//!
//! - `__CxxFrameHandler3` (Rust's, for its landing pads: panics): the RVA of a `FuncInfo`, and the
//!   RVAs in it (its unwind map's actions, try blocks' handler arrays, catch handlers' types and
//!   code, its IP-to-state map, its exception-specification list);
//! - `__C_specific_handler` (the C runtime's start-up code): its scope table;
//! - a handler in the module itself (the C runtime's `__GSHandlerCheck`, linked statically into
//!   each library): its data holds no RVAs, only frame offsets.
//!
//! A handler is told by the import its thunk (`jmp [slot]`) jumps through; another import is an
//! error. The structures are those of the Visual C++ runtime's `ehdata.h` (x64, `FuncInfo` of
//! magic `0x19930520` to `0x19930522`) and of the PE format (`UNWIND_INFO`, the scope table).

use std::collections::{HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

/// The handlers whose data has RVAs.
#[derive(Clone, Copy)]
enum Handler {
    Cxx3,
    CSpecific,
    /// One in the image (`__GSHandlerCheck`): no RVAs in its data.
    Own,
}

/// The import names of image `img` (its bytes from RVA `first`), by the RVA of their slot in the
/// import address tables.
pub fn import_names(img: &[u8], first: u32, dir_rva: u32) -> Result<HashMap<u32, String>> {
    let mut names = HashMap::new();
    if dir_rva == 0 {
        return Ok(names);
    }
    let r = Reader { img, first };
    let mut d = dir_rva;
    while (0..5).any(|k| r.u32(d + k * 4).is_ok_and(|v| v != 0)) {
        let (oft, ft) = (r.u32(d)?, r.u32(d + 16)?);
        let lookup = if oft != 0 { oft } else { ft };
        for k in 0.. {
            let v = r.u64(lookup + k * 8)?;
            if v == 0 {
                break;
            }
            if v & 0x8000_0000_0000_0000 == 0 {
                names.insert(ft + k * 8, r.cstr(v as u32 + 2)?);
            }
        }
        d += 20;
    }
    Ok(names)
}

/// Rebases the exception data of image `img` (its bytes from RVA `first`, placed so that its RVA
/// `x` is the stub's `c + x`): every RVA that the unwinder and the handlers read from its unwind
/// infos, in place, given the module's import names ([`import_names`]). Gives the function
/// entries of its exception directory (`count` at `pdata_rva`) with the stub's RVAs.
pub fn rebase(
    img: &mut [u8],
    first: u32,
    pdata_rva: u32,
    count: u32,
    c: u32,
    names: &HashMap<u32, String>,
) -> Result<Vec<[u32; 3]>> {
    let mut r = Rebase {
        orig: img.to_vec(),
        img,
        first,
        c,
        names,
        done: HashSet::new(),
        infos: HashSet::new(),
        funcinfos: HashSet::new(),
    };
    let mut entries = Vec::with_capacity(count as usize);
    for i in 0..count {
        let e = pdata_rva + i * 12;
        let (begin, end, info) = (r.field(e)?, r.field(e + 4)?, r.field(e + 8)?);
        r.unwind_info(info)?;
        entries.push([begin, end, info].map(|v| v.wrapping_add(c)));
    }
    Ok(entries)
}

/// The bytes of an image by RVA.
struct Reader<'a> {
    img: &'a [u8],
    first: u32,
}

impl Reader<'_> {
    fn at(&self, rva: u32, n: usize) -> Result<&[u8]> {
        let o = rva.checked_sub(self.first).ok_or_else(|| format!("RVA {rva:#x} before the image"))? as usize;
        self.img.get(o..o + n).ok_or_else(|| format!("RVA {rva:#x} past the image"))
    }
    fn u8(&self, rva: u32) -> Result<u8> {
        Ok(self.at(rva, 1)?[0])
    }
    fn u32(&self, rva: u32) -> Result<u32> {
        Ok(u32::from_le_bytes(self.at(rva, 4)?.try_into().unwrap()))
    }
    fn u64(&self, rva: u32) -> Result<u64> {
        Ok(u64::from_le_bytes(self.at(rva, 8)?.try_into().unwrap()))
    }
    fn cstr(&self, rva: u32) -> Result<String> {
        let o = rva.checked_sub(self.first).ok_or("a name before the image")? as usize;
        let s = self.img.get(o..).ok_or("a name past the image")?;
        let end = s.iter().position(|&b| b == 0).ok_or("an unterminated name")?;
        Ok(String::from_utf8_lossy(&s[..end]).into_owned())
    }
}

struct Rebase<'a> {
    /// The image as it was: what is read (an RVA read is the module's, whether or not rebased).
    orig: Vec<u8>,
    /// The image written: each RVA field rebased once.
    img: &'a mut [u8],
    first: u32,
    c: u32,
    names: &'a HashMap<u32, String>,
    /// The RVA fields rebased, the unwind infos and the `FuncInfo`s seen (shared between entries).
    done: HashSet<u32>,
    infos: HashSet<u32>,
    funcinfos: HashSet<u32>,
}

impl Rebase<'_> {
    fn read(&self) -> Reader<'_> {
        Reader { img: &self.orig, first: self.first }
    }

    /// The value of the RVA field at `rva`, as the module had it; rebased in the image unless 0
    /// (none).
    fn field(&mut self, rva: u32) -> Result<u32> {
        let v = self.read().u32(rva)?;
        if v != 0 && self.done.insert(rva) {
            let o = (rva - self.first) as usize;
            self.img[o..o + 4].copy_from_slice(&v.wrapping_add(self.c).to_le_bytes());
        }
        Ok(v)
    }

    fn unwind_info(&mut self, info: u32) -> Result<()> {
        if !self.infos.insert(info) {
            return Ok(());
        }
        let (flags, codes) = (self.read().u8(info)? >> 3, self.read().u8(info + 2)? as u32);
        // after the header (4 bytes) and the codes (2 bytes each, an even count of them)
        let after = info + 4 + codes.next_multiple_of(2) * 2;
        const EHANDLER: u8 = 1;
        const UHANDLER: u8 = 2;
        const CHAININFO: u8 = 4;
        if flags & CHAININFO != 0 {
            // the function entry chained to, whose unwind info applies too
            self.field(after)?;
            self.field(after + 4)?;
            let chained = self.field(after + 8)?;
            return self.unwind_info(chained);
        }
        if flags & (EHANDLER | UHANDLER) == 0 {
            return Ok(());
        }
        let handler = self.field(after)?;
        let data = after + 4;
        match self.handler(handler)? {
            Handler::Cxx3 => {
                let funcinfo = self.field(data)?;
                self.funcinfo(funcinfo)
            }
            Handler::CSpecific => self.scope_table(data),
            Handler::Own => Ok(()),
        }
    }

    /// The handler at `rva`: by the import its thunk (`jmp [rip+disp32]`) jumps through.
    fn handler(&self, rva: u32) -> Result<Handler> {
        let r = self.read();
        if r.at(rva, 2)? != [0xff, 0x25] {
            return Ok(Handler::Own);
        }
        let slot = (rva + 6).wrapping_add(r.u32(rva + 2)?);
        match self.names.get(&slot).map(String::as_str) {
            Some("__CxxFrameHandler3") => Ok(Handler::Cxx3),
            Some("__C_specific_handler") => Ok(Handler::CSpecific),
            Some(name) => Err(format!("an exception handler not known: {name}")),
            None => Err(format!("an exception handler's thunk at {rva:#x} through no import")),
        }
    }

    fn funcinfo(&mut self, fi: u32) -> Result<()> {
        if !self.funcinfos.insert(fi) {
            return Ok(());
        }
        let r = self.read();
        let magic = r.u32(fi)? & 0x1fff_ffff;
        if !(0x1993_0520..=0x1993_0522).contains(&magic) {
            return Err(format!("a FuncInfo at {fi:#x} of magic {magic:#x}, not known"));
        }
        let (max_state, n_try, n_ip) = (r.u32(fi + 4)?, r.u32(fi + 12)?, r.u32(fi + 20)?);
        // the unwind map: (to state, action) each
        let unwind_map = self.field(fi + 8)?;
        for k in 0..max_state {
            self.field(unwind_map + k * 8 + 4)?;
        }
        // the try blocks: (try low, try high, catch high, catches, handler array) each
        let try_map = self.field(fi + 16)?;
        for t in 0..n_try {
            let e = try_map + t * 20;
            let n_catch = self.read().u32(e + 12)?;
            let handlers = self.field(e + 16)?;
            for h in 0..n_catch {
                self.handler_type(handlers + h * 20)?;
            }
        }
        // the IP-to-state map: (IP, state) each
        let ip_map = self.field(fi + 24)?;
        for k in 0..n_ip {
            self.field(ip_map + k * 8)?;
        }
        // (+28: the unwind help, a frame offset) the exception-specification list
        if magic >= 0x1993_0521 {
            let es = self.field(fi + 32)?;
            if es != 0 {
                let n = self.read().u32(es)?;
                let types = self.field(es + 4)?;
                for k in 0..n {
                    self.handler_type(types + k * 20)?;
                }
            }
        }
        Ok(())
    }

    /// A `HandlerType`: adjectives, the type (0: catch all), the catch object's frame offset, the
    /// handler's code, its frame.
    fn handler_type(&mut self, h: u32) -> Result<()> {
        self.field(h + 4)?;
        self.field(h + 12)?;
        Ok(())
    }

    /// `__C_specific_handler`'s scope table: the count, then (begin, end, handler, target) each,
    /// the handler a filter's RVA or 1 (EXCEPTION_EXECUTE_HANDLER), the target 0 for a
    /// termination handler.
    fn scope_table(&mut self, data: u32) -> Result<()> {
        let n = self.read().u32(data)?;
        for k in 0..n {
            let e = data + 4 + k * 16;
            self.field(e)?;
            self.field(e + 4)?;
            if self.read().u32(e + 8)? > 1 {
                self.field(e + 8)?;
            }
            self.field(e + 12)?;
        }
        Ok(())
    }
}
