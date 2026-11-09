//! Passphrases: the one of `-P`, asked for as the work begins (twice, compressing), and those
//! asked for as an archive needs one, decompressing: on the terminal, its echo off, or of the
//! frontend ([`crate::ipc`], the question `passphrase`). Without a terminal, `-P` takes a line
//! of standard input; `--passphrase-from` takes it from where it tells instead ([`Source`]).
//! Any passphrase is taken, an empty one too (as PDF's), and those that
//! open an archive are tried first on the archives after it.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;

static KEYS: std::sync::OnceLock<Keys> = std::sync::OnceLock::new();

/// The passphrases of this run made ([`Keys::new`]); once, after the frontend's hello. `from`:
/// that of `-P` taken from there rather than asked for.
pub fn init(given: bool, compress: bool, from: Option<&Source>) -> Result<(), String> {
    let k = match from {
        Some(f) => Keys::of(f.read()?),
        None => Keys::new(given, compress)?,
    };
    KEYS.set(k).map_err(|_| "passphrases asked for twice".to_string())
}

/// The passphrases of this run ([`init`]).
pub fn keys() -> &'static Keys {
    KEYS.get().expect("pass::init")
}

/// Times a passphrase is asked for on the terminal before an archive is given up.
const TRIES: usize = 3;

pub struct Keys {
    /// The passphrases that opened an archive (`-P`'s first, where it was given).
    known: Mutex<Vec<Vec<u8>>>,
    /// Whether a passphrase may be asked for as an archive needs one (a terminal or a
    /// frontend), and whether `-P` was given.
    asks: bool,
    given: bool,
}

/// Where a passphrase is asked for.
#[derive(Clone, Copy, PartialEq)]
enum Asker {
    Terminal,
    Frontend,
    /// A line of standard input, once.
    Input,
    None,
}

fn asker() -> Asker {
    if crate::ipc::get().is_some_and(|i| i.answers(aqr_ipc::ask::PASSPHRASE)) {
        Asker::Frontend
    } else if crate::ipc::get().is_none() && std::io::stdin().is_terminal() {
        Asker::Terminal
    } else if crate::ipc::get().is_none() {
        Asker::Input
    } else {
        Asker::None
    }
}

/// Where `--passphrase-from` takes the passphrase: a line of standard input (`stdin`), of a
/// file descriptor inherited (`fd:N`), of a file (any other path); or an environment
/// variable's value whole (`env:NAME`).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Stdin,
    Fd(i32),
    Env(std::ffi::OsString),
    File(std::path::PathBuf),
}

impl Source {
    pub fn parse(v: &std::ffi::OsStr) -> Result<Source, String> {
        let b = v.as_encoded_bytes();
        Ok(if b == b"stdin" {
            Source::Stdin
        } else if let Some(n) = b.strip_prefix(b"fd:") {
            let n = std::str::from_utf8(n).ok().and_then(|n| n.parse().ok()).filter(|&n: &i32| n >= 0);
            Source::Fd(n.ok_or("--passphrase-from fd:N takes a file descriptor's number")?)
        } else if let Some(n) = b.strip_prefix(b"env:") {
            // SAFETY: split after ASCII, of an `OsStr`'s bytes
            Source::Env(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(n) }.to_owned())
        } else {
            Source::File(v.into())
        })
    }

    /// The passphrase it gives: a line, its end (`\n`, `\r\n`) left out; or the variable's value.
    pub fn read(&self) -> Result<Vec<u8>, String> {
        let line = |r: &mut dyn std::io::Read, what: &str| -> Result<Vec<u8>, String> {
            // (a byte at a time: nothing read past the line, of a pipe kept open)
            let mut out = Vec::new();
            let mut b = [0u8];
            loop {
                match r.read(&mut b) {
                    Ok(0) if out.is_empty() => return Err(format!("{what}: no passphrase (ended)")),
                    Ok(0) => break,
                    Ok(_) if b[0] == b'\n' => break,
                    Ok(_) => out.push(b[0]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(format!("{what}: {e}")),
                }
            }
            if out.last() == Some(&b'\r') {
                out.pop();
            }
            Ok(out)
        };
        match self {
            Source::Stdin => line(&mut std::io::stdin().lock(), "standard input"),
            #[cfg(unix)]
            Source::Fd(n) => {
                use std::os::fd::FromRawFd;
                // SAFETY: the descriptor read, not closed (it is not this process's to close)
                let mut f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(*n) });
                line(&mut *f, &format!("fd:{n}"))
            }
            #[cfg(not(unix))]
            Source::Fd(_) => Err("--passphrase-from fd:N: not on this system".into()),
            Source::Env(n) => match std::env::var_os(n) {
                Some(v) => Ok(v.into_encoded_bytes()),
                None => Err(format!("--passphrase-from env:{}: not set", n.display())),
            },
            Source::File(p) => {
                let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let mut r = std::io::BufReader::new(&mut f);
                line(&mut r, &p.display().to_string())
            }
        }
    }
}

