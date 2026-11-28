//! The window after installing: Explorer as the user sees it, two files right-clicked (a file
//! and a `.aqr` archive), the menus that Explorer shows of them drawn open, the submenu
//! "Aqueous" too; under them a line of what the command pointed at (or chosen by the keyboard)
//! does, else what was installed; and "OK".
//!
//! - The menus are drawn by the theme's parts that Windows' menus are drawn by (`MENU`: the
//!   background, the item hot or disabled, the separator, the arrow of a submenu), in the menu
//!   font, laid out as Windows lays out the menus of Explorer (icons beside the text): measured
//!   on Windows 11 (`Layout::WIN11`, at 96 DPI; the rounded corners and the shadow, which DWM
//!   draws of a real menu, drawn here too), from the theme's metrics before it. Without visual
//!   styles (Windows 7's classic look, Windows Server's) they are drawn as classic menus are, of
//!   the system's colors (the raised edge, the disabled text embossed, the arrow a triangle),
//!   laid out as measured there (`Layout::PLAIN`); shadows and underlined access keys as the
//!   user has them. The files' icons are the system's (of their extensions, `.aqr` registered
//!   just now), selected as a list view of Explorer's draws them (`Explorer::ListView`; without
//!   it, the name on the highlight).
//! - The commands are those that `aqr-win --install` registered and aqr-win-shell's menu
//!   "Aqueous" shows of such files, with Explorer's own "Open" and "Properties" (whose tab
//!   "Versions" is Aqueous's, of `.aqr` files). "Mount as a drive" is disabled where aqr-mount
//!   or WinFsp is not installed, as the menu shows it then, its line telling what it needs.
//! - Each command is a stop of Tab (in the menu's order, a submenu's commands after the command
//!   that opens it), the arrows move within the menus as in a real one (Right into the submenu,
//!   Left out of it, and between the two files at the top level), and the window's text is the
//!   command chosen, for screen readers. Windows 11 draws its own menu first: the line tells
//!   that these are under "Show more options" there.
//!
//! Plain Win32 and GDI, the functions of Windows 8.1 and 10 (the DPI's) looked up as it runs,
//! so that the installer runs on Windows 7. `aqr-win-install --tour` shows it alone.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BF_RECT, BF_TOP, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    BeginPaint, BitBlt, COLOR_3DHILIGHT, COLOR_3DSHADOW, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
    COLOR_MENU, COLOR_MENUTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateDIBSection, CreateFontIndirectW, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_HIDEPREFIX,
    DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, DrawEdge, DrawTextW,
    EDGE_ETCHED, EDGE_RAISED, EndPaint, FW_BOLD, FillRect, GdiFlush, GetDC, GetMonitorInfoW, GetSysColor,
    GetSysColorBrush, HBITMAP, HDC, HFONT, HGDIOBJ, InvalidateRect, LOGFONTW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::UI::Accessibility::NotifyWinEvent;
use windows_sys::Win32::UI::Controls::{
    CloseThemeData, DrawThemeBackground, DrawThemeText, GetThemeInt, GetThemeMargins, GetThemePartSize, HTHEME,
    ILD_TRANSPARENT, ImageList_GetIcon, LISS_SELECTED, LVP_LISTITEM, MARGINS, MENU_POPUPBACKGROUND, MENU_POPUPBORDERS,
    MENU_POPUPCHECK, MENU_POPUPGUTTER, MENU_POPUPITEM, MENU_POPUPSEPARATOR, MENU_POPUPSUBMENU, MPI_DISABLED,
    MPI_DISABLEDHOT, MPI_HOT, MPI_NORMAL, MSM_NORMAL, OpenThemeData, TMT_BORDERSIZE, TMT_CONTENTMARGINS, TS_TRUE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_RIGHT,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BS_DEFPUSHBUTTON, CW_USEDEFAULT, CreateWindowExW, DI_NORMAL, DLGC_WANTARROWS, DLGC_WANTTAB,
    DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, DrawIconEx, GetClientRect, GetCursorPos, GetMessageW,
    HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IDCANCEL, IDOK, IMAGE_ICON, IsDialogMessageW, LR_DEFAULTCOLOR, LoadCursorW,
    LoadImageW, MSG, NONCLIENTMETRICSW, PostQuitMessage, RegisterClassExW, SPI_GETICONTITLELOGFONT,
    SPI_GETNONCLIENTMETRICS, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW,
    ShowWindow, SystemParametersInfoW, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETDLGCODE, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT,
    WM_SETFOCUS, WM_SETFONT, WM_SETICON, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows_sys::core::{GUID, PCWSTR};
use windows_sys::w;

use aqr_l10n::{common, install as l, shell, win};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// A command of a menu drawn (a separator where `text` is empty).
struct Row {
    text: String,
    about: String,
    bold: bool,
    icon: bool,
    disabled: bool,
    sub: Vec<Row>,
}

impl Row {
    fn new(text: &str, about: &str) -> Row {
        Row { text: text.into(), about: about.into(), bold: false, icon: false, disabled: false, sub: Vec::new() }
    }

    fn separator() -> Row {
        Row::new("", "")
    }

    fn is_separator(&self) -> bool {
        self.text.is_empty()
    }
}

/// A file right-clicked: its name and the menu of it.
struct Scene {
    name: String,
    menu: Vec<Row>,
}

/// The two files and their menus, as `aqr-win --install` registered them and aqr-win-shell's
/// `more` shows them (`mount`: whether aqr-mount and WinFsp are there).
fn scenes(mount: bool) -> [Scene; 2] {
    let compress = || Row { icon: true, ..Row::new(shell::compress(), l::about_compress()) };
    let more = |archive: bool| {
        let mut sub = Vec::new();
        if archive {
            let about = if mount { l::about_mount() } else { l::about_mount_missing() };
            sub.push(Row::new(shell::test(), l::about_test()));
            sub.push(Row { disabled: !mount, ..Row::new(shell::mount(), about) });
            sub.push(Row::new(shell::extract_version(), l::about_extract_version()));
            sub.push(Row::new(shell::name_version(), l::about_name_version()));
            sub.push(Row::separator());
        }
        sub.push(Row::new(shell::compress_passphrase(), l::about_compress_passphrase()));
        sub.push(Row::separator());
        sub.push(Row::new(shell::compress_zip(), l::about_compress_zip()));
        sub.push(Row::new(shell::compress_7z(), l::about_compress_7z()));
        sub.push(Row::separator());
        sub.push(Row::new(shell::compress_options(), l::about_compress_options()));
        Row { icon: true, sub, ..Row::new("Aqueous", l::about_menu()) }
    };
    let file = Scene {
        name: l::sample_file().into(),
        menu: vec![
            Row { bold: true, ..Row::new(l::open(), l::about_open()) },
            compress(),
            more(false),
            Row::separator(),
            Row::new(l::properties(), l::about_properties()),
        ],
    };
    let archive = Scene {
        name: l::sample_archive().into(),
        menu: vec![
            Row { bold: true, ..Row::new(l::open(), l::about_open_archive()) },
            Row::new(shell::extract(), l::about_extract()),
            Row::new(shell::extract_to(), l::about_extract_to()),
            compress(),
            more(true),
            Row::separator(),
            Row::new(l::properties(), l::about_properties_archive()),
        ],
    };
    [file, archive]
}

/// A command: of scene `scene`, the row `row` of its menu, and of its submenu `sub`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct At {
    scene: usize,
    row: usize,
    sub: Option<usize>,
}

