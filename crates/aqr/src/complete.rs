//! `aqr --setup-autocomplete[=SHELL]`, a command alone: the completion of `aqr` and `unaqr`'s
//! options (and of their values: formats, levels, encodings, files and directories) set up for
//! a shell, `zsh`, `bash`, `fish` or `powershell` (by default the one of `$SHELL`; PowerShell
//! on Windows).
//!
//! The script is written where the shell finds it or into this user's data directory
//! (`$XDG_DATA_HOME/aqr`, else `~/.local/share/aqr`; `%LOCALAPPDATA%\Aqueous` on Windows), and
//! a line reading it added to the shell's startup file where it is not there yet:
//!
//! - zsh: `completion.zsh`, read by `${ZDOTDIR:-~}/.zshrc` (`compinit` run first where it was
//!   not);
//! - bash: `completion.bash`, read by `~/.bashrc` (bash 3.2 on: missing words of no option
//!   left to the shell's own file names);
//! - fish: `aqr.fish` and `unaqr.fish` in `${XDG_CONFIG_HOME:-~/.config}/fish/completions`,
//!   which fish reads itself;
//! - PowerShell: `completion.ps1`, read by the profile that `pwsh` (else `powershell`) tells
//!   (`$PROFILE.CurrentUserCurrentHost`).
//!
//! The options are [`OPTIONS`]: every one that `--help` tells of (a test checks it).

use std::path::{Path, PathBuf};

/// What an option takes.
#[derive(Clone, Copy)]
pub enum Value {
    None,
    /// A file (or a path not there yet).
    File,
    /// A file, given after `=` alone.
    OptionalFile,
    Dir,
    /// Something of no completion (a name, a number, a size).
    Text,
    /// One of these.
    Of(fn() -> Vec<String>),
}

pub struct Opt {
    pub short: Option<char>,
    pub long: Option<&'static str>,
    pub value: Value,
    /// Whether it may be given more than once.
    pub many: bool,
    pub help: &'static str,
}

const fn opt(short: Option<char>, long: Option<&'static str>, value: Value, help: &'static str) -> Opt {
    Opt { short, long, value, many: false, help }
}

const fn many(short: Option<char>, long: Option<&'static str>, value: Value, help: &'static str) -> Opt {
    Opt { short, long, value, many: true, help }
}

fn formats() -> Vec<String> {
    std::iter::once("aqr").chain(aqr_pack::Format::names()).map(String::from).collect()
}

fn levels() -> Vec<String> {
    let words = ["low", "mid", "high"].into_iter().map(String::from);
    (0..=9).map(|n| n.to_string()).chain(words).chain((1..=19).map(|n| format!("zstd{n}"))).collect()
}

fn encodings() -> Vec<String> {
    ["auto", "raw", "system", "shift_jis", "euc-kr", "gbk", "gb18030", "big5", "cp437", "utf-8", "windows-1252"]
        .map(String::from)
        .to_vec()
}

