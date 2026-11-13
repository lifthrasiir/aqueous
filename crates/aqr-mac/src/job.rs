//! The work: `aqr --ipc` run on the paths in a process of its own, what it tells taken into a
//! [`aqr_ipc::Job`] by threads reading it, which the app looks at on its timer ([`crate::app`]);
//! stopped, and killed where it does not stop.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use aqr_ipc::{Outcome, Point, STOP, Verb, file_name, outcome};

pub struct Job {
    /// What `aqr` tells, and what it is told.
    pub ipc: aqr_ipc::Job,
    /// The process of `aqr` (the name of the file it writes goes by it).
    pub pid: u32,
    child: Mutex<Child>,
    /// When the stop was asked for.
    stopping: Mutex<Option<Instant>>,
}

impl Job {
    /// Runs `aqr` on `paths` (`-P` where `encrypt`), read on threads of their own.
    pub fn start(verb: Verb, paths: Vec<PathBuf>, encrypt: bool) -> Result<Arc<Job>, String> {
        let (cmd, count) = aqr_ipc::command(&aqr_ipc::aqr(), verb, paths, encrypt, "aqr");
        Self::run(verb, cmd, count)
    }

    /// Runs `aqr` to extract version `point` of `archive` as it was, into a folder beside it
    /// ([`aqr_ipc::version_folder`]).
    pub fn start_at(archive: &Path, point: &Point) -> Result<Arc<Job>, String> {
        let into = aqr_ipc::version_folder(archive, point);
        let (cmd, count) = aqr_ipc::command_at(&aqr_ipc::aqr(), archive, point, &into);
        Self::run(Verb::Extract, cmd, count)
    }

    /// Runs `cmd` (`aqr --ipc` of `count` archives), read on threads of their own.
    fn run(verb: Verb, mut cmd: Command, count: usize) -> Result<Arc<Job>, String> {
        let mut child = cmd.spawn().map_err(|e| aqr_l10n::common::not_run(cmd.get_program().display(), e))?;
        let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        let job = Arc::new(Job {
            ipc: aqr_ipc::Job::new(verb, count, child.stdin.take()),
            pid: child.id(),
            child: Mutex::new(child),
            stopping: Mutex::new(None),
        });
        let j = job.clone();
        std::thread::spawn(move || j.ipc.read_out(out));
        let j = job.clone();
        std::thread::spawn(move || j.ipc.read_err(err));
        Ok(job)
    }

    /// Asks `aqr` to stop (its output being written removed); it is killed where it has not
    /// in [`STOP`] ([`Job::done`]). Of any thread.
    pub fn stop(&self) {
        let mut stopping = self.stopping.lock().unwrap();
        if stopping.is_none() {
            *stopping = Some(Instant::now());
            self.ipc.cancel();
        }
    }

    /// What `aqr` ended with, once it has and all it told is taken; killed where it was asked to
    /// stop long ago.
    pub fn done(&self) -> Option<ExitStatus> {
        let mut child = self.child.lock().unwrap();
        if self.stopping.lock().unwrap().is_some_and(|t| t.elapsed() > STOP) {
            _ = child.kill();
        }
        if !self.ipc.state.lock().unwrap().eof {
            return None;
        }
        let st = child.try_wait().ok().flatten()?;
        // (its input closed, as it is gone)
        self.ipc.close();
        Some(st)
    }

    /// The file that Finder shows the archive at work on: the archive being written (under the
    /// name `aqr` writes it, `NAME.PID.part`, renamed once it is written), what is extracted
    /// (the archive until `aqr` has decided that), or the archive being tested.
    pub fn shown(&self) -> Option<PathBuf> {
        let s = self.ipc.state.lock().unwrap();
        let input = s.input()?;
        match (self.ipc.verb, &s.output) {
            (Verb::Compress, Some(o)) => {
                let mut name = o.file_name()?.to_owned();
                name.push(format!(".{}.part", self.pid));
                Some(o.with_file_name(name))
            }
            (Verb::Extract, Some(o)) => Some(o.clone()),
            _ => Some(input.to_path_buf()),
        }
    }
}
