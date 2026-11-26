//! The command line parsed ([`Args`]): the options, the inputs, and the lists of files given.

use super::*;

pub(crate) struct Args {
    pub(crate) decompress: bool,
    /// Decompressing, but nothing written.
    pub(crate) test: bool,
    /// Listing the archives' entries (`-t`).
    pub(crate) list: bool,
    /// Telling all that an archive records (`-I`).
    pub(crate) info: bool,
    /// Printing the archive value an archive is interpreted to (`-Z interpret`, of `info` too:
    /// a command reading an archive alone, as `-I` is).
    pub(crate) interpret: bool,
    pub(crate) output: Option<PathBuf>,
    /// Decompressing, a single entry at the top beside the archive, else a directory of its name.
    pub(crate) auto_dir: bool,
    pub(crate) force: bool,
    /// `--rm`: the inputs removed once their archive is made, or an archive once extracted.
    pub(crate) rm: bool,
    /// `--run-before CMD`, `--run-after CMD`: commands run before and after each archive is
    /// written ([`crate::hooks`]).
    pub(crate) run_before: Vec<String>,
    pub(crate) run_after: Vec<String>,
    /// `--atomic`: compressing, an archive made whole or not at all, rather than committed as it
    /// is written ([`stop`]).
    pub(crate) atomic: bool,
    /// `-B SIZE` given: the batches of that size, not fitted to the files ([`crate::compress::fitted_batch`]).
    pub(crate) batch_given: bool,
    /// `-j LABEL`: compressing, the state the archive is left in named as a journal point
    /// ([`journal`]), the label as given (`opt.journal` has it with its placeholders filled in).
    pub(crate) journal: Option<String>,
    /// `--at SPEC`: listing, testing or decompressing, the archive as it was at a journal point
    /// ([`journal`]).
    pub(crate) at: Option<journal::At>,
    /// `--history`: the journal points of the archives listed.
    pub(crate) history: bool,
    /// `--tag LABEL`: the archive's state named as a journal point, nothing else changed.
    pub(crate) tag: Option<String>,
    /// `--diff`: what differs between two states of the archives (`--from` and `--at` pick them).
    pub(crate) diff: bool,
    pub(crate) from: Option<journal::At>,
    /// `--keep-since SPEC`: `--compact` keeping the journal points from the one `SPEC` picks on
    /// (with the states between); `--drop-history`: dropping them all, with those states.
    pub(crate) keep_since: Option<journal::At>,
    pub(crate) drop_history: bool,
    /// `-a`: compressing, the inputs appended to an archive ([`append`]), the first input or `-o`.
    pub(crate) append: bool,
    /// `-s` (with `append`): what the archive holds that the inputs do not taken away too.
    pub(crate) sync: bool,
    /// Editing the archive (the first input) in place ([`edit`]), and writing it again of what
    /// it holds (`--compact`).
    pub(crate) edit: Option<edit::Edit>,
    pub(crate) compact: bool,
    /// `--convert`: each input written again as an Aqueous archive ([`convert`]); `-P` and `-A`
    /// put on, or taken off by `--no-passphrase` and `--no-archival`.
    pub(crate) convert: bool,
    pub(crate) no_passphrase: bool,
    pub(crate) no_archival: bool,
    /// The options given that change how the files are compressed (`-l`, `-S`, ...): not for an
    /// Aqueous archive converted, which is not compressed again.
    pub(crate) recompressing: Vec<&'static str>,
    /// `-L`: compressing, the links found (and given) followed, archived as what they lead to.
    pub(crate) dereference: bool,
    /// What is not commonly kept of the metadata kept too ([`meta`]).
    pub(crate) preserve: bool,
    /// `--mark WHEN`: how the files are marked as come from elsewhere ([`meta::Marks`]).
    pub(crate) mark: Option<meta::Marks>,
    pub(crate) quiet: bool,
    /// Logging each file archived.
    pub(crate) verbose: bool,
    pub(crate) opt: Options,
    /// Compressing, the paths found (in the directory of `-C` before each); decompressing, the
    /// archives, then the paths of one selected.
    pub(crate) inputs: Vec<PathBuf>,
    /// Compressing, by input, the path as given (from the directory of `-C` before it), which
    /// it is archived by.
    pub(crate) given: Vec<PathBuf>,
    /// Decompressing, the directory of `-C` (empty: the current one), where what is extracted
    /// goes.
    pub(crate) dir: PathBuf,
    /// Compressing, by input, the path in the archive that `--rename` gives it.
    pub(crate) renames: Vec<Option<PathBuf>>,
    /// What `--stats` asked for.
    pub(crate) stats: Option<&'static stats::Stats>,
    /// Driven by a frontend ([`ipc`]).
    pub(crate) ipc: bool,
    /// Started for a frontend that tells what to do later ([`session`]).
    pub(crate) session: bool,
    /// Serving a frontend the entries and files of archives ([`serve`]).
    pub(crate) serve: bool,
    /// Listing as JSON (`--json`).
    pub(crate) json: bool,
    /// Compressing, into archives of this format (`--format`, or `-o`'s name).
    pub(crate) format: Pack,
    /// `-l` as given (read as the format's level: [`Args::format`]'s, or Aqueous's).
    level: Option<String>,
    /// `-P`: compressing, the archives encrypted with a passphrase asked for; decompressing,
    /// the passphrase asked for before the archives need it ([`pass`]).
    pub(crate) passphrase: bool,
    /// Compressing, what is left out of the inputs (`-x`).
    pub(crate) exclude: Exclude,
    /// Compressing, the files of attributes given (`--attributes`, docs/hints-design.md).
    pub(crate) attributes: Vec<PathBuf>,
    /// `--passphrase-from`: where `-P`'s passphrase is taken (implying it); of `--convert`, where
    /// the input's and `-P`'s are, as many as are needed (not implying `-P`).
    pub(crate) pass_from: Vec<pass::Source>,
    /// `--each`: decompressing or listing, every input an archive.
    each: bool,
    /// Decompressing (or listing) some paths of the archive: each path (names as the tree has them), and
    /// where it goes where `--rename` tells.
    pub(crate) select: Option<extract::Selection>,
    /// Decompressing, how names are written (`-E`, `--normalize`, `--bad-names`).
    pub(crate) names: names::Opts,
    /// Decompressing, which links are made (`--links`).
    pub(crate) links: extract::Links,
    /// The input is standard input (`-`, alone).
    pub(crate) stdin: bool,
    /// The output is standard output (`-o -`; or by default, of standard input): compressing,
    /// the archive; decompressing, the file of an archive of a single file, or the one of its
    /// paths given.
    pub(crate) stdout: bool,
}