fn of(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The shells completed.
pub const SHELLS: [&str; 4] = ["zsh", "bash", "fish", "powershell"];

pub const OPTIONS: &[Opt] = &[
    opt(Some('c'), Some("compress"), Value::None, "compress (the default as aqr)"),
    opt(Some('d'), Some("decompress"), Value::None, "decompress (the default as unaqr)"),
    opt(Some('t'), Some("list"), Value::None, "list the entries of an archive"),
    opt(Some('K'), Some("test"), Value::None, "decode and check an archive, writing nothing"),
    opt(Some('I'), Some("info"), Value::None, "all that an archive records"),
    opt(None, Some("delete"), Value::None, "take the paths out of an archive"),
    opt(None, Some("move"), Value::None, "move the paths of an archive (each to its --rename)"),
    opt(None, Some("compact"), Value::None, "write an archive again of what it holds"),
    opt(None, Some("keep-since"), Value::Text, "with --compact: keep the journal points from SPEC on"),
    opt(None, Some("drop-history"), Value::None, "with --compact: keep no journal point"),
    many(None, Some("rename"), Value::File, "archive the input before (extract or move the path before) at TO"),
    many(Some('x'), Some("exclude"), Value::Text, "leave out the entries of NAME, or of a path"),
    many(None, Some("files-from"), Value::File, "the inputs listed in FILE"),
    opt(None, Some("null"), Value::None, "the lists of --files-from ended by NUL"),
    opt(Some('L'), Some("dereference"), Value::None, "follow links, archiving what they lead to"),
    opt(None, Some("format"), Value::Of(formats), "the format of the archives made"),
    opt(None, Some("each"), Value::None, "every input an archive, each extracted whole"),
    opt(Some('o'), Some("output"), Value::File, "write to OUTPUT (- standard output)"),
    many(Some('C'), Some("directory"), Value::Dir, "the inputs after it in DIR; extract into DIR"),
    opt(None, Some("auto-dir"), Value::None, "extract beside the archive, into a directory where needed"),
    opt(Some('f'), Some("force"), Value::None, "overwrite outputs"),
    opt(None, Some("rm"), Value::None, "remove the inputs once archived, or archives once extracted"),
    opt(Some('p'), Some("preserve"), Value::None, "keep and apply the metadata not commonly kept too"),
    opt(Some('q'), Some("quiet"), Value::None, "log nothing but errors"),
    opt(Some('v'), Some("verbose"), Value::None, "log each file archived"),
    many(None, Some("stats"), Value::OptionalFile, "tell what stands out; FILE.json or FILE.html"),
    opt(None, Some("json"), Value::None, "list as JSON (with -t)"),
    opt(None, Some("ipc"), Value::None, "be driven by a frontend"),
    opt(Some('P'), Some("passphrase"), Value::None, "encrypt with a passphrase asked for"),
    opt(None, Some("passphrase-from"), Value::Of(|| of(&["stdin", "fd:", "env:"])), "the passphrase from SOURCE"),
    opt(Some('l'), Some("level"), Value::Of(levels), "the level of compression"),
    opt(None, Some("depth"), Value::Text, "recompress formats nested at most N deep"),
    opt(Some('B'), Some("batch"), Value::Text, "make about SIZE of files at once"),
    opt(Some('b'), Some("block-size"), Value::Text, "the most bytes of a block files share"),
    opt(Some('S'), Some("segment-size"), Value::Text, "close a segment after about SIZE"),
    opt(Some('D'), Some("dictionaries"), Value::None, "compress blocks alike with dictionaries"),
    opt(Some('A'), Some("archival"), Value::None, "mirror blocks for long-term preservation"),
    opt(None, Some("no-check"), Value::None, "do not decode the archive made to check it"),
    opt(None, Some("atomic"), Value::None, "make the archive whole or not at all"),
    opt(Some('a'), Some("append"), Value::None, "append the inputs to an archive"),
    opt(Some('s'), Some("sync"), Value::None, "append, and take away what the inputs do not have"),
    opt(Some('j'), Some("journal"), Value::Text, "name the state left as a point to return to"),
    opt(None, Some("at"), Value::Text, "the archive as it was at a journal point"),
    opt(None, Some("history"), Value::None, "list the journal points of an archive"),
    opt(Some('E'), Some("encoding"), Value::Of(encodings), "names of no declared encoding"),
    opt(None, Some("normalize"), Value::Of(|| of(&["auto", "nfc", "nfd", "keep"])), "how Unicode names are normalized"),
    opt(
        None,
        Some("bad-names"),
        Value::Of(|| of(&["auto", "keep", "replace", "fail"])),
        "names not written as they are",
    ),
    opt(None, Some("links"), Value::Of(|| of(&["inside", "all"])), "which links are made"),
    opt(None, Some("wasm"), Value::None, "run every module as WebAssembly"),
    opt(None, Some("no-sandbox"), Value::None, "run the Wasm modules in this process"),
    opt(Some('T'), Some("threads"), Value::Text, "threads to use"),
    opt(Some('M'), Some("memory"), Value::Of(|| of(&["avail", "0"])), "about the most heap to take at once"),
    opt(None, Some("application-bound"), Value::Of(|| of(&["unlimited"])), "the most memory a module may need"),
    opt(Some('h'), Some("help"), Value::None, "show the help"),
    opt(Some('V'), Some("version"), Value::None, "show the version"),
    opt(None, Some("setup-autocomplete"), Value::Of(|| of(&SHELLS)), "set up this completion for SHELL, alone"),
];

/// The options' names as given: `-x`, `--long`; the levels `-0` to `-9` too.
pub fn names() -> Vec<String> {
    let mut out = Vec::new();
    for o in OPTIONS {
        out.extend(o.short.map(|c| format!("-{c}")));
        out.extend(o.long.map(|l| format!("--{l}")));
    }
    out.extend((0..=9).map(|n| format!("-{n}")));
    out
}

/// A string in single quotes for POSIX shells (and zsh, fish).
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The script for zsh: `_arguments` of every option, the files after them.
pub fn zsh() -> String {
    // (in `_arguments`'s specs: no `[]:` in the help, escaped where they are)
    let help = |h: &str| h.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]").replace(':', "\\:");
    let mut specs = Vec::new();
    for o in OPTIONS {
        let action = match o.value {
            Value::None => String::new(),
            Value::File | Value::OptionalFile => ":file:_files".into(),
            Value::Dir => ":directory:_files -/".into(),
            Value::Text => ":value: ".into(),
            Value::Of(f) => format!(":value:({})", f().join(" ")),
        };
        let star = if o.many { "*" } else { "" };
        if let Some(c) = o.short {
            let arg = if matches!(o.value, Value::None) { "" } else { "+" };
            specs.push(format!("{star}-{c}{arg}[{}]{action}", help(o.help)));
        }
        if let Some(l) = o.long {
            let (arg, action) = match o.value {
                Value::None => ("", action),
                Value::OptionalFile => ("=-", "::file:_files".into()),
                _ => ("=", action),
            };
            specs.push(format!("{star}--{l}{arg}[{}]{action}", help(o.help)));
        }
    }
    specs.extend((0..=9).map(|n| format!("-{n}[level {n}]")));
    specs.push("*:file:_files".into());
    let specs: Vec<String> = specs.iter().map(|s| format!("    {} \\\n", quoted(s))).collect();
    format!(
        "# The completion of aqr and unaqr for zsh (aqr --setup-autocomplete=zsh).\n\
         (( $+functions[compdef] )) || {{ autoload -Uz compinit && compinit }}\n\
         _aqr() {{\n  _arguments -s -S \\\n{}    && return 0\n}}\n\
         compdef _aqr aqr unaqr\n",
        specs.concat()
    )
}

