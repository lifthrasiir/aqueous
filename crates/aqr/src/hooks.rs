//! `--run-before CMD`, `--run-after CMD`: commands run by the system's shell (`cmd /c` on
//! Windows, `sh -c` elsewhere) before and after each archive is written (`-c`, its other formats
//! and self-extracting programs, `-a`, and the archive edited in place: `--delete`, `--move`,
//! `--compact`), each as many times as given, in order. What they run on is in their
//! environment, not their text (so that no path is quoted for a shell):
//!
//! - `AQR_OUTPUT`: the archive's path (absolute)
//! - `AQR_HOOK`: `before` or `after`
//!
//! Before: once whether it is to be written over is decided (not of an output skipped), before
//! anything is written. After: once it is written whole, as nothing else of aqr will touch it (a
//! signature added to a self-extracting program, say). A command failing (not exiting 0) fails
//! aqr, the archive left as it is. An Aqueous archive (or a self-extracting program) is located
//! again after: one a command left aqr unable to find in its file fails.

use std::path::Path;
use std::process::Command;

/// When a hook runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    Before,
    After,
}

impl When {
    fn name(self) -> &'static str {
        match self {
            When::Before => "before",
            When::After => "after",
        }
    }

    fn option(self) -> &'static str {
        match self {
            When::Before => "--run-before",
            When::After => "--run-after",
        }
    }
}

/// Runs the hooks of `when` (of `a`) on archive `out`, in order.
pub(crate) fn run(a: &crate::Args, when: When, out: &Path) -> Result<(), String> {
    let cmds = match when {
        When::Before => &a.run_before,
        When::After => &a.run_after,
    };
    if cmds.is_empty() {
        return Ok(());
    }
    let path = std::path::absolute(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for cmd in cmds {
        let status = shell(cmd)
            .env("AQR_OUTPUT", &path)
            .env("AQR_HOOK", when.name())
            .status()
            .map_err(|e| format!("{} {cmd:?}: not run ({e})", when.option()))?;
        if !status.success() {
            let how = status.code().map_or_else(|| "was stopped".to_string(), |c| format!("exited with {c}"));
            return Err(format!("{} {cmd:?} {how} ({})", when.option(), out.display()));
        }
    }
    Ok(())
}

/// Checks that Aqueous archive `out` is still found in its file after the hooks run on it.
pub(crate) fn still_there(a: &crate::Args, out: &Path) -> Result<(), String> {
    if a.run_after.is_empty() {
        return Ok(());
    }
    let c = std::fs::read(out).map_err(|e| format!("{} after --run-after: {e}", out.display()))?;
    match aqr_archive::locate(&c) {
        Some(_) => Ok(()),
        None => Err(format!("{}: no archive found in it after --run-after (the command changed it so)", out.display())),
    }
}

/// `cmd` run by the system's shell.
#[cfg(windows)]
fn shell(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut c = Command::new(comspec);
    // (`/s /c "..."`: the command as given, the outer quotes taken off, as cmd takes it)
    c.raw_arg(format!("/d /s /c \"{cmd}\""));
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]);
    c
}