/// What is compressed, by input: the path found, the path as given (from the directory of `-C`
/// before it, which it is archived by) and the path in the archive that `--rename` gives it.
#[derive(Clone, Copy)]
pub(crate) struct Inputs<'a> {
    pub(crate) paths: &'a [PathBuf],
    pub(crate) given: &'a [PathBuf],
    pub(crate) renames: &'a [Option<PathBuf>],
}

impl<'a> Inputs<'a> {
    pub(crate) fn of(a: &'a Args) -> Self {
        Inputs { paths: &a.inputs, given: &a.given, renames: &a.renames }
    }

    /// The inputs from the `skip`th on.
    pub(crate) fn from(self, skip: usize) -> Self {
        Inputs { paths: &self.paths[skip..], given: &self.given[skip..], renames: &self.renames[skip..] }
    }

    /// Each input alone.
    pub(crate) fn each(self) -> impl Iterator<Item = Inputs<'a>> {
        use std::slice::from_ref as one;
        let all = self.paths.iter().zip(self.given).zip(self.renames);
        all.map(|((path, given), rename)| Inputs { paths: one(path), given: one(given), renames: one(rename) })
    }

    /// The path of the input where it is alone and not renamed.
    pub(crate) fn sole(&self) -> Option<&'a PathBuf> {
        match (self.paths, self.renames) {
            ([input], [None]) => Some(input),
            _ => None,
        }
    }

    /// The paths found, as one line.
    pub(crate) fn shown(&self) -> String {
        self.paths.iter().map(|i| i.display().to_string()).collect::<Vec<_>>().join(" ")
    }
}

/// The format of the archives made: Aqueous, or another of [`aqr_pack`] at its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pack {
    Aqr,
    Other(aqr_pack::Format, Option<u8>),
    /// A self-extracting program ([`sfx`]).
    Sfx,
}

impl Pack {
    /// The extension of its archives.
    pub(crate) fn ext(self) -> String {
        match self {
            Pack::Aqr => ".aqr".into(),
            Pack::Other(f, _) => f.ext(),
            Pack::Sfx => ".exe".into(),
        }
    }
}

impl Args {
    /// The option appending as given, for messages: `-a` or `-s`.
    pub(crate) fn verb(&self) -> &'static str {
        if self.sync { "-s" } else { "-a" }
    }
}

pub(crate) fn parse() -> Result<Args, String> {
    parse_from(std::env::args_os().collect())
}

