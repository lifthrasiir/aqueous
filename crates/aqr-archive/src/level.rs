//! The levels of the archive ([`Level`]): the zstd level each stream is compressed at, picked
//! by a sample of it ([`Pick`]), or how it is stored ([`How`], [`Plan`]).

use super::*;

/// How hard an [`Encoder`] compresses: a level of the archive, which picks the zstd level of
/// each stream (and how hard aqr-compress recompresses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A level from 0 (fastest) to 9 (smallest), [`Level::LOW`], [`Level::MID`] and
    /// [`Level::HIGH`] being 2, 5 and 8. At 0 ([`Level::STORED`]) nothing is compressed: every
    /// block is uncompressed, but for those that are always Zstandard (formula, module and
    /// resource blocks), which hold raw Zstandard blocks. Otherwise each stream is uncompressed
    /// if incompressible (`PROBE_LEVEL`), by a sample and then, where the sample is not all of it, by
    /// all of it (which finds what repeats from afar), compressed at the probe level where only
    /// all of it compresses; else at the base zstd level of the level's `Pick`, or:
    ///
    /// - at a higher level of it where that saves enough for its time, at its rate of bytes
    ///   per second of compression;
    /// - at a lower level of it where that is smaller by 1/32 at least (beyond what a sample
    ///   may be off by), as it is faster too;
    /// - at its dense level, where it has one, if of PE code or text (`Encoder::how`): if
    ///   longer than a sample, always; else where that is smaller.
    ///
    /// Module and formula blocks, and streams shorter than `SMALL`, are at the base. Streams
    /// of [`Kind::Compressed`] skip the sample: all of each is compressed at the probe level,
    /// at any level but 0.
    Of(u8),
    /// Every stream at this zstd level, but those of [`Kind::Compressed`] (as for
    /// [`Level::Of`]).
    Zstd(i32),
}

impl Level {
    /// The fastest of [`Level::Of`].
    pub const MIN: u8 = 0;
    /// The smallest of [`Level::Of`].
    pub const MAX: u8 = 9;
    /// Low: compressing about 3 times as fast as [`Level::MID`] (the archiver's serial paths
    /// aside), decoding about twice (as it is now: the same modules).
    pub const LOW: Level = Level::Of(2);
    /// The default.
    pub const MID: Level = Level::Of(5);
    /// Nothing compressed.
    pub const STORED: Level = Level::Of(0);
    /// High: compressing at 40 MB/s and decoding at 200 MB/s at least, on 10gb.
    pub const HIGH: Level = Level::Of(8);

    /// The level of [`Level::Of`] that what is not zstd's goes by: [`Level::MID`]'s for a zstd
    /// level.
    pub fn scale(self) -> u8 {
        match self {
            Level::Of(n) => n.clamp(Level::MIN, Level::MAX),
            Level::Zstd(_) => 5,
        }
    }
}

/// `low`, `mid`, `high`, `0` to `9`, or `zstdN` (zstd level `N`).
impl std::str::FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Level, String> {
        match s {
            "low" => Ok(Level::LOW),
            "mid" => Ok(Level::MID),
            "high" => Ok(Level::HIGH),
            _ => match (s.strip_prefix("zstd"), s.parse::<u8>()) {
                (Some(l), _) => l.parse().map(Level::Zstd).map_err(|_| format!("not a zstd level: {l}")),
                (None, Ok(n)) if (Level::MIN..=Level::MAX).contains(&n) => Ok(Level::Of(n)),
                _ => Err(format!("not a level (low, mid, high, 0-9 or zstdN): {s}")),
            },
        }
    }
}

aqr_common::zopt_namespace!("zstd", "Zstandard levels and jobs of the archive's streams (aqr-formula)");