/// The script for bash (3.2 on): options, their values, files left to the shell.
pub fn bash() -> String {
    let mut cases = String::new();
    for o in OPTIONS {
        let words = match o.value {
            Value::None | Value::OptionalFile => continue,
            Value::File => "COMPREPLY=(); return 0".to_string(),
            Value::Dir => "COMPREPLY=($(compgen -d -- \"$cur\")); return 0".to_string(),
            Value::Text => "COMPREPLY=(); return 0".to_string(),
            Value::Of(f) => format!("COMPREPLY=($(compgen -W {} -- \"$cur\")); return 0", quoted(&f().join(" "))),
        };
        let names: Vec<String> =
            o.short.map(|c| format!("-{c}")).into_iter().chain(o.long.map(|l| format!("--{l}"))).collect();
        cases += &format!("    {}) {words} ;;\n", names.join("|"));
    }
    format!(
        "# The completion of aqr and unaqr for bash (aqr --setup-autocomplete=bash).\n\
         _aqr() {{\n\
         \x20 local cur=\"${{COMP_WORDS[COMP_CWORD]}}\" opt=\"${{COMP_WORDS[COMP_CWORD-1]}}\"\n\
         \x20 # (--opt=value: split at = by the shell, or not)\n\
         \x20 if [ \"$cur\" = \"=\" ]; then cur=\"\"\n\
         \x20 elif [ \"$opt\" = \"=\" ]; then opt=\"${{COMP_WORDS[COMP_CWORD-2]}}\"\n\
         \x20 elif [[ \"$cur\" == --*=* ]]; then opt=\"${{cur%%=*}}\"; cur=\"${{cur#*=}}\"\n\
         \x20 fi\n\
         \x20 case \"$opt\" in\n{cases}\
         \x20 esac\n\
         \x20 if [[ \"$cur\" == -* ]]; then\n\
         \x20   COMPREPLY=($(compgen -W {} -- \"$cur\"))\n\
         \x20 else\n\
         \x20   COMPREPLY=()\n\
         \x20 fi\n\
         }}\n\
         complete -o default -F _aqr aqr unaqr\n",
        quoted(&names().join(" "))
    )
}