/// The arguments `argv` (the program's name first) as [`Args`].
pub(crate) fn parse_from(all: Vec<OsString>) -> Result<Args, String> {
    let mut argv = all.clone().into_iter();
    let me = argv.next().map(PathBuf::from).unwrap_or_default();
    // (`--stats` alone, no input nor any other option: [`stats::alone`])
    let stats_alone =
        argv.len() > 0 && all.iter().skip(1).all(|a| a == "--stats" || a.to_string_lossy().starts_with("--stats="));
    let unaqr = me.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("unaqr"));
    // (a command alone: [`complete`])
    let all: Vec<OsString> = all.iter().skip(1).cloned().collect();
    if let Some(first) = all.first().map(|f| f.to_string_lossy().into_owned())
        && (first == "--setup-autocomplete" || first.starts_with("--setup-autocomplete="))
    {
        let shell = match (first.split_once('='), &all[1..]) {
            (Some((_, s)), []) => Some(s.to_owned()),
            (None, []) => None,
            (None, [s]) => Some(s.to_string_lossy().into_owned()),
            _ => return Err("--setup-autocomplete is a command alone (no other argument)".into()),
        };
        std::process::exit(match complete::setup(shell.as_deref()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("aqr: {e}");
                1
            }
        });
    }
    let mut a = Args {
        decompress: unaqr,
        test: false,
        list: false,
        info: false,
        interpret: false,
        output: None,
        auto_dir: false,
        force: false,
        rm: false,
        run_before: Vec::new(),
        run_after: Vec::new(),
        atomic: false,
        batch_given: false,
        journal: None,
        at: None,
        history: false,
        diff: false,
        tag: None,
        from: None,
        keep_since: None,
        drop_history: false,
        append: false,
        sync: false,
        edit: None,
        compact: false,
        convert: false,
        no_passphrase: false,
        no_archival: false,
        recompressing: Vec::new(),
        dereference: false,
        preserve: false,
        mark: None,
        quiet: false,
        verbose: false,
        opt: Options { threads: 0, ..Options::default() },
        inputs: Vec::new(),
        given: Vec::new(),
        dir: PathBuf::new(),
        renames: Vec::new(),
        stats: None,
        ipc: false,
        session: false,
        serve: false,
        json: false,
        format: Pack::Aqr,
        level: None,
        passphrase: false,
        pass_from: Vec::new(),
        exclude: Exclude::default(),
        attributes: Vec::new(),
        each: false,
        select: None,
        names: names::Opts::default(),
        links: extract::Links::Inside,
        stdin: false,
        stdout: false,
    };
    // (by the number of inputs before it, `--rename`'s place and value)
    let mut renames: Vec<(usize, PathBuf)> = Vec::new();
    let (mut stats, mut stats_to) = (false, Vec::new());
    let mut format = None;
    let mut sfx = false;
    let mut null = false;
    // (`--files-from -`: standard input taken by a list)
    let (mut list_stdin, mut listed) = (false, false);
    let mut memory = None;
    let mut application_bound = None;
    // (`--lossy[=SPEC]`, checked once all are read)
    let mut lossy: Option<String> = None;
    // (`-C`: the directory so far, by input the one it was given in, and the last given)
    let (mut dir, mut dirs, mut last_dir) = (PathBuf::new(), Vec::new(), None);
    // (`-Z`: the developer options, set once all are read)
    let mut zopts = Vec::new();
    let mut lex = Lexer { argv, short: Default::default(), only_inputs: false };
    while let Some(tok) = lex.next() {
        let (s, mut inline) = match tok {
            Tok::Input(i) => {
                a.inputs.push(i.into());
                dirs.push(dir.clone());
                continue;
            }
            Tok::Opt(s, inline) => (s, inline),
        };
        // (as given: a path need not be Unicode; [`text`] for the others)
        let mut value = |name: &str| match inline.take() {
            Some(v) => Ok(v),
            None => lex.value().ok_or(format!("{name} takes a value")),
        };
        match &*s {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("aqr {}", env!("AQR_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--compress" => (a.decompress, a.list, a.info, a.interpret) = (false, false, false, false),
            "-d" | "--decompress" => {
                (a.decompress, a.test, a.list, a.info, a.interpret) = (true, false, false, false, false)
            }
            "-K" | "--test" => (a.decompress, a.test, a.list, a.info, a.interpret) = (true, true, false, false, false),
            "-t" | "--list" => (a.list, a.info, a.interpret) = (true, false, false),
            "-I" | "--info" => (a.info, a.list, a.interpret) = (true, false, false),
            "--delete" => a.edit = Some(edit::Edit::Delete),
            "--move" => a.edit = Some(edit::Edit::Move),
            "--compact" => a.compact = true,
            "--convert" => a.convert = true,
            "--no-passphrase" => a.no_passphrase = true,
            "--no-archival" => a.no_archival = true,
            // (a command, not an option: none of aqr_common::zopt's)
            "-Z" => match text(value("-Z")?) {
                z if z == "interpret" => (a.info, a.list, a.interpret) = (true, false, true),
                z => zopts.push(z),
            },
            "-o" | "--output" => a.output = Some(value("-o")?.into()),
            "-C" | "--directory" => {
                let to = dir.join(value("-C")?);
                if let Err(e) = fs::read_dir(&to) {
                    return Err(format!("-C {}: {e}", to.display()));
                }
                (dir, last_dir) = (to, Some(a.inputs.len()));
            }
            "--auto-dir" => a.auto_dir = true,
            "-f" | "--force" => a.force = true,
            "--rm" => a.rm = true,
            "--run-before" => a.run_before.push(text(value("--run-before")?)),
            "--run-after" => a.run_after.push(text(value("--run-after")?)),
            "--atomic" => a.atomic = true,
            "-j" | "--journal" => a.journal = Some(text(value("-j")?)),
            "--at" => a.at = Some(journal::at_of(&text(value("--at")?))?),
            "--history" => a.history = true,
            "--diff" => a.diff = true,
            "--tag" => a.tag = Some(text(value("--tag")?)),
            "--from" => a.from = Some(journal::at_of(&text(value("--from")?))?),
            "--keep-since" => a.keep_since = Some(journal::at_of(&text(value("--keep-since")?))?),
            "--drop-history" => a.drop_history = true,
            "-a" | "--append" => (a.append, a.sync) = (true, false),
            "-s" | "--sync" => (a.append, a.sync) = (true, true),
            "-L" | "--dereference" => a.dereference = true,
            "-p" | "--preserve" => a.preserve = true,
            "--mark" => a.mark = Some(meta::Marks::parse(&text(value("--mark")?))?),
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-T" | "--threads" => a.opt.threads = text(value("-T")?).parse().map_err(|_| "-T takes a number")?,
            "-M" | "--memory" => memory = Some(text(value("-M")?)),
            "-l" | "--level" => {
                a.level = Some(text(value("-l")?));
                a.recompressing.push("-l");
            }
            "--format" => {
                sfx = false;
                format = Some(match &*text(value("--format")?) {
                    "aqr" => None,
                    "sfx" => {
                        sfx = true;
                        None
                    }
                    f => match aqr_pack::Format::by_format_name(f) {
                        Some(f) => Some(f),
                        None => {
                            let names = aqr_pack::Format::names().collect::<Vec<_>>().join(", ");
                            return Err(format!("not a format to compress into (aqr, {names}, sfx): {f}"));
                        }
                    },
                })
            }
            "--depth" => {
                a.recompressing.push("--depth");
                a.opt.depth = text(value("--depth")?).parse().map_err(|_| "--depth takes a number")?
            }
            "-B" | "--batch" => {
                a.recompressing.push("-B");
                a.opt.batch = size_of(&text(value("-B")?)).ok_or("-B takes a size (64M, 1G, ...)")?.max(1);
                a.batch_given = true;
            }
            "-b" | "--block-size" => {
                a.recompressing.push("-b");
                a.opt.block = size_of(&text(value("-b")?)).ok_or("-b takes a size (64K, 1M, ...)")?.max(1)
            }
            "-S" | "--segment-size" => {
                a.recompressing.push("-S");
                a.opt.segment = size_of(&text(value("-S")?)).ok_or("-S takes a size (64M, 1G, ...)")?
            }
            "-D" | "--dictionaries" => {
                a.recompressing.push("-D");
                a.opt.dictionaries = true
            }
            "-A" | "--archival" => a.opt.mirrors = true,
            "--no-check" => a.opt.verify = false,
            "--lossy" => {
                a.recompressing.push("--lossy");
                lossy = Some(text(value("--lossy (what may be lost: CLASS[,CLASS...], of coding, slack, all)")?))
            }
            "--stats" => match inline.take() {
                None => stats = true,
                Some(to) => {
                    let to = PathBuf::from(to);
                    if !stats::takes(&to) {
                        return Err("--stats= takes a file named .json or .html".into());
                    }
                    stats_to.push(to);
                }
            },
            "--application-bound" => {
                let v = text(value("--application-bound")?);
                application_bound = Some(bound_of(&v)?);
            }
            "--ipc" => a.ipc = true,
            "--session" => (a.session, a.ipc) = (true, true),
            "--serve" => a.serve = true,
            "--json" => a.json = true,
            "--setup-autocomplete" => return Err("--setup-autocomplete is a command alone (no other argument)".into()),
            "-P" | "--passphrase" => a.passphrase = true,
            "--passphrase-from" => {
                a.pass_from = pass::Source::parse_list(&value("--passphrase-from")?)?;
            }
            "--each" => a.each = true,
            "-x" | "--exclude" => a.exclude.add(&value("-x")?)?,
            "--attributes" => a.attributes.push(value("--attributes")?.into()),
            "--null" => null = true,
            "--files-from" => {
                let from = value("--files-from")?;
                list_stdin |= stdio::is(Path::new(&from));
                listed = true;
                for p in files_from(&from, null)? {
                    if stdio::is(&p) {
                        return Err(format!("--files-from {}: - (standard input) among the paths", from.display()));
                    }
                    a.inputs.push(p);
                    dirs.push(dir.clone());
                }
            }
            "--rename" => renames.push((a.inputs.len(), value("--rename")?.into())),
            "-E" | "--encoding" => a.names.encoding = aqr_unpack::Choice::parse(&text(value("-E")?))?,
            "--normalize" => a.names.normalize = names::Opts::normalize(&text(value("--normalize")?))?,
            "--bad-names" => a.names.bad = names::Opts::bad(&text(value("--bad-names")?))?,
            "--links" => a.links = extract::Links::parse(&text(value("--links")?))?,
            // -0 to -9, and whatever level of more digits there may be (one level, not several)
            s if s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_digit()) => {
                a.level = Some(s[1..].to_string());
                a.recompressing.push("-l");
            }
            s => return Err(format!("unknown option {s}")),
        }
        if inline.is_some() {
            return Err(format!("{s} takes no value"));
        }
    }
    // (`--passphrase-from` of one source implies `-P`, but of `--convert`, where `-P` tells
    // whether a new one is needed: two are its alone)
    if !a.convert && !a.pass_from.is_empty() {
        if a.pass_from.len() > 1 {
            return Err(
                "--passphrase-from: two sources (apart by ;) are of --convert alone (a path of a ;: fd:N)".into()
            );
        }
        a.passphrase = true;
    }
    // (elsewhere they ask for what is the default, but against -P and -A)
    if a.passphrase && a.no_passphrase {
        return Err("-P with --no-passphrase".into());
    }
    if a.opt.mirrors && a.no_archival {
        return Err("-A with --no-archival".into());
    }
    // (compressing, there is no archive's mark to give, nor one to make)
    if !a.decompress
        && !a.list
        && !a.info
        && let Some(m @ (meta::Marks::Always | meta::Marks::Propagate)) = a.mark
    {
        return Err(format!("--mark={} is for extracting: compressing, keep or never", m.name()));
    }
    match aqr_common::zopt::apply(&zopts)? {
        aqr_common::zopt::Applied::Set => {
            aqr_common::mem::log_from_option();
            // (the options' defaults that `Options` took when it was made, before `-Z`: taken
            // again, as nothing else sets them)
            let d = Options::default();
            (a.opt.smaller, a.opt.description, a.opt.expansion) = (d.smaller, d.description, d.expansion);
        }
        aqr_common::zopt::Applied::Help(h) => {
            print!("{h}");
            std::process::exit(0);
        }
    }
    if stats_alone {
        let r = stats::alone(stats, &stats_to, std::io::stderr().is_terminal());
        std::process::exit(match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("aqr: {e}");
                1
            }
        });
    }
    // (none given, of standard input where it is not a terminal: as zstd and gzip have it)
    if a.inputs.is_empty() && !listed && !a.list && !a.info && !a.ipc && !a.serve && !std::io::stdin().is_terminal() {
        a.inputs.push("-".into());
        dirs.push(dir.clone());
    }
    if a.inputs.is_empty() && !a.session && !a.serve {
        return Err("no input".into());
    }
    if stats || !stats_to.is_empty() {
        a.stats = Some(stats::init(stats_to));
    }
    if a.opt.threads == 0 {
        a.opt.threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    }
    let system = (aqr_common::budget::total(), aqr_common::budget::available());
    let limit = budget(memory.as_deref(), system)?;
    aqr_common::budget::global().set_limit(limit);
    if memory.is_some() {
        // (and the heap of a recompression: it gives up past it, the data kept as it is)
        aqr_common::budget::set_attempt_limit(limit);
        // (what a user asked for also bounds the memories of an application of a module in Wasm:
        // its concurrency is lowered to fit, and it fails if it does not at 1)
        aqr_decompress::set_application_bound(limit);
    }
    match application_bound {
        // (compressing: the most an application may need to be made, which the archive's check
        // by decoding then has to allow; decompressing: the most one is let to need)
        Some(b) if !a.decompress => {
            a.opt.application_bound = Some(b);
            aqr_formula::set_application_limit(b.max(aqr_formula::SUPPORTED_BOUND));
        }
        Some(b) => aqr_formula::set_application_limit(b),
        None => {}
    }
    if let Some(spec) = &lossy {
        let modes = [
            (a.decompress || a.list || a.info, "it is for compressing"),
            (a.edit.is_some() || a.compact, "an archive edited keeps its files as they are"),
            (a.tag.is_some() || a.history || a.diff, "it is for compressing"),
            (a.opt.mirrors, "-A (an archive for preservation keeps its files as they are)"),
        ];
        if let Some((_, why)) = modes.iter().find(|m| m.0) {
            return Err(format!("--lossy: {why}"));
        }
        a.opt.lossy = Some(aqr_compress::lossy::Lossy::parse(spec)?);
    }
    if a.serve {
        if !a.inputs.is_empty() || a.ipc {
            return Err("--serve takes no inputs (they come with its requests), nor --ipc".into());
        }
        return Ok(a);
    }
    if a.session {
        if !a.inputs.is_empty() {
            return Err("--session takes no inputs (they come with its begin)".into());
        }
        return Ok(a);
    }
    if let Some(o) = hooks(&a) {
        let reading = [
            (a.decompress, "-d or -K"),
            (a.list, "-t"),
            (a.info, "-I"),
            (a.tag.is_some(), "--tag"),
            (a.history, "--history"),
            (a.diff, "--diff"),
        ];
        if let Some((_, what)) = reading.iter().find(|m| m.0) {
            return Err(format!("{o} with {what} (it is of an archive written)"));
        }
    }
    if a.convert {
        convert_args(&mut a, renames, last_dir.is_some())?;
        return Ok(a);
    }
    if a.edit.is_some() || a.compact {
        edit_args(&mut a, renames, last_dir.is_some())?;
        return Ok(a);
    }
    if a.keep_since.is_some() || a.drop_history {
        return Err("--keep-since and --drop-history are for --compact".into());
    }
    if a.tag.is_some() {
        if a.decompress || a.list || a.info || a.journal.is_some() || a.output.is_some() || a.rm || a.history || a.diff
        {
            return Err("--tag takes a label and an archive alone".into());
        }
        if a.inputs.len() != 1 {
            return Err("--tag takes one archive".into());
        }
        a.opt.journal = None;
        return Ok(a);
    }
    if a.history || a.diff {
        let what = if a.diff { "--diff" } else { "--history" };
        if a.history && a.diff {
            return Err("--history with --diff".into());
        }
        if a.decompress || a.list || a.info || a.journal.is_some() || a.output.is_some() || a.rm {
            return Err(format!("{what} takes an archive{} alone", if a.diff { " and paths" } else { "s" }));
        }
        if a.history && (a.at.is_some() || a.from.is_some()) {
            return Err("--history takes archives alone".into());
        }
        // (--diff with paths: they are taken as the inputs of -a are, below)
        if a.history {
            return Ok(a);
        }
    }
    if a.from.is_some() && !a.diff {
        return Err("--from is for --diff".into());
    }
    if a.at.is_some() && !(a.list || a.decompress || a.diff) {
        return Err("--at is for -t, -d, -K and --diff".into());
    }
    if let Some(label) = &a.journal {
        if a.decompress || a.list || a.info {
            return Err("-j is for compressing".into());
        }
        a.opt.journal = Some(journal::point(label, journal::now_ms()?)?);
    }
    if a.json && !(a.list || a.diff) {
        return Err("--json is for -t, --history and --diff".into());
    }
    if a.info {
        if a.rm || a.each || a.dereference || a.output.is_some() || !renames.is_empty() || last_dir.is_some() {
            return Err(match a.interpret {
                true => "-Z interpret takes an archive alone".into(),
                false => "-I takes an archive and its paths alone".into(),
            });
        }
        if a.interpret && a.inputs.len() > 1 {
            return Err("-Z interpret takes an archive alone".into());
        }
        return Ok(a);
    }
    if a.list {
        if a.rm {
            return Err("--rm with -t".into());
        }
        if a.output.is_some() || !renames.is_empty() || last_dir.is_some() {
            return Err("-t takes an archive and its paths alone".into());
        }
        if !a.each && a.inputs.len() > 1 {
            a.select = Some(selection(a.inputs.split_off(1), Vec::new())?);
        }
    }
    if (!a.exclude.names.is_empty() || !a.exclude.paths.is_empty()) && (a.decompress || a.list || a.info) {
        return Err("-x is for compressing".into());
    }
    if !a.attributes.is_empty() && (a.decompress || a.list || a.info) {
        return Err("--attributes is for compressing".into());
    }
    if a.dereference && (a.decompress || a.list) {
        return Err("-L is for compressing".into());
    }
    if a.dereference && a.rm {
        // (what a link leads to is not the input's to remove, and removing the link alone
        // would not take away what was archived)
        return Err("--rm with -L".into());
    }
    if a.list {
        return Ok(a);
    }
    if !a.decompress {
        if a.each {
            return Err("--each is for decompressing".into());
        }
        let named = a.output.as_ref().and_then(|o| aqr_pack::Format::by_name(&os_bytes(o.as_os_str())));
        a.format = match format.unwrap_or(named) {
            None => Pack::Aqr,
            Some(f) => {
                let (range, _) = f.levels();
                let level = match &a.level {
                    None => None,
                    Some(l) => match l.parse::<u8>() {
                        Ok(n) if range.contains(&n) => Some(n),
                        _ => {
                            let (lo, hi) = (range.start(), range.end());
                            return Err(format!("not a level of {} ({lo}-{hi}): {l}", f.ext()));
                        }
                    },
                };
                if a.opt.mirrors {
                    return Err(format!("-A: {} archives are not made so here (.aqr only)", f.ext()));
                }
                if a.opt.lossy.is_some() {
                    return Err(format!("--lossy: {} archives are not made so here (.aqr only)", f.ext()));
                }
                if a.passphrase && !f.encrypts() {
                    return Err(format!("-P: {} archives are not encrypted here (.zip and .7z are)", f.ext()));
                }
                Pack::Other(f, level)
            }
        };
    }
    if a.format == Pack::Aqr
        && let Some(l) = &a.level
    {
        a.opt.level = l.parse()?;
    }
    if sfx && !a.decompress {
        let not = |what: &str| Err(format!("--format sfx: not with {what}"));
        match () {
            _ if a.append => return not("-a"),
            _ if a.opt.mirrors => return not("-A"),
            _ if a.passphrase => return not("-P (the program does not ask for one)"),
            _ if a.journal.is_some() => return not("-j"),
            _ => {}
        }
        a.format = Pack::Sfx;
        // (the modules the program carries named by their hashes alone)
        a.opt.modules = sfx::wasm_of;
    }
    if !a.decompress {
        if last_dir == Some(a.inputs.len()) {
            return Err(format!("-C {} not before an input", dir.display()));
        }
        a.given = std::mem::take(&mut a.inputs);
        a.inputs = a.given.iter().zip(&dirs).map(|(i, d)| if stdio::is(i) { i.clone() } else { d.join(i) }).collect();
        a.renames = vec![None; a.inputs.len()];
        for (n, to) in renames {
            let Some(k) = n.checked_sub(1) else {
                return Err(format!("--rename {} not after an input", to.display()));
            };
            if a.renames[k].replace(to).is_some() {
                return Err(format!("{}: --rename given twice", a.inputs[k].display()));
            }
        }
    } else if !a.each && a.inputs.len() > 1 {
        a.select = Some(selection(a.inputs.split_off(1), renames)?);
        if a.auto_dir {
            return Err("--auto-dir with paths of an archive".into());
        }
    } else if let Some((_, to)) = renames.first() {
        return Err(format!("--rename {} not after a path of an archive", to.display()));
    }
    if a.rm && a.test {
        return Err("--rm with -K".into());
    }
    if a.rm && a.select.is_some() {
        return Err("--rm with paths of an archive (it is removed once extracted whole)".into());
    }
    if a.decompress && a.output.is_some() && a.inputs.len() > 1 && !a.auto_dir {
        return Err("-o with several archives to decompress".into());
    }
    if list_stdin && (a.inputs.iter().any(|i| stdio::is(i)) || a.pass_from.contains(&pass::Source::Stdin)) {
        return Err("--files-from - with standard input taken otherwise too".into());
    }
    stdio_args(&mut a)?;
    if a.stdout && hooks(&a).is_some() {
        return Err(format!("{}: an archive to standard output is no file to run it on", hooks(&a).unwrap()));
    }
    if a.format == Pack::Sfx && (a.stdin || a.stdout) {
        return Err("--format sfx: not with standard input or output".into());
    }
    a.dir = dir;
    Ok(a)
}