/// The layout of a menu, in pixels at 96 DPI (scaled by [`App::s`]).
#[derive(Clone, Copy)]
struct Layout {
    /// From the window's top (and bottom) to the first item's, the border's pixel included.
    pad: i32,
    /// An item's height, at least; with an icon, at least; and above the text's line.
    item: i32,
    icon_item: i32,
    text_pad: i32,
    separator: i32,
    /// The hot item's background, in from the sides.
    hot_inset: i32,
    icon_x: i32,
    text_x: i32,
    /// From the text's end to the window's right.
    right: i32,
    /// The arrow of a submenu's box, its right in from the window's right.
    arrow_right: i32,
    /// The separator's line, in from the sides.
    separator_inset: i32,
    /// A submenu's left in from its parent's right.
    overlap: i32,
    radius: i32,
    /// How far the shadow reaches, and its strongest alpha.
    shadow: i32,
    shadow_alpha: f32,
    border: COLORREF,
}

impl Layout {
    /// Windows 11 (measured of a menu of `TrackPopupMenu`, `MNS_CHECKORBMP` as Explorer's).
    const WIN11: Layout = Layout {
        pad: 3,
        item: 22,
        icon_item: 22,
        text_pad: 4,
        separator: 9,
        hot_inset: 6,
        icon_x: 9,
        text_x: 39,
        right: 60,
        arrow_right: 13,
        separator_inset: 9,
        overlap: 6,
        radius: 8,
        shadow: 12,
        shadow_alpha: 0.16,
        border: rgb(229, 229, 229),
    };

    /// Without visual styles (the classic look of Windows 7, Windows Server's): measured of a
    /// menu of `TrackPopupMenu` there, of 96 DPI.
    const PLAIN: Layout = Layout {
        pad: 3,
        item: 0,
        icon_item: 19,
        text_pad: 3,
        separator: 9,
        hot_inset: 3,
        icon_x: 5,
        text_x: 22,
        right: 39,
        arrow_right: 8,
        separator_inset: 4,
        overlap: 6,
        radius: 0,
        shadow: 4,
        shadow_alpha: 0.35,
        border: 0,
    };

    /// Before, from the theme's metrics where it gives them.
    fn classic(theme: HTHEME) -> Layout {
        let mut item = MARGINS::default();
        let mut check = MARGINS::default();
        let mut check_size = SIZE { cx: 16, cy: 16 };
        let mut border = 0;
        // SAFETY: the theme open, the values written to locals (left as they were on failure)
        unsafe {
            GetThemeMargins(theme, null_mut(), MENU_POPUPITEM, 0, TMT_CONTENTMARGINS as i32, null(), &mut item);
            GetThemeMargins(theme, null_mut(), MENU_POPUPCHECK, 0, TMT_CONTENTMARGINS as i32, null(), &mut check);
            GetThemePartSize(theme, null_mut(), MENU_POPUPCHECK, 0, null(), TS_TRUE, &mut check_size);
            GetThemeInt(theme, MENU_POPUPBACKGROUND, 0, TMT_BORDERSIZE as i32, &mut border);
        }
        let gutter = check.cxLeftWidth + check_size.cx + check.cxRightWidth;
        Layout {
            pad: 3,
            item: check.cyTopHeight + check_size.cy + check.cyBottomHeight,
            icon_item: check.cyTopHeight + check_size.cy + check.cyBottomHeight,
            text_pad: 4,
            separator: 9,
            hot_inset: 3,
            icon_x: 3 + check.cxLeftWidth,
            text_x: 3 + gutter + border.clamp(2, 8) + item.cxLeftWidth,
            right: 48,
            arrow_right: 8,
            separator_inset: 3 + gutter + border.clamp(2, 8),
            overlap: 3,
            radius: 0,
            shadow: 5,
            shadow_alpha: 0.22,
            border: rgb(151, 151, 151),
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// A menu laid out: where it is, its items' tops and heights (and a separator's), and whose
/// submenu it is.
struct Placed {
    rect: RECT,
    rows: Vec<(i32, i32)>,
}

/// A scene laid out: its file's tile, its menu and the submenu.
struct ScenePlaced {
    tile: RECT,
    icon: RECT,
    label: RECT,
    menu: Placed,
    /// The row of the menu that opens it, and it.
    sub: Option<(usize, Placed)>,
}

/// The drawing's control (the window class of [`CANVAS`]).
struct App {
    main: HWND,
    canvas: HWND,
    about: HWND,
    ok: HWND,
    scenes: [Scene; 2],
    /// The line where no command is chosen.
    idle: String,
    win11: bool,
    dpi: u32,
    menu_font: HFONT,
    bold_font: HFONT,
    label_font: HFONT,
    text_font: HFONT,
    menu_theme: HTHEME,
    list_theme: HTHEME,
    layout: Layout,
    item_icon: HICON,
    file_icons: [HICON; 2],
    placed: Vec<ScenePlaced>,
    /// The command pointed at, and that chosen by the keyboard (shown where none is pointed at,
    /// while the control has the focus).
    pointed: Option<At>,
    chosen: Option<At>,
    focused: bool,
    /// Whether menus have shadows, and their access keys are underlined.
    shadows: bool,
    cues: bool,
    tracking: bool,
}

thread_local! {
    static APP: RefCell<Option<Box<App>>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_deref_mut().map(f)))
}

const MAIN: PCWSTR = w!("Aqueous.InstallerTour");
const CANVAS: PCWSTR = w!("Aqueous.InstallerTour.Menus");

/// Shows the window until it is closed: of the installation in `dir` (whose aqr-mount, with
/// WinFsp, lets archives be mounted), `held` where Explorer still runs the earlier shell
/// extension.
pub fn show(dir: &Path, held: bool) {
    let mount = dir.join("aqr-mount.exe").is_file() && winfsp();
    let win11 = aqr_l10n::windows_11_on() && std::env::var_os("AQR_TOUR_CLASSIC").is_none();
    let mut idle = l::installed_line().to_string();
    if aqr_l10n::windows_11_on() {
        idle = format!("{idle}\n{}", win::more_options());
    }
    if held {
        idle = format!("{idle}\n{}", l::restart_explorer());
    }
    // SAFETY: this program's classes registered, its windows made and run on this thread; the
    // state boxed in a thread-local until the window is destroyed
    unsafe {
        let instance = GetModuleHandleW(null());
        let cursor = LoadCursorW(null_mut(), IDC_ARROW);
        for (name, proc, brush) in
            [(MAIN, main_proc as WndProc, GetSysColorBrush(COLOR_WINDOW)), (CANVAS, canvas_proc as WndProc, null_mut())]
        {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: instance,
                hCursor: cursor,
                hbrBackground: brush,
                lpszClassName: name,
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
        }
        let title = wide("Aqueous");
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let main = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            MAIN,
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if main.is_null() {
            return;
        }
        for (size, which) in [(32, ICON_BIG), (16, ICON_SMALL)] {
            let icon = LoadImageW(instance, 1 as PCWSTR, IMAGE_ICON, size, size, LR_DEFAULTCOLOR);
            SendMessageW(main, WM_SETICON, which as usize, icon as isize);
        }
        let canvas = CreateWindowExW(
            0,
            CANVAS,
            null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            0,
            0,
            0,
            main,
            null_mut(),
            instance,
            null(),
        );
        let about = CreateWindowExW(
            0,
            w!("STATIC"),
            null(),
            WS_CHILD | WS_VISIBLE | 0x80, // SS_NOPREFIX
            0,
            0,
            0,
            0,
            main,
            null_mut(),
            instance,
            null(),
        );
        let ok_text = wide(common::ok());
        let ok = CreateWindowExW(
            0,
            w!("BUTTON"),
            ok_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            0,
            0,
            0,
            0,
            main,
            IDOK as usize as _,
            instance,
            null(),
        );
        let app = App {
            main,
            canvas,
            about,
            ok,
            scenes: scenes(mount),
            idle,
            win11,
            dpi: dpi_of(main),
            menu_font: null_mut(),
            bold_font: null_mut(),
            label_font: null_mut(),
            text_font: null_mut(),
            menu_theme: 0,
            list_theme: 0,
            layout: Layout::WIN11,
            item_icon: null_mut(),
            file_icons: [null_mut(); 2],
            placed: Vec::new(),
            pointed: None,
            chosen: None,
            focused: false,
            shadows: true,
            cues: false,
            tracking: false,
        };
        APP.with(|a| *a.borrow_mut() = Some(Box::new(app)));
        with_app(|a| {
            a.build();
            a.describe();
            a.center();
        });
        ShowWindow(main, SW_SHOW);
        SetFocus(ok);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(main, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        APP.with(|a| a.borrow_mut().take());
    }
}

/// (of `KeyboardAndMouse`, which windows-sys does not have)
const WM_MOUSELEAVE: u32 = 0x02a3;

type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// Whether WinFsp is installed: its installer's `InstallDir` (aqr-win-shell's `mount::winfsp`).
fn winfsp() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    [w!(r"SOFTWARE\WOW6432Node\WinFsp"), w!(r"SOFTWARE\WinFsp")].iter().any(|&key| {
        // SAFETY: names ending in NUL; the size of the value alone asked
        unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("InstallDir"), RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut())
                == 0
        }
    })
}