/// The script for fish, of command `cmd` (unaqr's wrapping aqr's).
pub fn fish(cmd: &str) -> String {
    let mut s = format!("# The completion of {cmd} for fish (aqr --setup-autocomplete=fish).\n");
    if cmd != "aqr" {
        return s + &format!("complete -c {cmd} -w aqr\n");
    }
    for o in OPTIONS {
        let mut line = "complete -c aqr".to_string();
        if let Some(c) = o.short {
            line += &format!(" -s {c}");
        }
        if let Some(l) = o.long {
            line += &format!(" -l {l}");
        }
        line += &match o.value {
            Value::None | Value::OptionalFile => String::new(),
            Value::File => " -r -F".into(),
            Value::Dir => " -x -a '(__fish_complete_directories (commandline -ct))'".into(),
            Value::Text => " -x".into(),
            Value::Of(f) => format!(" -x -a {}", quoted(&f().join(" "))),
        };
        line += &format!(" -d {}\n", quoted(o.help));
        s += &line;
    }
    for n in 0..=9 {
        s += &format!("complete -c aqr -o {n} -d 'level {n}'\n");
    }
    s
}

/// The script for PowerShell: options and their values; files left to PowerShell where none.
pub fn powershell() -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut opts = Vec::new();
    let mut values = Vec::new();
    for o in OPTIONS {
        let names: Vec<String> =
            o.short.map(|c| format!("-{c}")).into_iter().chain(o.long.map(|l| format!("--{l}"))).collect();
        for n in &names {
            opts.push(format!("    @({}, {})", q(n), q(o.help)));
            match o.value {
                Value::Of(f) => values.push(format!(
                    "  $values[{}] = @({})",
                    q(n),
                    f().iter().map(|v| q(v)).collect::<Vec<_>>().join(", ")
                )),
                Value::Dir => values.push(format!("  $values[{}] = 'dir'", q(n))),
                Value::File | Value::Text => values.push(format!("  $values[{}] = $null", q(n))),
                Value::None | Value::OptionalFile => {}
            }
        }
    }
    for n in 0..=9 {
        opts.push(format!("    @('-{n}', 'level {n}')"));
    }
    format!(
        "# The completion of aqr and unaqr for PowerShell (aqr --setup-autocomplete=powershell).\n\
         Register-ArgumentCompleter -Native -CommandName aqr, unaqr -ScriptBlock {{\n\
         \x20 param($wordToComplete, $commandAst, $cursorPosition)\n\
         \x20 $opts = @(\n{}\n  )\n\
         \x20 $values = [hashtable]::new([StringComparer]::Ordinal)\n{}\n\
         \x20 $word = \"$wordToComplete\"\n\
         \x20 $before = @($commandAst.CommandElements | Where-Object {{ $_.Extent.EndOffset -lt $cursorPosition }})\n\
         \x20 $prev = if ($before.Count -gt 1) {{ $before[-1].ToString() }} else {{ '' }}\n\
         \x20 $prefix = ''\n\
         \x20 if ($word -match '^(--[^=]+)=(.*)$') {{ $prev = $Matches[1]; $prefix = $prev + '='; $word = $Matches[2] }}\n\
         \x20 if ($values.ContainsKey($prev)) {{\n\
         \x20   $v = $values[$prev]\n\
         \x20   if ($v -is [array]) {{\n\
         \x20     $v | Where-Object {{ $_ -like \"$word*\" }} | ForEach-Object {{\n\
         \x20       [System.Management.Automation.CompletionResult]::new($prefix + $_, $_, 'ParameterValue', $_)\n\
         \x20     }}\n\
         \x20   }} elseif ($v -eq 'dir') {{\n\
         \x20     Get-ChildItem -Directory -Path \"$word*\" -ErrorAction SilentlyContinue | ForEach-Object {{\n\
         \x20       [System.Management.Automation.CompletionResult]::new($prefix + $_.Name, $_.Name, 'ProviderContainer', $_.FullName)\n\
         \x20     }}\n\
         \x20   }}\n\
         \x20   return\n\
         \x20 }}\n\
         \x20 if ($word -like '-*') {{\n\
         \x20   $opts | Where-Object {{ $_[0] -clike \"$word*\" }} | ForEach-Object {{\n\
         \x20     [System.Management.Automation.CompletionResult]::new($_[0], $_[0], 'ParameterName', $_[1])\n\
         \x20   }}\n\
         \x20 }}\n\
         }}\n",
        opts.join(",\n"),
        values.join("\n")
    )
}