/// The option of the hooks given (`--run-before`, else `--run-after`), where any is.
fn hooks(a: &Args) -> Option<&'static str> {
    match (a.run_before.is_empty(), a.run_after.is_empty()) {
        (false, _) => Some("--run-before"),
        (true, false) => Some("--run-after"),
        (true, true) => None,
    }
}

/// Of [`parse`], editing an archive ([`edit`]): the archive and its paths, checked against the
/// other options given (`renames` as [`selection`] takes them; `dir`: `-C` given).
fn edit_args(a: &mut Args, renames: Vec<(usize, PathBuf)>, dir: bool) -> Result<(), String> {
    let o = a.edit.map_or("--compact", edit::Edit::option);
    let modes = [
        (a.decompress && !a.test, "-d"),
        (a.test, "-K"),
        (a.list, "-t"),
        (a.info, "-I"),
        (a.append, a.verb()),
        (a.passphrase, "-P (an encrypted archive is not edited: --convert takes its encryption off)"),
        (a.opt.mirrors, "-A"),
        (a.journal.is_some(), "-j"),
        (a.output.is_some(), "-o"),
        (dir, "-C"),
        (a.rm, "--rm"),
        (a.each, "--each"),
        (a.dereference, "-L"),
        (a.auto_dir, "--auto-dir"),
        (a.json, "--json"),
        (!a.exclude.names.is_empty() || !a.exclude.paths.is_empty(), "-x"),
    ];
    if let Some((_, what)) = modes.iter().find(|m| m.0) {
        return Err(format!("{o} with {what}"));
    }
    if a.keep_since.is_some() && a.drop_history {
        return Err("--keep-since with --drop-history".into());
    }
    if a.keep_since.is_some() && a.edit.is_some() {
        return Err("--keep-since is for --compact alone (the paths edited are not in the states between)".into());
    }
    if (a.keep_since.is_some() || a.drop_history) && a.edit == Some(edit::Edit::Delete) && !a.compact {
        return Err("--drop-history is for --compact (--delete alone only adds)".into());
    }
    if a.inputs.iter().any(|i| stdio::is(i)) {
        return Err(format!("{o}: an archive in place, not - (standard input)"));
    }
    if a.edit != Some(edit::Edit::Move)
        && let Some((_, to)) = renames.first()
    {
        return Err(format!("{o}: --rename {} (paths are taken out)", to.display()));
    }
    match a.edit {
        None if a.inputs.len() != 1 => return Err("--compact alone takes an archive alone".into()),
        None => {}
        Some(_) if a.inputs.len() < 2 => return Err(format!("{o} takes an archive and its paths")),
        Some(_) => a.select = Some(selection(a.inputs.split_off(1), renames)?),
    }
    if a.edit == Some(edit::Edit::Move)
        && let Some(sel) = &a.select
        && let Some(k) = sel.paths.iter().position(|p| p.1.is_none())
    {
        return Err(format!("--move: {} without --rename TO after it", sel.shown(k)));
    }
    if let Some(l) = &a.level {
        a.opt.level = l.parse()?;
    }
    Ok(())
}