aqr_common::zopt! {
    /// The zstd level at which a stream is incompressible if it saves less than 1/32 (as a
    /// recompressor's entropy-coded output does), whatever level would compress it otherwise.
    /// A stream that its sample does not compress so, or of [`Kind::Compressed`], is compressed at
    /// this level all of it (some 1 GB/s on a core, of 13 instructions a byte with long-distance
    /// matching, on noise), and kept so where that saves 1/32: a sample of chunks of 64 KiB does
    /// not see what repeats from afar (a video's keyframes, the same each time).
    pub(crate) static PROBE_LEVEL: Opt<i32> = Opt::value("zstd:probe-level", 1, "zstd level that tells whether a stream compresses at all");
    /// A stream is kept compressed only where that saves 1/N of it at least ([`saves`]).
    static STORE_MARGIN: Opt<usize> = Opt::value("zstd:store-margin", 32, "a stream compressed is kept if it saves 1/N or more");
    /// A lower level of a [`Pick`] wins where it is smaller than the base by 1/N at least (beyond
    /// what a sample may be off by).
    pub(crate) static LOWER_MARGIN: Opt<usize> = Opt::value("zstd:lower-margin", 32, "a lower level wins if smaller than the base by 1/N or more");
    /// The base zstd level of [`Level::Of`] from 2 on.
    pub(crate) static BASE_LEVEL: Opt<i32> = Opt::value("zstd:base-level", 3, "base zstd level of archive levels from 2 on");
    /// The bytes per second the base level compresses (as measured on text and PSD skeletons, as
    /// are the others).
    static BASE_SPEED: Opt<f64> = Opt::value("zstd:base-speed", 350e6, "assumed speed of the base level, bytes per second");
    /// The zstd levels above the base that [`Level::Of`] picks from: of level 5 the first three,
    /// below as many fewer as it is lower; of level 9 all.
    static HIGHER: Opt<&'static [i32]> = Opt::value("zstd:higher", &[4, 6, 9, 16], "higher levels picked from, as many as the archive level allows");
    /// The speed of each of [`HIGHER`], bytes per second.
    static HIGHER_SPEEDS: Opt<&'static [f64]> =
        Opt::value("zstd:higher-speeds", &[330e6, 140e6, 100e6, 8.2e6], "assumed speeds of the higher levels, bytes per second");
    /// The bytes that [`Level::MID`] would save for a second of compression above the base:
    /// level 9 where it saves 0.71% of the data over level 3 (2??% of what they compress to,
    /// where 10gb's solid blocks save 8% at the median); zstd at 100 MB/s on a core is still far
    /// from all of compression's time.
    static MID_RATE: Opt<f64> = Opt::value("zstd:mid-rate", 1e6, "bytes saved per second that justify a higher level (archive levels below 6)");
    /// The zstd level of dense streams from level 8 ([`Level::HIGH`]) on: level 16 (btultra, 8.2
    /// MB/s on a core; 9 at 89) was smaller than 9 on every block of 1 MiB or more of 10gb's PE
    /// code (0.949 of it at the median, 0.996 at most) and text (0.935, 0.957 at most), about ??9
    /// MB for some 200 s of a core; not so on other data (raw 1.006 at most). Not in estimates
    /// ([`Encoder::least`]): a probe goes by the level picked.
    pub(crate) static DENSE_LEVEL: Opt<i32> = Opt::value("zstd:dense-level", 16, "zstd level of dense streams (PE code, text)");
    /// The archive level from which dense streams have a level of their own.
    pub(crate) static DENSE_FROM: Opt<u8> = Opt::value("zstd:dense-from", 8, "archive level from which dense streams get the dense level");
    /// The streams that are dense: `text` (of the kind), a class's name, or the start of one
    /// (ending with `/`).
    pub(crate) static DENSE_CLASSES: Opt<&'static [&'static str]> =
        Opt::value("zstd:dense-classes", &["text", "pe", "x86/", "arm64/"], "classes of dense streams: text, a name, or a prefix ending in /");
    /// The zstd level of the streams of [`TOGETHER`] from [`DENSE_FROM`] on, as dense streams
    /// are: of AdbeRdr940_hu_HU at -9 (one stream of 121 MB), 30.65 MB at 16, 28.86 MB at 19
    /// (23 s rather than 17), 28.71 MB at 22 (42 s).
    pub(crate) static TOGETHER_LEVEL: Opt<i32> = Opt::value("zstd:together-level", 19, "zstd level of the stream of all of a batch's classes (together:always) from zstd:dense-from on");
    /// Streams shorter than this are compressed at the base by [`Level::Of`], as picking would
    /// take longer than any level.
    pub(crate) static SMALL: Opt<usize> = Opt::size("zstd:small", 64 << 10, "streams shorter are compressed at the base level without picking");
    /// [`Level::Of`] picks the level by this many bytes of the stream, in [`SAMPLE_CHUNKS`]
    /// chunks spread evenly over it (or by the whole stream if no longer).
    pub(crate) static SAMPLE: Opt<usize> = Opt::size("zstd:sample", 1 << 20, "bytes of a stream that the level is picked by");
    pub(crate) static SAMPLE_CHUNKS: Opt<usize> = Opt::value("zstd:sample-chunks", 16, "chunks the sample is cut in, spread over the stream");
}