/// The script of `shell`'s completion.
pub fn script(shell: &str) -> String {
    match shell {
        "zsh" => zsh(),
        "bash" => bash(),
        "fish" => fish("aqr"),
        _ => powershell(),
    }
}

/// The shell to set up where none is given: `$SHELL`'s, or PowerShell on Windows.
fn default_shell() -> Result<String, String> {
    if cfg!(windows) {
        return Ok("powershell".into());
    }
    let sh = std::env::var_os("SHELL").ok_or("--setup-autocomplete: $SHELL not set: give the shell (=zsh, ...)")?;
    let name = Path::new(&sh).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match SHELLS.contains(&&*name) {
        true => Ok(name),
        false => Err(format!("--setup-autocomplete: {name} ($SHELL) is not completed ({})", SHELLS.join(", "))),
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::home_dir().ok_or("--setup-autocomplete: no home directory".into())
}

/// The directory of this user's data of aqr.
fn data_dir() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").ok_or("--setup-autocomplete: %LOCALAPPDATA% not set")?;
        return Ok(PathBuf::from(local).join("Aqueous"));
    }
    Ok(match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("aqr"),
        None => home()?.join(".local/share/aqr"),
    })
}

/// Writes `text` to `path`, its directories made.
fn write(path: &Path, text: &str) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(err)?;
    }
    std::fs::write(path, text).map_err(err)
}

/// Adds `line` to startup file `rc` where no line there names `script` yet: whether it did.
fn add_line(rc: &Path, script: &Path, line: &str) -> Result<bool, String> {
    let err = |e: std::io::Error| format!("{}: {e}", rc.display());
    let had = match std::fs::read_to_string(rc) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(err(e)),
    };
    if had.lines().any(|l| l.contains(&*script.to_string_lossy())) {
        return Ok(false);
    }
    let sep = if had.is_empty() || had.ends_with('\n') { "" } else { "\n" };
    if let Some(d) = rc.parent() {
        std::fs::create_dir_all(d).map_err(err)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(rc).map_err(err)?;
    writeln!(f, "{sep}{line}").map_err(err)?;
    Ok(true)
}

/// The profile of PowerShell's, as `pwsh` (else `powershell`) tells it.
fn powershell_profile() -> Result<PathBuf, String> {
    for exe in ["pwsh", "powershell"] {
        let out = std::process::Command::new(exe)
            .args(["-NoProfile", "-NoLogo", "-Command", "$PROFILE.CurrentUserCurrentHost"])
            .output();
        if let Ok(o) = out
            && o.status.success()
        {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if !p.is_empty() {
                return Ok(p.into());
            }
        }
    }
    Err("--setup-autocomplete=powershell: neither pwsh nor powershell tells its profile".into())
}