/// A function as `GetProcAddress` gives it.
type Proc = unsafe extern "system" fn() -> isize;
type GetDpiForWindow = unsafe extern "system" fn(HWND) -> u32;
type SystemParametersInfoForDpi = unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> i32;
type AdjustWindowRectExForDpi = unsafe extern "system" fn(*mut RECT, u32, i32, u32, u32) -> i32;

/// A function of user32's, where it has it (Windows 10's, of the DPI).
fn user32(name: &[u8]) -> Option<Proc> {
    // SAFETY: user32.dll, loaded with the program; a name ending in NUL
    unsafe {
        let user32 = GetModuleHandleW(w!("user32.dll"));
        if user32.is_null() { None } else { GetProcAddress(user32, name.as_ptr()) }
    }
}

/// The DPI of window `h` (the system's before Windows 10 1607).
fn dpi_of(h: HWND) -> u32 {
    let dpi = match user32(b"GetDpiForWindow\0") {
        // SAFETY: the function of that name, of that signature; a window
        Some(f) => unsafe { std::mem::transmute::<Proc, GetDpiForWindow>(f)(h) },
        // SAFETY: the screen's DC, released
        None => unsafe {
            let dc = GetDC(null_mut());
            let d = windows_sys::Win32::Graphics::Gdi::GetDeviceCaps(dc, 90); // LOGPIXELSY
            ReleaseDC(null_mut(), dc);
            d.max(0) as u32
        },
    };
    dpi.max(96)
}

/// `SystemParametersInfoW` at `dpi`, where the system has it (else at the system's).
///
/// # Safety
/// As `SystemParametersInfoW`.
unsafe fn parameters(action: u32, size: u32, param: *mut c_void, dpi: u32) -> bool {
    unsafe {
        match user32(b"SystemParametersInfoForDpi\0") {
            Some(f) => std::mem::transmute::<Proc, SystemParametersInfoForDpi>(f)(action, size, param, 0, dpi) != 0,
            None => SystemParametersInfoW(action, size, param, 0) != 0,
        }
    }
}

impl App {
    /// `v` pixels at 96 DPI at the window's.
    fn s(&self, v: i32) -> i32 {
        (v as i64 * self.dpi as i64 / 96) as i32
    }

    /// The fonts, themes, icons and layout at the DPI, the windows placed and sized.
    fn build(&mut self) {
        self.free();
        // SAFETY: GDI's and the theme's objects made for this window, freed by `free`; the
        // windows this program's
        unsafe {
            let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
            ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            parameters(SPI_GETNONCLIENTMETRICS, ncm.cbSize, &mut ncm as *mut _ as *mut c_void, self.dpi);
            let mut icon_font: LOGFONTW = std::mem::zeroed();
            parameters(SPI_GETICONTITLELOGFONT, size_of::<LOGFONTW>() as u32, &mut icon_font as *mut _ as _, self.dpi);
            // (Windows 7 and 8 scale by the system's DPI alone, and so do their metrics)
            self.menu_font = CreateFontIndirectW(&ncm.lfMenuFont);
            let mut bold = ncm.lfMenuFont;
            bold.lfWeight = FW_BOLD as i32;
            self.bold_font = CreateFontIndirectW(&bold);
            self.label_font = CreateFontIndirectW(&icon_font);
            self.text_font = CreateFontIndirectW(&ncm.lfMessageFont);
            self.menu_theme = OpenThemeData(self.canvas, w!("MENU"));
            self.list_theme = OpenThemeData(self.canvas, w!("Explorer::ListView"));
            self.layout = match (self.win11, self.menu_theme) {
                (true, _) => Layout::WIN11,
                (false, 0) => Layout::PLAIN,
                (false, theme) => Layout::classic(theme),
            };
            // (menus' shadows and access keys underlined, as the user has them)
            let mut on = 0i32;
            SystemParametersInfoW(0x1024, 0, &mut on as *mut _ as *mut c_void, 0); // SPI_GETDROPSHADOW
            self.shadows = on != 0 || self.win11;
            on = 0;
            SystemParametersInfoW(0x100A, 0, &mut on as *mut _ as *mut c_void, 0); // SPI_GETKEYBOARDCUES
            self.cues = on != 0;
            let small = self.s(16);
            self.item_icon =
                LoadImageW(GetModuleHandleW(null()), 1 as PCWSTR, IMAGE_ICON, small, small, LR_DEFAULTCOLOR);
            for (i, scene) in self.scenes.iter().enumerate() {
                self.file_icons[i] = file_icon(&scene.name, self.s(48));
            }
            for h in [self.about, self.ok] {
                SendMessageW(h, WM_SETFONT, self.text_font as usize, 1);
            }
        }
        self.place();
    }

