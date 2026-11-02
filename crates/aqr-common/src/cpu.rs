//! The CPU time of the calling thread, for the tools' timings (wall time is skewed by other load).

/// The CPU time of the calling thread so far, in seconds.
///
/// On Windows this is `GetThreadTimes`' (kernel and user), which advances by the scheduler's
/// tick (about 15.6 ms): good for sums over many samples, not for timing one short run.
/// Elsewhere (neither Unix nor Windows), the wall time since the first call.
pub fn thread() -> f64 {
    imp::thread()
}

#[cfg(unix)]
mod imp {
    pub fn thread() -> f64 {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
        t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        /// In seconds, from units of 100 ns.
        fn seconds(&self) -> f64 {
            ((self.high as u64) << 32 | self.low as u64) as f64 * 1e-7
        }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn GetThreadTimes(
            thread: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    pub fn thread() -> f64 {
        let [mut creation, mut exit, mut kernel, mut user] = Default::default();
        unsafe { GetThreadTimes(GetCurrentThread(), &mut creation, &mut exit, &mut kernel, &mut user) };
        kernel.seconds() + user.seconds()
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn thread() -> f64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn advances_with_work() {
        let t0 = super::thread();
        let mut x = 1u64;
        while super::thread() - t0 < 0.05 {
            for i in 0..1_000_000u64 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
            }
        }
        assert!(super::thread() >= t0 + 0.05);
    }
}
