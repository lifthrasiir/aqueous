//! The program the settings run once the files are written ([`aqr_sfx::config::Run`]): a file
//! of the archive, run by itself (`CreateProcessW`, no shell), its arguments as they are given.

use std::path::{Path, PathBuf};

use aqr_sfx::config::{DEST, Run, Window};

/// Runs `r`'s program of the files written under `dest`; its exit code where it is waited for.
pub fn run(dest: &Path, r: &Run) -> Result<Option<i32>, String> {
    let under = |p: &str| -> PathBuf {
        let mut d = dest.to_path_buf();
        p.split('/').filter(|s| !s.is_empty() && *s != ".").for_each(|s| d.push(aqr_sfx::sanitized(s.as_bytes())));
        d
    };
    let (program, cwd) = (under(&r.program[0]), under(&r.cwd));
    let at = dest.display().to_string();
    let args: Vec<String> = r.program[1..].iter().map(|a| a.replace(DEST, &at)).collect();
    let env: Vec<(String, String)> = r.env.iter().map(|(n, v)| (n.clone(), v.replace(DEST, &at))).collect();
    spawn(&program, &args, &env, &cwd, r.window, r.wait).map_err(|e| format!("{}: {e}", program.display()))
}

/// The environment block of this process's variables and `env` (in place of those of the same
/// names, which Windows tells apart by case alone): `NAME=value` each, sorted by name, each
/// ended by a NUL and the block by another.
#[cfg(any(windows, test))]
fn environment(own: impl Iterator<Item = (String, String)>, env: &[(String, String)]) -> Vec<u16> {
    let mut all: std::collections::BTreeMap<String, (String, String)> = std::collections::BTreeMap::new();
    for (n, v) in own.chain(env.iter().cloned()) {
        all.insert(n.to_uppercase(), (n, v));
    }
    let mut block: Vec<u16> = Vec::new();
    for (n, v) in all.into_values() {
        block.extend(format!("{n}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    block
}

#[cfg(windows)]
fn spawn(
    program: &Path,
    args: &[String],
    env: &[(String, String)],
    cwd: &Path,
    window: Window,
    wait: bool,
) -> Result<Option<i32>, String> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let mut line = quote(&program.to_string_lossy());
    for a in args {
        line.push(' ');
        line.push_str(&quote(a));
    }
    let (app, dir) = (wide(program.as_os_str()), wide(cwd.as_os_str()));
    let mut line: Vec<u16> = line.encode_utf16().chain([0]).collect();
    let mut si = sys::StartupInfo { cb: std::mem::size_of::<sys::StartupInfo>() as u32, ..Default::default() };
    let mut flags = sys::CREATE_UNICODE_ENVIRONMENT;
    match window {
        Window::Normal => {}
        Window::Hidden => {
            si.flags = sys::STARTF_USESHOWWINDOW;
            si.show_window = sys::SW_HIDE;
            flags |= sys::CREATE_NO_WINDOW;
        }
        Window::Minimized => {
            si.flags = sys::STARTF_USESHOWWINDOW;
            si.show_window = sys::SW_SHOWMINNOACTIVE;
        }
    }
    // (this process's own where nothing is set)
    let mut block = (!env.is_empty()).then(|| {
        let own =
            std::env::vars_os().map(|(n, v)| (n.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()));
        environment(own, env)
    });
    let block_ptr = block.as_mut().map_or(std::ptr::null_mut(), |b| b.as_mut_ptr().cast());
    let mut pi = sys::ProcessInformation::default();
    // SAFETY: NUL-terminated strings, the command line writable, the environment a block of
    // UTF-16 (CREATE_UNICODE_ENVIRONMENT); the handles closed below
    let ok = unsafe {
        sys::CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            flags,
            block_ptr,
            dir.as_ptr(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut code = None;
    // SAFETY: the handles CreateProcessW gave
    unsafe {
        sys::CloseHandle(pi.thread);
        if wait {
            sys::WaitForSingleObject(pi.process, u32::MAX);
            let mut c = 0u32;
            if sys::GetExitCodeProcess(pi.process, &mut c) != 0 {
                code = Some(c as i32);
            }
        }
        sys::CloseHandle(pi.process);
    }
    Ok(code)
}

/// Argument `a` as the C runtime's command line parsing takes it back (in quotes where it has a
/// space, a tab or a quote, or is empty; the backslashes before a quote doubled).
#[cfg(any(windows, test))]
pub(crate) fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '\n', '\x0b', '"']) {
        return a.into();
    }
    let mut q = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            // (the backslashes before it doubled, those written already, and one for it)
            '"' => {
                q.extend(std::iter::repeat_n('\\', slashes + 1));
                slashes = 0;
            }
            _ => slashes = 0,
        }
        q.push(c);
    }
    // (those before the closing quote doubled)
    q.extend(std::iter::repeat_n('\\', slashes));
    q.push('"');
    q
}

