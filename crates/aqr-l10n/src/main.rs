//! Writes what the system reads of the catalogs, for the scripts that bundle the frontends.
//!
//! usage: aqr-l10n lproj DIR   (the `.strings` of each language: DIR/LANG.lproj/*.strings, for
//!                              aqr-mac's bundle.sh into Contents/Resources)
//!        aqr-l10n rc          (Windows' string table on standard output, as aqr-win embeds it)

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["lproj", dir] => {
            for (folder, file, contents) in aqr_l10n::LPROJ {
                let d = std::path::Path::new(dir).join(folder);
                if let Err(e) = std::fs::create_dir_all(&d).and_then(|()| std::fs::write(d.join(file), contents)) {
                    eprintln!("aqr-l10n: {}: {e}", d.join(file).display());
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        ["rc"] => {
            print!("{}", aqr_l10n::RC);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: aqr-l10n lproj DIR | rc");
            ExitCode::from(2)
        }
    }
}