/// Sets up the completion for `shell` (`None`: [`default_shell`]), telling on standard error
/// what it did.
pub fn setup(shell: Option<&str>) -> Result<(), String> {
    let shell = match shell {
        Some(s) if SHELLS.contains(&s) => s.to_owned(),
        Some(s) => return Err(format!("--setup-autocomplete: not a shell completed ({}): {s}", SHELLS.join(", "))),
        None => default_shell()?,
    };
    let tell = |what: String| eprintln!("aqr: {what}");
    let posix_line = |p: &Path| {
        let q = quoted(&p.to_string_lossy());
        format!("[ -r {q} ] && . {q}  # aqr --setup-autocomplete")
    };
    let (script, rc, line) = match &*shell {
        "fish" => {
            let dir = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => home()?.join(".config"),
            }
            .join("fish/completions");
            for cmd in ["aqr", "unaqr"] {
                let p = dir.join(format!("{cmd}.fish"));
                write(&p, &fish(cmd))?;
                tell(format!("wrote {}", p.display()));
            }
            tell("fish reads it in new shells".into());
            return Ok(());
        }
        "zsh" => {
            let p = data_dir()?.join("completion.zsh");
            let rc = match std::env::var_os("ZDOTDIR").filter(|d| !d.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => home()?,
            }
            .join(".zshrc");
            let line = posix_line(&p);
            (p, rc, line)
        }
        "bash" => {
            let p = data_dir()?.join("completion.bash");
            let line = posix_line(&p);
            (p, home()?.join(".bashrc"), line)
        }
        _ => {
            let p = data_dir()?.join("completion.ps1");
            let line = format!(". '{}'  # aqr --setup-autocomplete", p.to_string_lossy().replace('\'', "''"));
            (p, powershell_profile()?, line)
        }
    };
    write(&script, &self::script(&shell))?;
    tell(format!("wrote {}", script.display()));
    match add_line(&rc, &script, &line)? {
        true => tell(format!("added a line reading it to {} (for new shells)", rc.display())),
        false => tell(format!("{} reads it already", rc.display())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every option `--help` tells of is completed.
    #[test]
    fn every_option() {
        let names = names();
        let missing: Vec<String> = crate::tests::help_options().into_iter().filter(|o| !names.contains(o)).collect();
        assert!(missing.is_empty(), "not completed: {missing:?}");
    }

    /// PowerShell loads the script (its hash tables ignore case, as `-b` and `-B` differ) and
    /// completes long options and values. (Windows PowerShell never calls a native completer
    /// for a word of one dash and a letter, so short options cannot be tested.)
    #[test]
    fn powershell_completes() {
        let Some(exe) = ["pwsh", "powershell"].into_iter().find(|e| {
            std::process::Command::new(e)
                .args(["-NoProfile", "-Command", "1"])
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            return;
        };
        let path = std::env::temp_dir().join(format!("aqr-complete-ps-{}.ps1", std::process::id()));
        std::fs::write(&path, powershell()).unwrap();
        let probe =
            |line: &str| format!("(TabExpansion2 '{line}' {}).CompletionMatches.CompletionText -join ' '", line.len());
        let cmd = format!(
            ". '{}'; {}; {}; {}; {}",
            path.display(),
            probe("aqr --for"),
            probe("aqr --format tar.x"),
            probe("aqr --normalize="),
            probe("aqr --b"),
        );
        let o =
            std::process::Command::new(exe).args(["-NoProfile", "-NonInteractive", "-Command", &cmd]).output().unwrap();
        _ = std::fs::remove_file(&path);
        let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(err.trim().is_empty(), "{err}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[..3],
            ["--format --force", "tar.xz", "--normalize=auto --normalize=nfc --normalize=nfd --normalize=keep"],
            "{out}"
        );
        assert!(lines[3] == "--batch --block-size --bad-names", "{out}");
    }
}