/// Of [`parse`], converting archives ([`convert`]): the inputs, checked against the other options
/// given (`dir`: `-C` given).
fn convert_args(a: &mut Args, renames: Vec<(usize, PathBuf)>, dir: bool) -> Result<(), String> {
    let modes = [
        (a.decompress && !a.test, "-d"),
        (a.test, "-K"),
        (a.list, "-t"),
        (a.info, "-I"),
        (a.append, a.verb()),
        (a.edit.is_some(), a.edit.map_or("", edit::Edit::option)),
        (a.compact, "--compact (--convert keeps the archive as it is, but with --keep-since or --drop-history)"),
        (a.tag.is_some(), "--tag"),
        (a.history, "--history"),
        (a.diff, "--diff"),
        (a.at.is_some(), "--at"),
        (a.from.is_some(), "--from"),
        (a.journal.is_some(), "-j"),
        (dir, "-C"),
        (a.rm, "--rm"),
        (a.each, "--each"),
        (a.dereference, "-L"),
        (a.auto_dir, "--auto-dir"),
        (a.json, "--json"),
        (!renames.is_empty(), "--rename"),
        (hooks(a).is_some(), hooks(a).unwrap_or("")),
        (a.format != Pack::Aqr, "--format (it writes Aqueous archives)"),
        (!a.exclude.names.is_empty() || !a.exclude.paths.is_empty(), "-x"),
    ];
    if let Some((_, what)) = modes.iter().find(|m| m.0) {
        return Err(format!("--convert with {what}"));
    }
    if a.keep_since.is_some() && a.drop_history {
        return Err("--keep-since with --drop-history".into());
    }
    if a.pass_from.len() > 2 {
        return Err("--passphrase-from: at most two sources (the input's;-P's)".into());
    }
    if a.inputs.iter().any(|i| stdio::is(i)) {
        return Err("--convert: an archive in a file, not - (standard input)".into());
    }
    if a.output.as_ref().is_some_and(|o| stdio::is(o)) {
        return Err("--convert: -o to a file, not - (standard output)".into());
    }
    if a.output.is_some() && a.inputs.len() > 1 {
        return Err("--convert: -o with several inputs".into());
    }
    if let Some(l) = &a.level {
        a.opt.level = l.parse()?;
    }
    Ok(())
}

