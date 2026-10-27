//! Workers on Linux, locked down by seccomp: of no new privileges, no descriptors but their
//! standard input, output and error, and system calls only for memory, threads, signals and
//! time (no files, sockets or processes). Every thread of the worker is filtered alike.
//!
//! The memory is not limited by the system yet: an rlimit would count the threads' stacks and
//! what Wasmtime reserves, and cgroups are not always delegated.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::Path;

/// A worker.
pub struct Child(std::process::Child);

impl Child {
    pub fn limit_memory(&self, _: u64) -> io::Result<()> {
        Ok(())
    }

    pub fn exit_code(&mut self) -> Option<i64> {
        super::unix::exit_code(|| self.0.try_wait())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<(Child, File, File)> {
    let (c, to, from) = super::unix::spawn(program, args)?;
    Ok((Child(c), to, from))
}

// The filter's instructions (linux/filter.h, linux/seccomp.h).
const LD_W_ABS: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const JEQ_K: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
const JGE_K: u16 = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
const JSET_K: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const RET_K: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

/// Offsets in `struct seccomp_data`: the call, the architecture, the low words of arguments.
const NR: u32 = 0;
const ARCH: u32 = 4;
const fn arg(i: u32) -> u32 {
    16 + 8 * i
}

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;
const fn errno(e: i32) -> u32 {
    libc::SECCOMP_RET_ERRNO | e as u32
}

/// The calls allowed whatever their arguments.
const ALLOWED: &[libc::c_long] = &[
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_readv,
    libc::SYS_writev,
    libc::SYS_close,
    libc::SYS_fstat,
    libc::SYS_mmap,
    libc::SYS_munmap,
    libc::SYS_mprotect,
    libc::SYS_madvise,
    libc::SYS_mremap,
    libc::SYS_brk,
    libc::SYS_futex,
    libc::SYS_sched_yield,
    libc::SYS_sched_getaffinity,
    libc::SYS_set_robust_list,
    libc::SYS_set_tid_address,
    libc::SYS_rseq,
    libc::SYS_membarrier,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_sigaltstack,
    libc::SYS_restart_syscall,
    libc::SYS_gettid,
    libc::SYS_getpid,
    libc::SYS_getrandom,
    libc::SYS_clock_gettime,
    libc::SYS_clock_getres,
    libc::SYS_clock_nanosleep,
    libc::SYS_nanosleep,
    libc::SYS_exit,
    libc::SYS_exit_group,
];

struct Filter(Vec<libc::sock_filter>);

impl Filter {
    fn op(&mut self, code: u16, jt: u8, jf: u8, k: u32) {
        self.0.push(libc::sock_filter { code, jt, jf, k });
    }

    fn load(&mut self, offset: u32) {
        self.op(LD_W_ABS, 0, 0, offset);
    }

    fn ret(&mut self, action: u32) {
        self.op(RET_K, 0, 0, action);
    }

    /// Where the call is `nr`, `then` (which returns), else on after it.
    fn when(&mut self, nr: libc::c_long, then: impl FnOnce(&mut Filter)) {
        let at = self.0.len();
        self.op(JEQ_K, 0, 0, nr as u32);
        then(self);
        self.0[at].jf = u8::try_from(self.0.len() - at - 1).expect("a short block");
    }
}

/// The filter: the calls of [`ALLOWED`], threads (not processes) cloned, signals to this
/// process, its name set and its limits read; everything else fails.
fn filter() -> Filter {
    let mut f = Filter(Vec::new());
    f.load(ARCH);
    f.op(JEQ_K, 1, 0, AUDIT_ARCH);
    f.ret(KILL);
    f.load(NR);
    #[cfg(target_arch = "x86_64")]
    {
        // no x32 calls
        f.op(JGE_K, 0, 1, 0x4000_0000);
        f.ret(KILL);
    }
    for &nr in ALLOWED {
        f.op(JEQ_K, 0, 1, nr as u32);
        f.ret(ALLOW);
    }
    f.when(libc::SYS_clone, |f| {
        f.load(arg(0));
        f.op(JSET_K, 0, 1, libc::CLONE_THREAD as u32);
        f.ret(ALLOW);
        f.ret(errno(libc::EPERM));
    });
    // clone3's flags are behind a pointer: the C library falls back to clone
    f.when(libc::SYS_clone3, |f| f.ret(errno(libc::ENOSYS)));
    let pid = std::process::id();
    f.when(libc::SYS_tgkill, |f| {
        f.load(arg(0));
        f.op(JEQ_K, 0, 1, pid);
        f.ret(ALLOW);
        f.ret(errno(libc::EPERM));
    });
    f.when(libc::SYS_prctl, |f| {
        f.load(arg(0));
        f.op(JEQ_K, 0, 1, libc::PR_SET_NAME as u32);
        f.ret(ALLOW);
        f.ret(errno(libc::EPERM));
    });
    f.when(libc::SYS_prlimit64, |f| {
        // the limits read, none set
        f.load(arg(2));
        f.op(JEQ_K, 0, 3, 0);
        f.load(arg(2) + 4);
        f.op(JEQ_K, 0, 1, 0);
        f.ret(ALLOW);
        f.ret(errno(libc::EPERM));
    });
    f.ret(errno(libc::EPERM));
    f
}

fn os_check(r: libc::c_long, what: &str) -> io::Result<()> {
    if r >= 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    Err(io::Error::new(e.kind(), format!("{what}: {e}")))
}

pub fn lockdown() -> io::Result<()> {
    // SAFETY: system calls of no pointers but the filter's, which outlives the call
    unsafe {
        os_check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as libc::c_long, "no new privileges")?;
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
            for fd in 3..1024 {
                libc::close(fd);
            }
        }
        let f = filter();
        let prog = libc::sock_fprog { len: f.0.len() as u16, filter: f.0.as_ptr() as *mut libc::sock_filter };
        let r = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &prog as *const libc::sock_fprog,
        );
        if r > 0 {
            return Err(io::Error::other(format!("thread {r} not filtered")));
        }
        os_check(r, "the filter")
    }
}
