//! The work: `aqr --ipc` run on the paths, what it tells taken into a [`Job`] (aqr-ipc) that
//! the window shows ([`crate::dialog::progress`]), and what came of it shown at the end.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aqr_ipc::{Job, Point, Verb, failure, intact, tested};

use crate::dialog::{self, Icon};

/// Runs `aqr` on `paths` (`-P` where `encrypt`; compressing, into archives of `format`, `aqr`
/// or another that `aqr --format` takes), and shows how far it is.
pub fn run(verb: Verb, paths: Vec<PathBuf>, encrypt: bool, format: &str) -> ExitCode {
    let (cmd, count) = aqr_ipc::command(&aqr_ipc::aqr(), verb, paths, encrypt, format);
    drive(verb, cmd, count)
}

/// Runs `aqr` extracting `archive` as it was at `point`, into a folder beside it named after the
/// version ([`aqr_ipc::version_folder`]), and shows how far it is.
pub fn run_at(archive: &Path, point: &Point) -> ExitCode {
    let into = aqr_ipc::version_folder(archive, point);
    let (cmd, count) = aqr_ipc::command_at(&aqr_ipc::aqr(), archive, point, &into);
    drive(Verb::Extract, cmd, count)
}

/// Runs `cmd` (of `count` archives), the window of how far it is shown and what came of it.
fn drive(verb: Verb, mut cmd: std::process::Command, count: usize) -> ExitCode {
    /// `CREATE_NO_WINDOW`: no console for it.
    const NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(NO_WINDOW);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let what = aqr_l10n::common::not_run(cmd.get_program().to_string_lossy(), e);
            dialog::message(Icon::Error, &verb.failed(""), &what, None);
            return ExitCode::FAILURE;
        }
    };
    let job = Job::new(verb, count, child.stdin.take());
    let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let status = std::thread::scope(|s| {
        let job = &job;
        s.spawn(move || job.read_out(out));
        s.spawn(move || job.read_err(err));
        let status = dialog::progress(job, &mut child);
        job.close();
        status
    });
    let s = job.state.into_inner().unwrap();
    if s.stopped || s.cancelled {
        return ExitCode::FAILURE;
    }
    let log: Vec<&str> = s.log.iter().map(String::as_str).collect();
    let log = log.join("\n");
    let name = s.name();
    if let Some(e) = failure(&s, status) {
        dialog::message(Icon::Error, &verb.failed(&name), &e, (!log.is_empty()).then_some(&log));
        return ExitCode::FAILURE;
    }
    if verb == Verb::Test {
        let lines: Vec<String> = s.ended.iter().map(|(n, e)| tested(n, e, count > 1)).collect();
        dialog::message(Icon::Information, &intact(&name, count), &lines.join("\n"), None);
    }
    ExitCode::SUCCESS
}