/// An argument as [`Lexer`] reads it: an input, or an option (`-x` or `--long`) with the value
/// given in the same argument (`-xVALUE`, `--long=VALUE`).
enum Tok {
    Input(OsString),
    Opt(String, Option<OsString>),
}

/// The short options that take a value: in a bundle (`-cfo OUT`), the rest of the argument is
/// theirs (`-oOUT`), or else the next argument.
const SHORT_VALUES: &[u8] = b"oCTMlBbSExjZ";

/// Reads the arguments as options and inputs, getopt's way: short options bundled (`-cf`), a
/// value joined to its option (`-T8`, `--memory=2G`), `--` ending the options, `-` an input
/// (standard input or output). The digits of a level (`-10`) are one option.
struct Lexer {
    argv: std::vec::IntoIter<OsString>,
    /// The options left of the bundle being read.
    short: std::collections::VecDeque<Tok>,
    only_inputs: bool,
}

impl Lexer {
    pub(crate) fn next(&mut self) -> Option<Tok> {
        if let Some(t) = self.short.pop_front() {
            return Some(t);
        }
        let arg = self.argv.next()?;
        let b = arg.as_encoded_bytes();
        if self.only_inputs || b.len() < 2 || b[0] != b'-' {
            return Some(Tok::Input(arg));
        }
        if b == b"--" {
            self.only_inputs = true;
            return self.next();
        }
        // SAFETY: split after ASCII, of an `OsStr`'s bytes
        let os = |b: &[u8]| unsafe { OsStr::from_encoded_bytes_unchecked(b) }.to_owned();
        if let Some(long) = b.strip_prefix(b"--") {
            return Some(match long.iter().position(|&c| c == b'=') {
                Some(i) => Tok::Opt(format!("--{}", String::from_utf8_lossy(&long[..i])), Some(os(&long[i + 1..]))),
                None => Tok::Opt(format!("--{}", String::from_utf8_lossy(long)), None),
            });
        }
        let mut i = 1;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_digit() {
                let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                self.short.push_back(Tok::Opt(format!("-{}", String::from_utf8_lossy(&b[i..i + n])), None));
                i += n;
                continue;
            }
            let name = format!("-{}", if c.is_ascii() { char::from(c).to_string() } else { "?".into() });
            if SHORT_VALUES.contains(&c) {
                let rest = (i + 1 < b.len()).then(|| os(&b[i + 1..]));
                self.short.push_back(Tok::Opt(name, rest));
                break;
            }
            self.short.push_back(Tok::Opt(name, None));
            i += 1;
        }
        self.short.pop_front()
    }

    /// The next argument as it is, an option's value.
    pub(crate) fn value(&mut self) -> Option<OsString> {
        self.argv.next()
    }
}

