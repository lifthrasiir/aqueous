//! The exit code of a run that fails: by what failed, as the spec's failures tell it apart
//! ([`Failure`]), noted where it is found ([`seen`], [`note`]) and taken as the run ends
//! ([`code`]).
//!
//! | code | |
//! |---|---|
//! | 0 | done |
//! | 1 | failed otherwise: reading or writing files, an archive of another format, ... |
//! | 2 | the arguments wrong |
//! | 3 | an archive invalid: damaged, cut short, files lost to it (repaired or not) |
//! | 4 | an archive not decrypted: no passphrase given opens it |
//! | 5 | an archive needing more than this decoder supports or is let to take (`-M`, `--application-bound`) |
//! | 6 | an archive of a critical extension not known |
//! | 7 | done but for what is told left out: entries not extracted, inputs not removed (`--rm`) |
//! | 130 | stopped (Ctrl-C, a signal, the frontend) |
//!
//! Where several are noted, the most severe (by the order 7, 3, 4, 5, 6) is the run's.

use std::sync::atomic::{AtomicU8, Ordering};

use aqr_formula::aqr::Failure;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Code {
    Invalid = 3,
    Decryption = 4,
    Resource = 5,
    Unsupported = 6,
    Incomplete = 7,
}

impl Code {
    fn rank(self) -> u8 {
        match self {
            Code::Incomplete => 1,
            Code::Invalid => 2,
            Code::Decryption => 3,
            Code::Resource => 4,
            Code::Unsupported => 5,
        }
    }

    fn of_rank(r: u8) -> Option<Code> {
        [Code::Incomplete, Code::Invalid, Code::Decryption, Code::Resource, Code::Unsupported]
            .get(r as usize - 1)
            .copied()
    }

    pub fn of(f: &Failure) -> Code {
        match f {
            Failure::Invalid(_) => Code::Invalid,
            Failure::Decryption(_) => Code::Decryption,
            Failure::Resource(_) => Code::Resource,
            Failure::Unsupported(_) => Code::Unsupported,
        }
    }
}

/// The rank of the most severe code noted (0: none).
static NOTED: AtomicU8 = AtomicU8::new(0);

/// Notes that `c` failed the run.
pub fn note(c: Code) {
    NOTED.fetch_max(c.rank(), Ordering::SeqCst);
}

/// `e`, noted where it is a [`Failure`]: for `map_err` before it is told as text.
pub fn seen<E: std::any::Any>(e: E) -> E {
    if let Some(f) = (&e as &dyn std::any::Any).downcast_ref::<Failure>() {
        note(Code::of(f));
    }
    e
}

/// The exit code of a run that failed: the most severe noted, else 1.
pub fn code() -> u8 {
    match NOTED.load(Ordering::SeqCst) {
        0 => 1,
        r => Code::of_rank(r).map_or(1, |c| c as u8),
    }
}