#[cfg(not(windows))]
fn spawn(
    program: &Path,
    args: &[String],
    env: &[(String, String)],
    cwd: &Path,
    _: Window,
    wait: bool,
) -> Result<Option<i32>, String> {
    let mut c = std::process::Command::new(program);
    c.args(args).envs(env.iter().map(|(n, v)| (n, v))).current_dir(cwd);
    match wait {
        true => c.status().map(|s| s.code()).map_err(|e| e.to_string()),
        false => c.spawn().map(|_| None).map_err(|e| e.to_string()),
    }
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;

    pub const CREATE_UNICODE_ENVIRONMENT: u32 = 0x400;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const STARTF_USESHOWWINDOW: u32 = 1;
    pub const SW_HIDE: u16 = 0;
    pub const SW_SHOWMINNOACTIVE: u16 = 7;

    #[repr(C)]
    pub struct StartupInfo {
        pub cb: u32,
        pub reserved: *mut u16,
        pub desktop: *mut u16,
        pub title: *mut u16,
        pub x: u32,
        pub y: u32,
        pub x_size: u32,
        pub y_size: u32,
        pub x_chars: u32,
        pub y_chars: u32,
        pub fill: u32,
        pub flags: u32,
        pub show_window: u16,
        pub reserved2_len: u16,
        pub reserved2: *mut u8,
        pub stdin: *mut c_void,
        pub stdout: *mut c_void,
        pub stderr: *mut c_void,
    }

    impl Default for StartupInfo {
        fn default() -> StartupInfo {
            // SAFETY: integers and pointers, all of them null
            unsafe { std::mem::zeroed() }
        }
    }

    #[repr(C)]
    pub struct ProcessInformation {
        pub process: *mut c_void,
        pub thread: *mut c_void,
        pub process_id: u32,
        pub thread_id: u32,
    }

    impl Default for ProcessInformation {
        fn default() -> ProcessInformation {
            // SAFETY: integers and pointers
            unsafe { std::mem::zeroed() }
        }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[allow(clippy::too_many_arguments)]
        pub fn CreateProcessW(
            app: *const u16,
            line: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit: i32,
            flags: u32,
            env: *mut c_void,
            dir: *const u16,
            si: *const StartupInfo,
            pi: *mut ProcessInformation,
        ) -> i32;
        pub fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
        pub fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        pub fn CloseHandle(h: *mut c_void) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::{environment, quote};

    #[test]
    fn environment_block() {
        let own = [("Path", "C:\\x"), ("TEMP", "t")].map(|(n, v)| (n.to_string(), v.to_string()));
        let b = environment(own.into_iter(), &[("PATH".into(), "y".into()), ("NEW".into(), "1".into())]);
        let text = String::from_utf16(&b).unwrap();
        assert_eq!(text, "NEW=1\0PATH=y\0TEMP=t\0\0");
    }

    #[test]
    fn arguments_quoted() {
        for (a, q) in [
            ("plain", "plain"),
            ("", "\"\""),
            ("two words", "\"two words\""),
            ("say \"hi\"", r#""say \"hi\"""#),
            (r"C:\dir\", r"C:\dir\"),
            (r"C:\a dir\", r#""C:\a dir\\""#),
            (r#"a\"b"#, r#""a\\\"b""#),
        ] {
            assert_eq!(quote(a), q, "{a}");
        }
    }
}