/// Of the arguments parsed, standard input and output (`-`): where they are taken, and what
/// they are not taken with.
fn stdio_args(a: &mut Args) -> Result<(), String> {
    a.stdin = a.inputs.iter().any(|i| stdio::is(i));
    let to = a.output.as_deref().is_some_and(stdio::is);
    if a.stdin && a.inputs.len() > 1 {
        return Err(match a.decompress {
            true => "- (standard input): an archive alone, of no paths given".into(),
            false => "- (standard input): the only input".into(),
        });
    }
    if a.stdin && a.rm {
        return Err("--rm with - (standard input)".into());
    }
    if a.stdin && a.passphrase && a.pass_from.first().is_none_or(|f| *f == pass::Source::Stdin) {
        // (a line of standard input, or the terminal: neither to be taken where it is the input)
        return Err("-P with - (standard input): the passphrase is read from it (--passphrase-from)".into());
    }
    if a.stdin && !a.decompress && a.renames.iter().any(Option::is_some) {
        return Err("--rename of - (standard input): an archive of a single file has no names".into());
    }
    a.stdout = !a.test && (to || (a.stdin && a.output.is_none()));
    if !a.stdout {
        return Ok(());
    }
    for (bad, what) in [(a.append, a.verb()), (a.auto_dir, "--auto-dir"), (a.each, "--each"), (a.ipc, "--ipc")] {
        if bad {
            return Err(format!("{what} with -o - (standard output)"));
        }
    }
    if !a.decompress {
        if std::io::stdout().is_terminal() && !a.force {
            return Err("not writing an archive to a terminal (-f to)".into());
        }
        // (written as it is made, never written over: nothing to commit)
        a.atomic = true;
    }
    Ok(())
}