impl Keys {
    /// The passphrases of a run of `-P`'s given (`--passphrase-from`): none asked for.
    fn of(p: Vec<u8>) -> Keys {
        Keys { known: Mutex::new(vec![p]), asks: false, given: true }
    }

    /// The passphrases of a run, `-P` given or not: asked for now where given (compressing,
    /// twice on a terminal and for the frontend to confirm).
    pub fn new(given: bool, compress: bool) -> Result<Keys, String> {
        let asker = asker();
        let mut known = Vec::new();
        if given {
            let mode = if compress { "compress" } else { "decompress" };
            match ask(asker, None, mode, false, compress)? {
                Some(p) => known.push(p),
                None => return Err("no passphrase given".into()),
            }
        }
        let asks = matches!(asker, Asker::Terminal | Asker::Frontend);
        Ok(Keys { known: Mutex::new(known), asks, given })
    }

    /// Why no passphrase opened an archive, as the failure tells it.
    pub fn none_opens(&self) -> &'static str {
        match (self.given || !self.known.lock().unwrap().is_empty(), self.asks) {
            (_, true) => "encrypted, and no passphrase given opens it",
            (true, false) => "encrypted, and the passphrase given does not open it",
            (false, false) => "encrypted (-P to give its passphrase)",
        }
    }

    /// The passphrase that archives are encrypted with (`-P`'s).
    pub fn sealing(&self) -> Option<Vec<u8>> {
        self.given.then(|| self.known.lock().unwrap()[0].clone())
    }

    /// What `open` gives of the first passphrase it takes (`Ok(None)`: not that one) for
    /// archive `path`: those that opened archives before, then asked for (again where wrong)
    /// while there is a terminal or a frontend to ask. `Ok(None)` where none is given or
    /// taken.
    pub fn open<T>(
        &self,
        path: &Path,
        mut open: impl FnMut(&[u8]) -> Result<Option<T>, String>,
    ) -> Result<Option<T>, String> {
        let known = self.known.lock().unwrap().clone();
        for p in &known {
            if let Some(t) = open(p)? {
                return Ok(Some(t));
            }
        }
        if !self.asks {
            return Ok(None);
        }
        let asker = asker();
        let mut again = !known.is_empty();
        for i in 0.. {
            if asker == Asker::Terminal && i == TRIES {
                break;
            }
            let Some(p) = ask(asker, Some(path), "decompress", again, false)? else { break };
            if let Some(t) = open(&p)? {
                self.known.lock().unwrap().push(p);
                return Ok(Some(t));
            }
            if asker == Asker::Terminal {
                eprintln!("{}: a wrong passphrase", path.display());
            }
            again = true;
        }
        Ok(None)
    }
}

