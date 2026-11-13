//! The work: `aqr --ipc` run on the paths, what it tells taken into a [`Job`] (aqr-ipc) that
//! the window shows ([`crate::dialog::progress`]), and what came of it shown at the end.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aqr_ipc::{Job, Outcome, Point, Verb, outcome};

use crate::dialog::{self, Icon};
use crate::options;

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

/// Runs `aqr --session` beside the compression dialog on `paths` ([`options::open`]): it finds
/// out what they are as the options are chosen, and once the user presses Compress it makes the
/// archive, the window of how far it is shown and what came of it.
pub fn run_options(mut paths: Vec<PathBuf>) -> ExitCode {
    paths.sort();
    let cmd = aqr_ipc::session_command(&aqr_ipc::aqr(), paths[0].parent());
    let (mut child, job) = match spawn(Verb::Compress, cmd, 1) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let job = std::sync::Arc::new(job);
    let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let status = std::thread::scope(|s| {
        let reading = &job;
        s.spawn(move || reading.read_out(out));
        s.spawn(move || reading.read_err(err));
        let status = match options::open(Some(job.clone()), paths) {
            Some(args) => {
                job.begin(&args);
                dialog::progress(&job, &mut child)
            }
            // (given up: nothing to wait for)
            None => {
                job.cancel();
                None
            }
        };
        job.close();
        _ = child.wait();
        status
    });
    let Ok(job) = std::sync::Arc::try_unwrap(job) else { return ExitCode::FAILURE };
    conclude(Verb::Compress, job, status, 1)
}

/// Starts `cmd` (of `count` archives): the process and its job, or the failure shown.
fn spawn(verb: Verb, mut cmd: std::process::Command, count: usize) -> Result<(std::process::Child, Job), ExitCode> {
    /// `CREATE_NO_WINDOW`: no console for it.
    const NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(NO_WINDOW);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let what = aqr_l10n::common::not_run(cmd.get_program().to_string_lossy(), e);
            dialog::message(Icon::Error, &verb.failed(""), &what, None);
            return Err(ExitCode::FAILURE);
        }
    };
    let job = Job::new(verb, count, child.stdin.take());
    Ok((child, job))
}

/// Runs `cmd` (of `count` archives), the window of how far it is shown and what came of it.
fn drive(verb: Verb, cmd: std::process::Command, count: usize) -> ExitCode {
    let (mut child, job) = match spawn(verb, cmd, count) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let status = std::thread::scope(|s| {
        let job = &job;
        s.spawn(move || job.read_out(out));
        s.spawn(move || job.read_err(err));
        let status = dialog::progress(job, &mut child);
        job.close();
        status
    });
    conclude(verb, job, status, count)
}

/// What came of `job` (of `count` archives), which ended with `status`: shown where it failed
/// (or, testing, what was found).
fn conclude(verb: Verb, job: Job, status: Option<std::process::ExitStatus>, count: usize) -> ExitCode {
    let s = job.state.into_inner().unwrap();
    let name = s.name();
    match outcome(verb, count, &s, status, &name) {
        Outcome::Failed { main, text, log } => {
            dialog::message(Icon::Error, &main, &text, log.as_deref());
            ExitCode::FAILURE
        }
        Outcome::Tested { main, text } => {
            dialog::message(Icon::Information, &main, &text, None);
            ExitCode::SUCCESS
        }
        Outcome::Silent => ExitCode::FAILURE,
        Outcome::Done => ExitCode::SUCCESS,
    }
}