/// The paths listed in file `from` (`-`: standard input) for `--files-from`: a line each, its
/// end (`\n`, `\r\n`) left out; or, where `null`, ended by NUL each. Empty ones left out.
fn files_from(from: &OsStr, null: bool) -> Result<Vec<PathBuf>, String> {
    let err = |e: std::io::Error| format!("--files-from {}: {e}", from.display());
    let mut b = Vec::new();
    match stdio::is(Path::new(from)) {
        true => std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut b).map_err(err)?,
        false => std::io::Read::read_to_end(&mut fs::File::open(from).map_err(err)?, &mut b).map_err(err)?,
    };
    let end = if null { 0 } else { b'\n' };
    Ok(b.split(|&c| c == end)
        .map(|l| if null { l } else { l.strip_suffix(b"\r").unwrap_or(l) })
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(os_from(l)))
        .collect())
}

/// An option's value as text (any of it not Unicode replaced), where it is not a path.
fn text(v: std::ffi::OsString) -> String {
    v.to_string_lossy().into_owned()
}

/// Path `p` of an archive as given: its names, encoded as the tree's (`.` left out).
pub(crate) fn path_names(p: &Path) -> Result<Vec<Vec<u8>>, String> {
    // (on Windows, `\` is `/` too)
    let bytes = os_bytes(p.as_os_str());
    let names: Vec<Vec<u8>> =
        bytes.split(|&b| b == b'/').filter(|n| !n.is_empty() && *n != b".").map(name_from).collect();
    if names.is_empty() || names.iter().any(|n| n == b"..") {
        return Err(format!("{}: not a path in an archive", p.display()));
    }
    Ok(names)
}

/// The paths of an archive given, `paths`, with `renames` (each by the number of inputs before
/// it, the archive's too): each path's names (encoded as the tree's), and where it goes.
fn selection(paths: Vec<PathBuf>, renames: Vec<(usize, PathBuf)>) -> Result<extract::Selection, String> {
    let mut out: Vec<(Vec<Vec<u8>>, Option<PathBuf>)> = Vec::new();
    for p in &paths {
        out.push((path_names(p)?, None));
    }
    for (n, to) in renames {
        let Some(k) = n.checked_sub(2) else {
            return Err(format!("--rename {} not after a path of an archive", to.display()));
        };
        if out[k].1.replace(to).is_some() {
            return Err(format!("{}: --rename given twice", paths[k].display()));
        }
    }
    for (i, a) in out.iter().enumerate() {
        if let Some(j) = out.iter().enumerate().position(|(j, b)| j != i && a.0.starts_with(&b.0)) {
            return Err(match a.0 == out[j].0 {
                true => format!("{}: given twice", paths[i].display()),
                false => format!("{}: under {}, given too", paths[i].display(), paths[j].display()),
            });
        }
    }
    Ok(extract::Selection { paths: out })
}