/// How [`Level::Of`] picks the zstd level of a stream (see there).
pub(crate) struct Pick {
    pub(crate) base: (i32, f64),
    /// Fastest first.
    pub(crate) lower: &'static [i32],
    pub(crate) higher: Vec<(i32, f64)>,
    /// The bytes worth saving for a second of compression above the base.
    pub(crate) rate: f64,
    pub(crate) dense: Option<i32>,
}

impl Pick {
    /// Level 1: zstd level 1 alone. 2??: the base, level 1 where smaller, and as many of
    /// [`HIGHER`] as the level is above 2. From 6 on, the higher level by the sample alone, as a
    /// sample of chunks of 64 KiB does not see the matches from afar that a higher level's
    /// larger window finds (10gb: some ??1 MB at 5's rate); from 8 on, the dense level for dense
    /// streams, and at 9 for any where the sample is smaller so.
    /// (Level 0 compresses nothing, and picks nothing: taken as 1.)
    pub(crate) fn of(level: u8) -> Pick {
        let n = level.clamp(1, Level::MAX);
        let (levels, speeds) = (HIGHER.get(), HIGHER_SPEEDS.get());
        assert_eq!(levels.len(), speeds.len(), "zstd:higher and zstd:higher-speeds are of different lengths");
        let base = (BASE_LEVEL.get(), BASE_SPEED.get());
        Pick {
            // (level 1's speed compared with no other)
            base: if n == 1 { (1, base.1) } else { base },
            lower: if n == 1 { &[] } else { &[1] },
            higher: levels
                .iter()
                .copied()
                .zip(speeds.iter().copied())
                .take(match n {
                    1 | 2 => 0,
                    3 => 1,
                    4 => 2,
                    5..=8 => 3,
                    _ => 4,
                })
                .collect(),
            rate: if n >= 6 { 0.0 } else { MID_RATE.get() },
            dense: (n >= DENSE_FROM.get()).then(|| DENSE_LEVEL.get()),
        }
    }

    /// The zstd level the most compressing of the levels picked from (not [`Pick::dense`]).
    pub(crate) fn top(&self) -> i32 {
        self.higher.last().map_or(self.base.0, |h| h.0)
    }
}

/// How a stream is compressed, by what it is ([`Encoder::how`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    /// Of [`Kind::Compressed`]: stored as it is, unless all of it compresses at [`PROBE_LEVEL`].
    Light,
    Usual,
    /// PE code or text, or the stream of [`TOGETHER`], at this zstd level where the [`Pick`] has
    /// a dense level.
    Dense(i32),
}

/// The class of a stream of all the classes of a batch (aqr-compress's `-Z together:always`),
/// compressed as dense streams are, at [`TOGETHER_LEVEL`].
pub const TOGETHER: &str = "together";

/// What an [`Encoder`] makes of a stream: compressed already ([`compressed`]), to be
/// compressed at a zstd level, to about so many bytes (by a sample), or to be compressed at
/// [`PROBE_LEVEL`] all of it and stored unless that saves enough ([`saves`]).
pub(crate) enum Plan {
    Done(Option<Vec<u8>>, i32),
    Level(i32, usize),
    Probe,
}

impl Plan {
    /// `data` compressed (or not) as planned, on up to `threads` threads, and at what level.
    pub(crate) fn carry_out(self, c: &mut CCtx, data: &[&[u8]], threads: usize) -> Result<(Compressed, i32)> {
        match self {
            Plan::Done(c, l) => Ok((c, l)),
            Plan::Level(l, size) => Ok((compressed_to(c, data, l, threads, size)?, l)),
            Plan::Probe => {
                let n = total(data);
                match compressed_to(c, data, PROBE_LEVEL.get(), threads, n)?.filter(|c| saves(c.len(), n)) {
                    Some(c) => Ok((Some(c), PROBE_LEVEL.get())),
                    None => Ok((None, STORED)),
                }
            }
        }
    }
}

/// Whether compressing `n` bytes to `len` is worth it rather than storing them ([`PROBE_LEVEL`]).
pub(crate) fn saves(len: usize, n: usize) -> bool {
    len < n - n / STORE_MARGIN.get()
}
