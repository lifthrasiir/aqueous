# aqr-l10n

The messages of the frontends (aqr-win, aqr-win-install, aqr-mac, and aqr-ipc's for them) in
the languages of `messages/*.toml`: English (`en.toml`, the source and the fallback) and Korean
(`ko.toml`). `aqr`'s own messages (its errors, which the windows show as they come) stay
English.

```rust
aqr_l10n::init(&aqr_l10n::system_languages());   // aqr-mac: NSBundle's preferred localizations
aqr_l10n::common::intact("a.aqr")                 // "a.aqr is intact", "a.aqr에 이상이 없습니다"
aqr_l10n::types::ZIP.resource()                   // its id in Windows' string table
```

```sh
cargo run -p aqr-l10n -- lproj DIR   # DIR/{en,ko}.lproj/*.strings (bundle.sh)
cargo run -p aqr-l10n -- rc          # Windows' string table, as aqr-win embeds it
```

## The catalogs

TOML, a section of messages for each part (`common`, `units`, `types`, `shell`, `win`, `install`, `mac`,
`services`), a key for each message:

```toml
[common]
intact = "{name} is intact"
tested = { one = "{n} file, {bytes}, …", other = "{n} files, {bytes}, …" }
```

- `{name}` is a placeholder, `{{` and `}}` braces. A table of CLDR's plural categories (`zero`,
  `one`, `two`, `few`, `many`, `other`; `other` always) is a plural, its form chosen by `{n}`
  and the language's rule (`plural` in `src/lib.rs`).
- `build.rs` makes of each section a module, of each message a constant (`Msg`, `INTACT`) and a
  function (`intact(name: impl Display) -> String`; `&'static str` where it has no placeholders;
  `n: u64` first for a plural), the placeholders of the English text being its parameters in
  their order: a message called with the wrong placeholders does not compile.
- A translation may leave out messages (English is shown) and placeholders, not add any; a
  key or a placeholder that English does not have fails the build, as does a lone brace.
- The language: `AQR_LANG` where it is one of the catalogs', else the first of the user's
  languages that one of the catalogs matches (the tag, a catalog's that it begins with, or one of
  the same language: `ko` of `ko-KR`), else English. On Windows those are the preferred UI
  languages (`GetUserPreferredUILanguages`, the list Windows' resources are loaded by); in the
  macOS app, `NSBundle`'s preferred localizations of the catalogs' (the app's own language in
  System Settings, else the system's).

## What the systems read themselves

Some texts are shown by the system and not by the program: Explorer's verbs and the types'
names, Finder's services and type names. Both systems localise those from resources, and both
are made from the catalogs here, the program's own messages being compiled into the binary as
tables.

- Windows: a string table (`STRINGTABLE` in a `LANGUAGE` block for each language) of the sections
  `types` and `shell`, which aqr-win's build script writes into `OUT_DIR` for `aqr-win.rc`. The
  registry names them by indirect strings, `@C:\…\aqr-win.exe,-ID` (`MUIVerb` of the verbs,
  `FriendlyTypeName` of the classes, `ApplicationDescription` of the capabilities), which Explorer
  loads in the user's language of the moment: changing it needs no registering again. The script
  is ASCII, the rest escaped (`L"\xb85c"`), as rc.exe, windres and llvm-rc all read that alike
  (llvm-rc's preprocessing loses `#pragma code_page`). These messages take no placeholders.
  The id of each is the one `messages/ids.toml` gives it (`types` 1100-1199, `shell` 1200-1299), and
  never changes for a released message: Windows keeps what it read of an id (`HKCU\Software\Classes\
  Local Settings\MuiCache`, which survives Explorer's restart), so a moved id shows another
  message's text until that is cleared. A new message takes the next id of its range; one taken away
  leaves its id retired in a comment there. The build fails for a message with no id, an id out of
  its range or used twice, and an id that is no message's. aqr-win's `--register` clears what Windows
  has cached of its own strings.
- macOS: `LANG.lproj/InfoPlist.strings` (the types' names: `UTTypeDescription` and
  `CFBundleTypeName` are looked up by their English text) and `ServicesMenu.strings` (the
  services' `NSMenuItem`), written into the bundle's `Resources` by `bundle.sh`. A test of aqr-mac
  checks that every such string of `Info.plist` is in the catalogs.
- Windows' strings could all be loaded from the string table (`LoadStringW`), and macOS' from a
  `Localizable.strings`; the tables in the binary serve both systems alike, keep the
  placeholders checked by the compiler, and are tested on any system.

## Adding a language

1. `messages/TAG.toml` (a BCP 47 tag, as Apple's `.lproj` take them: `ja`, `zh-Hans`), of the
   messages it translates.
2. Its plural rule in `plural` (`src/lib.rs`; a test checks that every catalog has one), and its
   Windows language in `RC_LANGUAGES` (`build.rs`: primary and sub language, of `winnt.h`).

## Status

Left: VERSIONINFO's description (English only); numbers and sizes in the language's own forms
(`aqr_ipc::size` writes `1.5 MB` everywhere); `aqr`'s messages; Settings' Apps' name and
installer's title stay "Aqueous".