    fn free(&mut self) {
        // SAFETY: objects made by `build`, no longer selected anywhere
        unsafe {
            for f in [self.menu_font, self.bold_font, self.label_font, self.text_font] {
                if !f.is_null() {
                    DeleteObject(f as HGDIOBJ);
                }
            }
            for t in [self.menu_theme, self.list_theme] {
                if t != 0 {
                    CloseThemeData(t);
                }
            }
            for i in [self.item_icon, self.file_icons[0], self.file_icons[1]] {
                if !i.is_null() {
                    DestroyIcon(i);
                }
            }
        }
        (self.menu_font, self.bold_font, self.label_font, self.text_font) =
            (null_mut(), null_mut(), null_mut(), null_mut());
        (self.menu_theme, self.list_theme) = (0, 0);
        (self.item_icon, self.file_icons) = (null_mut(), [null_mut(); 2]);
    }

    /// The width of `text` in `font`, and the height of a line of it.
    fn measure(&self, dc: HDC, font: HFONT, text: &str, flags: u32) -> (i32, i32) {
        let t = wide(text);
        let mut r = RECT::default();
        // SAFETY: a DC of the screen's, the text ending in NUL
        unsafe {
            let old = SelectObject(dc, font as HGDIOBJ);
            DrawTextW(dc, t.as_ptr(), -1, &mut r, DT_CALCRECT | DT_SINGLELINE | flags);
            SelectObject(dc, old);
        }
        (r.right - r.left, r.bottom - r.top)
    }

    /// A menu of `rows` laid out at (`x`, `y`).
    fn lay_menu(&self, dc: HDC, rows: &[Row], x: i32, y: i32) -> Placed {
        let ly = self.layout;
        let (_, line) = self.measure(dc, self.menu_font, "Ag", 0);
        let item = self.s(ly.item).max(line + self.s(ly.text_pad));
        let mut width = 0;
        let mut top = self.s(ly.pad);
        let mut out = Vec::new();
        for r in rows {
            let h = if r.is_separator() {
                self.s(ly.separator)
            } else {
                let font = if r.bold { self.bold_font } else { self.menu_font };
                width = width.max(self.measure(dc, font, &r.text, DT_HIDEPREFIX).0);
                if r.icon { item.max(self.s(ly.icon_item)) } else { item }
            };
            out.push((top, h));
            top += h;
        }
        let w = self.s(ly.text_x) + width + self.s(ly.right);
        Placed { rect: RECT { left: x, top: y, right: x + w, bottom: y + top + self.s(ly.pad) }, rows: out }
    }

