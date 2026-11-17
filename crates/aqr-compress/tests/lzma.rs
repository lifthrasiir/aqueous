//! `-Z lzma`: the streams of a batch compressed solid by LZMA, in groups, every slice an output
//! of a group, the files given back. (A test binary of its own: it sets the process's `-Z`.)

mod common;

/// [`common::files_given_back`] in groups of 64 KiB: so that the outputs of several groups are
/// taken by several files, out of their order, some of them more than once.
#[test]
fn files_given_back() {
    aqr_common::zopt::apply(&["lzma", "lzma:group=64K"]).unwrap();
    let r = common::files_given_back();
    let modules: Vec<&str> = r.files.iter().flat_map(|f| f.modules.iter().map(|m| m.name)).collect();
    assert!(modules.contains(&"lzma-decode"), "{modules:?}");
}
