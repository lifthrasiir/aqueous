//! The banner above the dialogs' prompt and license (the settings' `ui.banner`): its image
//! decoded by WIC (PNG, BMP, JPEG, GIF: Windows 7 on), scaled from 96 DPI to the window's,
//! and made a bitmap over the dialog's colour.

use core::ffi::c_void;

type P = *mut c_void;

/// The DPI of window `h`: `GetDpiForWindow`'s where there is one (Windows 10 on), else the
/// screen's.
pub fn dpi(h: P) -> u32 {
    // SAFETY: names NUL-terminated; the function looked up of its documented type; a DC of
    // the screen released
    unsafe {
        let user32 = sys::GetModuleHandleW("user32".encode_utf16().chain([0]).collect::<Vec<u16>>().as_ptr());
        let f = sys::GetProcAddress(user32, c"GetDpiForWindow".as_ptr().cast());
        if !f.is_null() {
            let f: unsafe extern "system" fn(P) -> u32 = std::mem::transmute(f);
            let d = f(h);
            if d > 0 {
                return d;
            }
        }
        let dc = sys::GetDC(std::ptr::null_mut());
        let d = sys::GetDeviceCaps(dc, 88); // LOGPIXELSX
        sys::ReleaseDC(std::ptr::null_mut(), dc);
        if d > 0 { d as u32 } else { 96 }
    }
}

/// Image `data` as a bitmap of 24 bits at `dpi` (its size taken as of 96 DPI), what it leaves
/// transparent of colour `background` (a `COLORREF`); with its width and height. `None` where
/// WIC does not decode it.
pub fn bitmap(data: &[u8], dpi: u32, background: u32) -> Option<(P, i32, i32)> {
    if u32::try_from(data.len()).is_err() {
        return None;
    }
    // SAFETY: COM of this thread (initialised here, apartment-threaded), each interface held
    // released once, after those made of it
    unsafe {
        let init = sys::CoInitializeEx(std::ptr::null_mut(), 0x2);
        let mut held = Vec::new();
        let r = decode(data, dpi, background, &mut held);
        for o in held.into_iter().rev() {
            slot::<Release>(o, 2)(o);
        }
        if init >= 0 {
            sys::CoUninitialize();
        }
        r
    }
}

type Release = unsafe extern "system" fn(P) -> u32;
type Make = unsafe extern "system" fn(P, *mut P) -> i32;

/// Method `i` of the vtable of COM object `o`, as `F`.
unsafe fn slot<F: Copy>(o: P, i: usize) -> F {
    // SAFETY: (the caller's) `o` an object whose vtable has a method of type `F` at `i`
    unsafe { std::mem::transmute_copy(&*(*(o as *const *const usize)).add(i)) }
}