    /// Everything laid out at the DPI: the scenes side by side, the line under them and "OK"
    /// under it; the window sized to them.
    fn place(&mut self) {
        let margin = self.s(16);
        let shadow = self.s(self.layout.shadow);
        // SAFETY: the screen's DC, released; this program's windows
        unsafe {
            let dc = GetDC(null_mut());
            let mut placed = Vec::new();
            let mut x = margin;
            let mut bottom = 0;
            for scene in &self.scenes {
                let tile_w = self.s(84);
                let icon = self.s(48);
                let label_w = tile_w - self.s(8);
                let t = wide(&scene.name);
                let mut lr = RECT { left: 0, top: 0, right: label_w, bottom: 0 };
                let old = SelectObject(dc, self.label_font as HGDIOBJ);
                DrawTextW(dc, t.as_ptr(), -1, &mut lr, DT_CALCRECT | DT_CENTER | DT_WORDBREAK | DT_NOPREFIX);
                SelectObject(dc, old);
                let top = margin;
                let icon_r = RECT {
                    left: x + (tile_w - icon) / 2,
                    top: top + self.s(6),
                    right: x + (tile_w + icon) / 2,
                    bottom: top + self.s(6) + icon,
                };
                let label = RECT {
                    left: x + self.s(4),
                    top: icon_r.bottom + self.s(4),
                    right: x + self.s(4) + label_w,
                    bottom: icon_r.bottom + self.s(4) + (lr.bottom - lr.top),
                };
                let tile = RECT { left: x, top, right: x + tile_w, bottom: label.bottom + self.s(4) };
                // (where it was right-clicked: in the tile, to the right of the icon, its name
                // left to be read)
                let menu = self.lay_menu(dc, &scene.menu, tile.right - self.s(14), icon_r.top + self.s(30));
                let sub = scene.menu.iter().position(|r| !r.sub.is_empty()).map(|i| {
                    let (row_top, _) = menu.rows[i];
                    let sub_x = menu.rect.right - self.s(self.layout.overlap);
                    let sub_y = menu.rect.top + row_top - self.s(self.layout.pad);
                    (i, self.lay_menu(dc, &scene.menu[i].sub, sub_x, sub_y))
                });
                let right = sub.as_ref().map_or(menu.rect.right, |(_, s)| s.rect.right).max(tile.right) + shadow;
                bottom =
                    bottom.max(menu.rect.bottom + shadow).max(sub.as_ref().map_or(0, |(_, s)| s.rect.bottom + shadow));
                placed.push(ScenePlaced { tile, icon: icon_r, label, menu, sub });
                x = right + self.s(24);
            }
            let width = x - self.s(24) + margin - shadow;
            // (the line as high as the longest it may be, so that nothing moves)
            let about_w = width - 2 * margin;
            let old = SelectObject(dc, self.text_font as HGDIOBJ);
            let mut about_h = 0;
            let mut texts: Vec<&str> = vec![&self.idle];
            for scene in &self.scenes {
                for r in &scene.menu {
                    texts.push(&r.about);
                    texts.extend(r.sub.iter().map(|r| r.about.as_str()));
                }
            }
            for t in texts {
                let t = wide(t);
                let mut r = RECT { left: 0, top: 0, right: about_w, bottom: 0 };
                DrawTextW(dc, t.as_ptr(), -1, &mut r, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
                about_h = about_h.max(r.bottom - r.top);
            }
            SelectObject(dc, old);
            ReleaseDC(null_mut(), dc);
            self.placed = placed;
            let canvas_h = bottom;
            let about_y = canvas_h + self.s(4);
            let (button_w, button_h) = (self.s(88), self.s(26));
            let button_y = about_y + about_h + self.s(16);
            let height = button_y + button_h + margin;
            SetWindowPos(self.canvas, null_mut(), 0, 0, width, canvas_h, SWP_NOZORDER | SWP_NOACTIVATE);
            SetWindowPos(self.about, null_mut(), margin, about_y, about_w, about_h, SWP_NOZORDER | SWP_NOACTIVATE);
            SetWindowPos(
                self.ok,
                null_mut(),
                (width - button_w) / 2,
                button_y,
                button_w,
                button_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let mut r = RECT { left: 0, top: 0, right: width, bottom: height };
            match user32(b"AdjustWindowRectExForDpi\0") {
                Some(f) => {
                    std::mem::transmute::<Proc, AdjustWindowRectExForDpi>(f)(&mut r, style, 0, 0, self.dpi);
                }
                None => {
                    AdjustWindowRectEx(&mut r, style, 0, 0);
                }
            }
            SetWindowPos(
                self.main,
                null_mut(),
                0,
                0,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE | 0x2, // SWP_NOMOVE
            );
            InvalidateRect(self.canvas, null(), 0);
        }
    }

    /// The window in the middle of the monitor where the cursor is.
    fn center(&self) {
        // SAFETY: this program's window; structs written to locals
        unsafe {
            let mut p = POINT::default();
            GetCursorPos(&mut p);
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi);
            let mut r = RECT::default();
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(self.main, &mut r);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let wa = mi.rcWork;
            let x = wa.left + ((wa.right - wa.left - w) / 2).max(0);
            let y = wa.top + ((wa.bottom - wa.top - h) / 2).max(0);
            SetWindowPos(self.main, null_mut(), x, y, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | 0x1); // SWP_NOSIZE
        }
    }

    fn row(&self, at: At) -> &Row {
        let r = &self.scenes[at.scene].menu[at.row];
        match at.sub {
            Some(i) => &r.sub[i],
            None => r,
        }
    }

    /// The command shown as chosen: that pointed at, else that of the keyboard.
    fn current(&self) -> Option<At> {
        self.pointed.or(if self.focused { self.chosen } else { None })
    }

    /// The line and the control's text of the command shown.
    fn describe(&self) {
        let (about, name) = match self.current() {
            Some(at) => {
                let r = self.row(at);
                (r.about.clone(), r.text.replace('&', ""))
            }
            None => (self.idle.clone(), String::new()),
        };
        let (about, name) = (wide(&about), wide(&name));
        // SAFETY: this program's windows, texts ending in NUL
        unsafe {
            SetWindowTextW(self.about, about.as_ptr());
            SetWindowTextW(self.canvas, name.as_ptr());
            NotifyWinEvent(0x800C, self.canvas, 0xFFFFFFFC_u32 as i32, 0); // EVENT_OBJECT_NAMECHANGE, OBJID_CLIENT
        }
    }

    /// Every command, in the order of Tab.
    fn order(&self) -> Vec<At> {
        let mut v = Vec::new();
        for (scene, s) in self.scenes.iter().enumerate() {
            for (row, r) in s.menu.iter().enumerate() {
                if r.is_separator() {
                    continue;
                }
                v.push(At { scene, row, sub: None });
                for (i, c) in r.sub.iter().enumerate() {
                    if !c.is_separator() {
                        v.push(At { scene, row, sub: Some(i) });
                    }
                }
            }
        }
        v
    }

    /// The commands of the menu that `at` is in, in order.
    fn siblings(&self, at: At) -> Vec<At> {
        let menu = &self.scenes[at.scene].menu;
        match at.sub {
            None => (0..menu.len())
                .filter(|&row| !menu[row].is_separator())
                .map(|row| At { sub: None, row, ..at })
                .collect(),
            Some(_) => {
                let sub = &menu[at.row].sub;
                (0..sub.len()).filter(|&i| !sub[i].is_separator()).map(|i| At { sub: Some(i), ..at }).collect()
            }
        }
    }

    /// The command of the keyboard's key `key` from `at`; `None` where it leaves the control.
    fn moved(&self, at: At, key: u16) -> Option<At> {
        let order = self.order();
        let here = |list: &[At]| list.iter().position(|&a| a == at).unwrap_or(0);
        let siblings = self.siblings(at);
        let first_of = |scene: usize| self.siblings(At { scene, row: 0, sub: None }).first().copied();
        match key {
            VK_TAB => {
                let i = here(&order);
                // SAFETY: the keyboard's state, read
                if unsafe { GetKeyState(VK_SHIFT as i32) } < 0 {
                    i.checked_sub(1).map(|i| order[i])
                } else {
                    order.get(i + 1).copied()
                }
            }
            VK_DOWN => Some(siblings[(here(&siblings) + 1) % siblings.len()]),
            VK_UP => Some(siblings[(here(&siblings) + siblings.len() - 1) % siblings.len()]),
            VK_HOME => siblings.first().copied(),
            VK_END => siblings.last().copied(),
            VK_RIGHT => match at.sub {
                None if !self.row(at).sub.is_empty() => self.siblings(At { sub: Some(0), ..at }).first().copied(),
                None => first_of((at.scene + 1) % self.scenes.len()),
                Some(_) => Some(at),
            },
            VK_LEFT => match at.sub {
                Some(_) => Some(At { sub: None, ..at }),
                None => first_of((at.scene + self.scenes.len() - 1) % self.scenes.len()),
            },
            _ => Some(at),
        }
    }

    /// The command at `p` of the control.
    fn hit(&self, p: POINT) -> Option<At> {
        let inside = |r: &RECT| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
        for (scene, sp) in self.placed.iter().enumerate().rev() {
            // (the submenu over its menu)
            let menus = sp.sub.iter().map(|(row, m)| (m, Some(*row))).chain([(&sp.menu, None)]);
            for (m, parent) in menus {
                if !inside(&m.rect) {
                    continue;
                }
                for (i, &(top, h)) in m.rows.iter().enumerate() {
                    let at = match parent {
                        Some(row) => At { scene, row, sub: Some(i) },
                        None => At { scene, row: i, sub: None },
                    };
                    if p.y >= m.rect.top + top && p.y < m.rect.top + top + h && !self.row(at).is_separator() {
                        return Some(at);
                    }
                }
                return None;
            }
        }
        None
    }

    fn paint(&self, dc: HDC, client: &RECT) {
        // SAFETY: a DC of the control's (a buffer of it), objects made and freed here
        unsafe {
            FillRect(dc, client, GetSysColorBrush(COLOR_WINDOW));
            SetBkMode(dc, TRANSPARENT as i32);
            let current = self.current();
            for (scene, sp) in self.placed.iter().enumerate() {
                // the file, selected: of the theme, its tile; else its name on the highlight
                let themed = self.list_theme != 0;
                if themed {
                    DrawThemeBackground(self.list_theme, dc, LVP_LISTITEM, LISS_SELECTED, &sp.tile, null());
                }
                let icon = self.file_icons[scene];
                if !icon.is_null() {
                    let (x, y, size) = (sp.icon.left, sp.icon.top, sp.icon.right - sp.icon.left);
                    DrawIconEx(dc, x, y, icon, size, size, 0, null_mut(), DI_NORMAL);
                }
                let old = SelectObject(dc, self.label_font as HGDIOBJ);
                let name = wide(&self.scenes[scene].name);
                let flags = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
                if !themed {
                    let mut fit = sp.label;
                    DrawTextW(dc, name.as_ptr(), -1, &mut fit, flags | DT_CALCRECT);
                    let (fw, mid) = (fit.right - fit.left, (sp.label.left + sp.label.right) / 2);
                    let back = RECT {
                        left: mid - fw / 2 - 2,
                        top: sp.label.top,
                        right: mid + fw - fw / 2 + 2,
                        bottom: sp.label.bottom,
                    };
                    FillRect(dc, &back, GetSysColorBrush(COLOR_HIGHLIGHT));
                }
                SetTextColor(dc, GetSysColor(if themed { COLOR_WINDOWTEXT } else { COLOR_HIGHLIGHTTEXT }));
                let mut lr = sp.label;
                DrawTextW(dc, name.as_ptr(), -1, &mut lr, flags);
                SelectObject(dc, old);
                // the menu, the submenu over it
                let menu = &self.scenes[scene].menu;
                // (the row whose submenu is open is hot, unless another of its menu is chosen)
                let open = sp.sub.as_ref().map(|(row, _)| *row);
                let other = matches!(current, Some(At { scene: s, sub: None, row }) if s == scene && Some(row) != open);
                let hot_row = |row: usize| match current {
                    Some(at) if at.scene == scene && at.sub.is_none() && at.row == row => true,
                    _ => Some(row) == open && !other,
                };
                self.draw_menu(dc, &sp.menu, menu, hot_row);
                if let Some((row, sub)) = &sp.sub {
                    let hot = |i: usize| current == Some(At { scene, row: *row, sub: Some(i) });
                    self.draw_menu(dc, sub, &menu[*row].sub, hot);
                }
            }
        }
    }

    /// Menu `rows`, placed at `m`, onto `dc`: its shadow, then it, through a bitmap of its own
    /// whose alpha makes its corners.
    fn draw_menu(&self, dc: HDC, m: &Placed, rows: &[Row], hot: impl Fn(usize) -> bool) {
        let ly = self.layout;
        let (w, h) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
        let radius = self.s(ly.radius) as f32;
        if self.shadows {
            self.shadow(dc, &m.rect, radius);
        }
        // SAFETY: a bitmap of ours of w × h pixels of 32 bits, drawn into and blended, freed
        unsafe {
            let Some((mem, bits, bmp)) = bitmap(dc, w, h) else { return };
            let old_bmp = SelectObject(mem, bmp as HGDIOBJ);
            let full = RECT { left: 0, top: 0, right: w, bottom: h };
            let theme = self.menu_theme;
            if theme != 0 {
                if !self.win11 {
                    DrawThemeBackground(theme, mem, MENU_POPUPBORDERS, 0, &full, null());
                }
                let inner = if self.win11 { full } else { RECT { left: 1, top: 1, right: w - 1, bottom: h - 1 } };
                DrawThemeBackground(theme, mem, MENU_POPUPBACKGROUND, 0, &inner, null());
                if !self.win11 {
                    let gx = self.s(ly.separator_inset) - self.s(3);
                    let g = RECT { left: self.s(3), top: 1, right: gx, bottom: h - 1 };
                    DrawThemeBackground(theme, mem, MENU_POPUPGUTTER, 0, &g, null());
                }
            } else {
                let mut edge = full;
                FillRect(mem, &full, GetSysColorBrush(COLOR_MENU));
                DrawEdge(mem, &mut edge, EDGE_RAISED, BF_RECT);
            }
            SetBkMode(mem, TRANSPARENT as i32);
            let icon = self.s(16);
            for (i, (r, &(top, rh))) in rows.iter().zip(&m.rows).enumerate() {
                if r.is_separator() {
                    let inset = self.s(ly.separator_inset);
                    let mut size = SIZE { cx: 1, cy: 1 };
                    if theme != 0 {
                        GetThemePartSize(theme, mem, MENU_POPUPSEPARATOR, 0, null(), TS_TRUE, &mut size);
                    }
                    let lh = if self.win11 { self.s(1).max(1) } else { size.cy.max(1) };
                    let y = top + (rh - lh) / 2;
                    let line = RECT { left: inset, top: y, right: w - inset, bottom: y + lh };
                    if theme == 0 {
                        let mut line = RECT { top: top + self.s(3), bottom: top + self.s(3) + 2, ..line };
                        DrawEdge(mem, &mut line, EDGE_ETCHED, BF_TOP);
                    } else if self.win11 {
                        fill(mem, &line, rgb(215, 215, 215));
                    } else {
                        DrawThemeBackground(theme, mem, MENU_POPUPSEPARATOR, 0, &line, null());
                    }
                    continue;
                }
                let is_hot = hot(i);
                let state = match (r.disabled, is_hot) {
                    (false, false) => MPI_NORMAL,
                    (false, true) => MPI_HOT,
                    (true, false) => MPI_DISABLED,
                    (true, true) => MPI_DISABLEDHOT,
                };
                let item = RECT { left: self.s(ly.hot_inset), top, right: w - self.s(ly.hot_inset), bottom: top + rh };
                if is_hot && self.win11 {
                    // (Windows 11's own, not the theme's: gray, its corners rounded)
                    GdiFlush();
                    let px = std::slice::from_raw_parts_mut(bits, (w * h) as usize);
                    round_fill(px, w, &item, self.s(4) as f32, rgb(240, 240, 240));
                } else if is_hot && theme != 0 {
                    DrawThemeBackground(theme, mem, MENU_POPUPITEM, state, &item, null());
                } else if is_hot {
                    FillRect(mem, &item, GetSysColorBrush(COLOR_HIGHLIGHT));
                }
                if r.icon && !self.item_icon.is_null() {
                    let y = top + (rh - icon) / 2;
                    DrawIconEx(mem, self.s(ly.icon_x), y, self.item_icon, icon, icon, 0, null_mut(), DI_NORMAL);
                }
                let font = if r.bold { self.bold_font } else { self.menu_font };
                let old = SelectObject(mem, font as HGDIOBJ);
                let text = wide(&r.text);
                let tr = RECT { left: self.s(ly.text_x), top, right: w - self.s(ly.right) / 2, bottom: top + rh };
                let mut flags = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS;
                if !self.cues {
                    flags |= DT_HIDEPREFIX;
                }
                let color = match (r.disabled, is_hot) {
                    (false, false) => COLOR_MENUTEXT,
                    (false, true) => COLOR_HIGHLIGHTTEXT,
                    (true, _) => COLOR_GRAYTEXT,
                };
                if theme != 0 {
                    DrawThemeText(theme, mem, MENU_POPUPITEM, state, text.as_ptr(), -1, flags, 0, &tr);
                } else {
                    // (disabled, embossed: its highlight a pixel down and right, unless hot)
                    if r.disabled && !is_hot {
                        let mut lit =
                            RECT { left: tr.left + 1, top: tr.top + 1, right: tr.right + 1, bottom: tr.bottom + 1 };
                        SetTextColor(mem, GetSysColor(COLOR_3DHILIGHT));
                        DrawTextW(mem, text.as_ptr(), -1, &mut lit, flags);
                    }
                    let mut tr = tr;
                    SetTextColor(mem, GetSysColor(if r.disabled && !is_hot { COLOR_3DSHADOW } else { color }));
                    DrawTextW(mem, text.as_ptr(), -1, &mut tr, flags);
                }
                SelectObject(mem, old);
                if !r.sub.is_empty() && theme != 0 {
                    let (aw, ah) = (self.s(11), self.s(10));
                    let ax = w - self.s(ly.arrow_right) - aw;
                    let a =
                        RECT { left: ax, top: top + (rh - ah) / 2, right: ax + aw, bottom: top + (rh - ah) / 2 + ah };
                    DrawThemeBackground(theme, mem, MENU_POPUPSUBMENU, MSM_NORMAL, &a, null());
                } else if !r.sub.is_empty() {
                    // (the classic arrow: a triangle of the text's color, 4 wide and 7 high)
                    let (aw, ah) = (self.s(4), self.s(7) | 1);
                    let ax = w - self.s(ly.arrow_right) - aw;
                    let ay = top + (rh - ah) / 2;
                    let c = GetSysColor(color);
                    for i in 0..aw {
                        let half = (ah / 2) * (aw - i) / aw;
                        let col = RECT {
                            left: ax + i,
                            top: ay + ah / 2 - half,
                            right: ax + i + 1,
                            bottom: ay + ah / 2 + half + 1,
                        };
                        fill(mem, &col, c);
                    }
                }
            }
            // the corners and the border, the alpha premultiplied
            GdiFlush();
            let px = std::slice::from_raw_parts_mut(bits, (w * h) as usize);
            let border = ly.border;
            let (br, bg, bb) = ((border & 0xff) as f32, ((border >> 8) & 0xff) as f32, ((border >> 16) & 0xff) as f32);
            let one = self.s(1).max(1) as f32;
            for y in 0..h {
                for x in 0..w {
                    let d = rounded(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, radius);
                    let cover = (0.5 - d).clamp(0.0, 1.0);
                    let inner = (0.5 - (d + one)).clamp(0.0, 1.0);
                    let p = &mut px[(y * w + x) as usize];
                    let (cb, cg, cr) = ((*p & 0xff) as f32, ((*p >> 8) & 0xff) as f32, ((*p >> 16) & 0xff) as f32);
                    // (before Windows 11 the theme's border is the bitmap's own)
                    let (inner, edge) = if self.win11 { (inner, cover - inner) } else { (cover, 0.0) };
                    let b = cb * inner + bb * edge;
                    let g = cg * inner + bg * edge;
                    let r = cr * inner + br * edge;
                    *p = (b as u32) | (g as u32) << 8 | (r as u32) << 16 | ((cover * 255.0) as u32) << 24;
                }
            }
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            AlphaBlend(dc, m.rect.left, m.rect.top, w, h, mem, 0, 0, w, h, blend);
            SelectObject(mem, old_bmp);
            DeleteObject(bmp as HGDIOBJ);
            DeleteDC(mem);
        }
    }

    /// The shadow of a menu at `r` (of corners of `radius`), as DWM draws it: soft, under it more.
    fn shadow(&self, dc: HDC, r: &RECT, radius: f32) {
        let reach = self.s(self.layout.shadow);
        let drop = self.s(2) as f32;
        let (w, h) = (r.right - r.left + 2 * reach, r.bottom - r.top + 2 * reach);
        let alpha = self.layout.shadow_alpha;
        // SAFETY: a bitmap of ours of w × h pixels of 32 bits, written and blended, freed
        unsafe {
            let Some((mem, bits, bmp)) = bitmap(dc, w, h) else { return };
            let old = SelectObject(mem, bmp as HGDIOBJ);
            let px = std::slice::from_raw_parts_mut(bits, (w * h) as usize);
            let (mw, mh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            for y in 0..h {
                for x in 0..w {
                    let fx = (x - reach) as f32 + 0.5;
                    let fy = (y - reach) as f32 + 0.5 - drop;
                    let d = rounded(fx, fy, mw, mh, radius).max(0.0);
                    let t = (1.0 - d / reach as f32).clamp(0.0, 1.0);
                    let a = (alpha * t * t * 255.0) as u32;
                    px[(y * w + x) as usize] = a << 24;
                }
            }
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            AlphaBlend(dc, r.left - reach, r.top - reach, w, h, mem, 0, 0, w, h, blend);
            SelectObject(mem, old);
            DeleteObject(bmp as HGDIOBJ);
            DeleteDC(mem);
        }
    }

    /// The command shown changed: drawn again and told.
    fn changed(&mut self, pointed: Option<At>, chosen: Option<At>) {
        if (pointed, chosen) == (self.pointed, self.chosen) {
            return;
        }
        (self.pointed, self.chosen) = (pointed, chosen);
        self.describe();
        // SAFETY: this program's window
        unsafe { InvalidateRect(self.canvas, null(), 0) };
    }
}

/// The signed distance of (`x`, `y`) to a rectangle of `w` × `h` at the origin whose corners are
/// rounded by `r` (negative inside).
fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (hw, hh) = (w / 2.0, h / 2.0);
    let qx = (x - hw).abs() - (hw - r);
    let qy = (y - hh).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// A DC and a top-down bitmap of `w` × `h` pixels of 32 bits selected into none yet: its bits.
///
/// # Safety
/// The caller frees the DC and the bitmap.
unsafe fn bitmap(dc: HDC, w: i32, h: i32) -> Option<(HDC, *mut u32, HBITMAP)> {
    unsafe {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bmp.is_null() || w <= 0 || h <= 0 {
            return None;
        }
        Some((CreateCompatibleDC(dc), bits as *mut u32, bmp))
    }
}

/// Rectangle `r` of the pixels `px` (rows of `w`), its corners rounded by `radius`, filled
/// with `color` (opaque, the edges blended).
fn round_fill(px: &mut [u32], w: i32, r: &RECT, radius: f32, color: COLORREF) {
    let (rw, rh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    let c = [(color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff].map(|v| v as f32);
    for y in r.top.max(0)..r.bottom {
        for x in r.left.max(0)..r.right.min(w) {
            let d = rounded((x - r.left) as f32 + 0.5, (y - r.top) as f32 + 0.5, rw, rh, radius);
            let a = (0.5 - d).clamp(0.0, 1.0);
            let Some(p) = px.get_mut((y * w + x) as usize) else { continue };
            let old = [(*p >> 16) & 0xff, (*p >> 8) & 0xff, *p & 0xff].map(|v| v as f32);
            let [r, g, b] = [0, 1, 2].map(|i| (old[i] + (c[i] - old[i]) * a).round() as u32);
            *p = r << 16 | g << 8 | b;
        }
    }
}

fn fill(dc: HDC, r: &RECT, color: COLORREF) {
    // SAFETY: a DC; the brush made and freed
    unsafe {
        let b = windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(color);
        FillRect(dc, r, b);
        DeleteObject(b as HGDIOBJ);
    }
}

/// `IID_IImageList`.
const IMAGE_LIST: GUID = GUID::from_u128(0x46eb5926_582e_4017_9fdf_e8998daa0950);

/// The system's icon of files named `name` (by its extension), of `size` pixels (from the image
/// list of 48 pixels, or of 256 where it is bigger).
fn file_icon(name: &str, size: i32) -> HICON {
    let n = wide(name);
    // SAFETY: a name ending in NUL, structs written to locals; the image list the system's
    unsafe {
        let mut info: SHFILEINFOW = std::mem::zeroed();
        let flags = SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES;
        if SHGetFileInfoW(n.as_ptr(), FILE_ATTRIBUTE_NORMAL, &mut info, size_of::<SHFILEINFOW>() as u32, flags) == 0 {
            return null_mut();
        }
        let which = if size <= 48 { 2 } else { 4 }; // SHIL_EXTRALARGE, SHIL_JUMBO
        let mut list = null_mut();
        if SHGetImageList(which, &IMAGE_LIST, &mut list) < 0 || list.is_null() {
            return null_mut();
        }
        ImageList_GetIcon(list as isize, info.iIcon, ILD_TRANSPARENT)
    }
}

unsafe extern "system" fn main_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: the window's messages, as Windows passes them
    unsafe {
        match msg {
            WM_COMMAND if matches!((wp & 0xffff) as i32, IDOK | IDCANCEL) => {
                DestroyWindow(h);
                0
            }
            WM_CLOSE => {
                DestroyWindow(h);
                0
            }
            WM_DESTROY => {
                with_app(|a| a.free());
                PostQuitMessage(0);
                0
            }
            WM_CTLCOLORSTATIC => {
                let dc = wp as HDC;
                SetBkColor(dc, GetSysColor(COLOR_WINDOW));
                SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
            WM_DPICHANGED => {
                let suggested = &*(lp as *const RECT);
                with_app(|a| {
                    a.dpi = ((wp & 0xffff) as u32).max(96);
                    SetWindowPos(
                        h,
                        null_mut(),
                        suggested.left,
                        suggested.top,
                        0,
                        0,
                        SWP_NOZORDER | SWP_NOACTIVATE | 0x1,
                    );
                    a.build();
                });
                0
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn canvas_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let point = || POINT { x: (lp & 0xffff) as i16 as i32, y: ((lp >> 16) & 0xffff) as i16 as i32 };
    // SAFETY: the control's messages, as Windows passes them
    unsafe {
        match msg {
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(h, &mut ps);
                let mut client = RECT::default();
                GetClientRect(h, &mut client);
                let (w, hh) = (client.right, client.bottom);
                let mem = CreateCompatibleDC(dc);
                let bmp = CreateCompatibleBitmap(dc, w.max(1), hh.max(1));
                let old = SelectObject(mem, bmp as HGDIOBJ);
                with_app(|a| a.paint(mem, &client));
                BitBlt(dc, 0, 0, w, hh, mem, 0, 0, SRCCOPY);
                SelectObject(mem, old);
                DeleteObject(bmp as HGDIOBJ);
                DeleteDC(mem);
                EndPaint(h, &ps);
                0
            }
            WM_GETDLGCODE => {
                // (Tab moves within, until it leaves the first or the last command)
                let m = lp as *const MSG;
                let mut code = DLGC_WANTARROWS as LRESULT;
                if !m.is_null() && (*m).message == WM_KEYDOWN && (*m).wParam == VK_TAB as usize {
                    let stays = with_app(|a| a.chosen.and_then(|at| a.moved(at, VK_TAB)).is_some()).unwrap_or(false);
                    if stays {
                        code |= DLGC_WANTTAB as LRESULT;
                    }
                }
                code
            }
            WM_SETFOCUS => {
                with_app(|a| {
                    a.focused = true;
                    let order = a.order();
                    // (by Tab: the first command, or the last by Shift+Tab; else the one it had)
                    let chosen = if GetKeyState(VK_TAB as i32) < 0 {
                        if GetKeyState(VK_SHIFT as i32) < 0 { order.last().copied() } else { order.first().copied() }
                    } else {
                        a.chosen.or(order.first().copied())
                    };
                    a.pointed = None;
                    a.chosen = None;
                    a.changed(None, chosen);
                });
                0
            }
            WM_KILLFOCUS => {
                with_app(|a| {
                    a.focused = false;
                    a.describe();
                    InvalidateRect(h, null(), 0);
                });
                0
            }
            WM_KEYDOWN => {
                let key = wp as u16;
                with_app(|a| {
                    let Some(at) = a.chosen else { return };
                    if let Some(next) = a.moved(at, key) {
                        a.changed(None, Some(next));
                    }
                });
                0
            }
            WM_MOUSEMOVE => {
                let p = point();
                with_app(|a| {
                    if !a.tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: h,
                            dwHoverTime: 0,
                        };
                        a.tracking = TrackMouseEvent(&mut t) != 0;
                    }
                    let pointed = a.hit(p);
                    a.changed(pointed, a.chosen);
                });
                0
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.tracking = false;
                    a.changed(None, a.chosen);
                });
                0
            }
            WM_LBUTTONDOWN => {
                let p = point();
                let hit = with_app(|a| a.hit(p)).flatten();
                if let Some(at) = hit {
                    with_app(|a| a.chosen = Some(at));
                    SetFocus(h);
                    with_app(|a| a.changed(a.pointed, Some(at)));
                }
                0
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}