/// A passphrase asked for by `asker` (of archive `path`, where there is one), for `mode`;
/// `again` where one was wrong; to be typed twice where `confirm`. `None` where none is given
/// (the frontend cancels, standard input ends).
fn ask(asker: Asker, path: Option<&Path>, mode: &str, again: bool, confirm: bool) -> Result<Option<Vec<u8>>, String> {
    match asker {
        Asker::Frontend => {
            let q = json!({
                "path": path.map(|p| p.display().to_string()),
                "mode": mode,
                "again": again,
                "confirm": confirm,
            });
            let ipc = crate::ipc::get().expect("a frontend");
            Ok(ipc.ask(aqr_ipc::ask::PASSPHRASE, q).and_then(|v| v.as_str().map(|s| s.as_bytes().to_vec())))
        }
        Asker::Terminal => {
            let of = path.map_or(String::new(), |p| format!(" for {}", p.display()));
            for _ in 0..TRIES {
                let Some(p) = terminal::read(&format!("Passphrase{of}: "))? else { return Ok(None) };
                if !confirm {
                    return Ok(Some(p));
                }
                let Some(q) = terminal::read("Passphrase again: ")? else { return Ok(None) };
                if p == q {
                    return Ok(Some(p));
                }
                eprintln!("the passphrases differ");
            }
            Err("the passphrases differ".into())
        }
        Asker::Input => {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => Ok(None),
                Ok(_) => {
                    let line = line.strip_suffix('\n').unwrap_or(&line);
                    Ok(Some(line.strip_suffix('\r').unwrap_or(line).as_bytes().to_vec()))
                }
                Err(e) => Err(format!("passphrase: {e}")),
            }
        }
        Asker::None => Ok(None),
    }
}

/// A line read from the terminal, nothing shown of what is typed.
mod terminal {
    /// The line typed after `prompt` (on standard error), `None` where it is cancelled
    /// (Ctrl-C, or Ctrl-D on nothing typed).
    #[cfg(unix)]
    pub fn read(prompt: &str) -> Result<Option<Vec<u8>>, String> {
        use std::io::{Read, Write};
        let err = |e: std::io::Error| format!("passphrase: {e}");
        eprint!("{prompt}");
        _ = std::io::stderr().flush();
        let fd = libc::STDIN_FILENO;
        // SAFETY: termios filled by tcgetattr
        let mut old = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        // byte by byte, neither echoed nor taken as signals (Ctrl-C cancels here, the terminal
        // given back as it was)
        let mut raw = old;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: a termios of the terminal's own
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        struct Restore(libc::termios);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: the termios it had
                unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.0) };
                eprintln!();
            }
        }
        let _restore = Restore(old);
        let mut line = Vec::new();
        let mut stdin = std::io::stdin().lock();
        let mut b = [0u8];
        loop {
            if stdin.read(&mut b).map_err(err)? == 0 {
                return Ok(None);
            }
            match b[0] {
                b'\r' | b'\n' => return Ok(Some(line)),
                // Ctrl-C, and Ctrl-D on nothing typed
                0x03 => return Ok(None),
                0x04 if line.is_empty() => return Ok(None),
                // backspace: the last character (of UTF-8) gone
                0x7f | 0x08 => {
                    while let Some(c) = line.pop() {
                        if c & 0xc0 != 0x80 {
                            break;
                        }
                    }
                }
                // Ctrl-U: all of it
                0x15 => line.clear(),
                c if c < 0x20 => {}
                c => line.push(c),
            }
        }
    }

    #[cfg(windows)]
    pub fn read(prompt: &str) -> Result<Option<Vec<u8>>, String> {
        use std::io::Write;
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };
        let err = |e: std::io::Error| format!("passphrase: {e}");
        eprint!("{prompt}");
        _ = std::io::stderr().flush();
        // SAFETY: the console's input handle, and its mode read and set back
        let h = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(h, &mut mode) } == 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        struct Restore(windows_sys::Win32::Foundation::HANDLE, u32);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: the mode it had
                unsafe { SetConsoleMode(self.0, self.1) };
                eprintln!();
            }
        }
        // SAFETY: as above
        if unsafe { SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT) } == 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        let _restore = Restore(h, mode);
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).map_err(err)? == 0 {
            return Ok(None);
        }
        let line = line.strip_suffix('\n').unwrap_or(&line);
        Ok(Some(line.strip_suffix('\r').unwrap_or(line).as_bytes().to_vec()))
    }

    #[cfg(not(any(unix, windows)))]
    pub fn read(_: &str) -> Result<Option<Vec<u8>>, String> {
        Err("no terminal to ask for a passphrase".into())
    }
}