/// [`bitmap`]'s work, the interfaces it makes noted in `held`.
unsafe fn decode(data: &[u8], dpi: u32, background: u32, held: &mut Vec<P>) -> Option<(P, i32, i32)> {
    let ok = |hr: i32| (hr >= 0).then_some(());
    let null = std::ptr::null_mut();
    // SAFETY: the interfaces' methods at the slots of their documented vtables (wincodec.h)
    unsafe {
        let mut f = null;
        ok(sys::CoCreateInstance(&sys::CLSID_WIC_IMAGING_FACTORY1, null, 0x1, &sys::IID_WIC_IMAGING_FACTORY, &mut f))?;
        held.push(f);
        // IWICImagingFactory::CreateStream, IWICStream::InitializeFromMemory
        let mut s = null;
        ok(slot::<Make>(f, 14)(f, &mut s))?;
        held.push(s);
        type FromMemory = unsafe extern "system" fn(P, *const u8, u32) -> i32;
        ok(slot::<FromMemory>(s, 16)(s, data.as_ptr(), data.len() as u32))?;
        // CreateDecoderFromStream (no vendor, metadata on demand), its first frame, its size
        let mut d = null;
        type FromStream = unsafe extern "system" fn(P, P, *const c_void, u32, *mut P) -> i32;
        ok(slot::<FromStream>(f, 4)(f, s, std::ptr::null(), 0, &mut d))?;
        held.push(d);
        let mut frame = null;
        ok(slot::<unsafe extern "system" fn(P, u32, *mut P) -> i32>(d, 13)(d, 0, &mut frame))?;
        held.push(frame);
        let (mut w, mut h) = (0u32, 0u32);
        ok(slot::<unsafe extern "system" fn(P, *mut u32, *mut u32) -> i32>(frame, 3)(frame, &mut w, &mut h))?;
        let scaled = |n: u32| ((n as u64 * dpi as u64 + 48) / 96).max(1);
        let (tw, th) = (scaled(w), scaled(h));
        if tw * th > 1 << 24 {
            return None;
        }
        let (tw, th) = (tw as u32, th as u32);
        // CreateFormatConverter: premultiplied BGRA (no dither, no palette), then
        // CreateBitmapScaler (Fant) where the DPI is not 96
        let mut c = null;
        ok(slot::<Make>(f, 10)(f, &mut c))?;
        held.push(c);
        type Convert = unsafe extern "system" fn(P, P, *const sys::Guid, u32, P, f64, u32) -> i32;
        ok(slot::<Convert>(c, 8)(c, frame, &sys::GUID_WIC_PIXEL_FORMAT_32BPP_PBGRA, 0, null, 0.0, 0))?;
        let mut source = c;
        if (tw, th) != (w, h) {
            let mut sc = null;
            ok(slot::<Make>(f, 11)(f, &mut sc))?;
            held.push(sc);
            ok(slot::<unsafe extern "system" fn(P, P, u32, u32, u32) -> i32>(sc, 8)(sc, c, tw, th, 3))?;
            source = sc;
        }
        let (tw, th) = (tw as usize, th as usize);
        let mut px = vec![0u8; tw * th * 4];
        type Copy = unsafe extern "system" fn(P, *const c_void, u32, u32, *mut u8) -> i32;
        ok(slot::<Copy>(source, 7)(source, std::ptr::null(), tw as u32 * 4, px.len() as u32, px.as_mut_ptr()))?;

        // over the background, as rows of 24 bits (top down; a bitmap of 32 the static control
        // of common controls 6 copies)
        let stride = (tw * 3 + 3) & !3;
        let info = sys::BitmapInfoHeader {
            size: 40,
            width: tw as i32,
            height: -(th as i32),
            planes: 1,
            bits: 24,
            compression: 0,
            size_image: 0,
            x_pels: 0,
            y_pels: 0,
            used: 0,
            important: 0,
        };
        let mut bits: *mut u8 = std::ptr::null_mut();
        let bmp = sys::CreateDIBSection(null, &info, 0, &mut bits, null, 0);
        if bmp.is_null() || bits.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts_mut(bits, stride * th);
        let bg = [(background >> 16) as u8 as u32, (background >> 8) as u8 as u32, background as u8 as u32];
        for y in 0..th {
            for x in 0..tw {
                let p = &px[(y * tw + x) * 4..][..4];
                let left = 255 - p[3] as u32;
                for k in 0..3 {
                    out[y * stride + x * 3 + k] = (p[k] as u32 + (bg[k] * left + 127) / 255).min(255) as u8;
                }
            }
        }
        Some((bmp, tw as i32, th as i32))
    }
}

/// Frees a bitmap of [`bitmap`].
pub fn free(b: P) {
    // SAFETY: a bitmap made here, selected nowhere
    unsafe { sys::DeleteObject(b) };
}

mod sys {
    use super::P;

    #[repr(C)]
    pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

    pub const CLSID_WIC_IMAGING_FACTORY1: Guid =
        Guid(0xcaca_f262, 0x9370, 0x4615, [0xa1, 0x3b, 0x9f, 0x55, 0x39, 0xda, 0x4c, 0x0a]);
    pub const IID_WIC_IMAGING_FACTORY: Guid =
        Guid(0xec5e_c8a9, 0xc395, 0x4314, [0x9c, 0x77, 0x54, 0xd7, 0xa9, 0x35, 0xff, 0x70]);
    pub const GUID_WIC_PIXEL_FORMAT_32BPP_PBGRA: Guid =
        Guid(0x6fdd_c324, 0x4e03, 0x4bfe, [0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, 0x10]);

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub size: u32,
        pub width: i32,
        pub height: i32,
        pub planes: u16,
        pub bits: u16,
        pub compression: u32,
        pub size_image: u32,
        pub x_pels: i32,
        pub y_pels: i32,
        pub used: u32,
        pub important: u32,
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn CoInitializeEx(reserved: P, model: u32) -> i32;
        pub fn CoUninitialize();
        pub fn CoCreateInstance(clsid: *const Guid, outer: P, context: u32, iid: *const Guid, out: *mut P) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        pub fn CreateDIBSection(
            dc: P,
            info: *const BitmapInfoHeader,
            usage: u32,
            bits: *mut *mut u8,
            section: P,
            offset: u32,
        ) -> P;
        pub fn DeleteObject(o: P) -> i32;
        pub fn GetDeviceCaps(dc: P, index: i32) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn GetDC(window: P) -> P;
        pub fn ReleaseDC(window: P, dc: P) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetModuleHandleW(name: *const u16) -> P;
        pub fn GetProcAddress(module: P, name: *const u8) -> P;
    }
}
