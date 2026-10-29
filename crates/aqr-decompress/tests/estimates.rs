//! The modules' memory estimates (`Module::memory_usage_in_pages`) against what their
//! applications take as WebAssembly, on the samples of `scripts/module-memory.sh` (in
//! `cache/memory/samples`, made by the script): ignored where they are not there.

use aqr_decompress::{MODULES, Wasm};
use aqr_formula::{PAGE, Usage, aqr};

#[test]
#[ignore = "the samples of scripts/module-memory.sh"]
fn estimates_hold() {
    let dir = format!("{}/cache/memory/samples", env!("CARGO_MANIFEST_DIR"));
    let mut archives: Vec<_> = std::fs::read_dir(&dir)
        .expect("the samples (scripts/module-memory.sh)")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "aqr"))
        .collect();
    archives.sort();
    let (mut checked, mut below) = (0, Vec::new());
    for path in archives {
        let c = std::fs::read(&path).unwrap();
        aqr_decompress::record_applications(true);
        let r = aqr::decode_value(&c, &Wasm);
        let applied = aqr_decompress::record_applications(false);
        assert!(r.is_ok(), "{}: {:?}", path.display(), r.err());
        for a in applied {
            let Some(k) = aqr_modules::KNOWN.iter().find(|k| k.sha512_256 == a.hash) else { continue };
            let Some(m) = MODULES.iter().find(|m| m.name == k.name && m.version == k.version) else { continue };
            let contents: Vec<&[u8]> = a.contents.iter().map(|c| &c[..]).collect();
            let usage = Usage {
                params: &a.params_data,
                resources: &a.resources,
                inputs: &a.inputs,
                outputs: &a.sizes,
                contents: &contents,
                initial_pages: aqr_decompress::initial_pages(&k.read_wasm().unwrap()).unwrap(),
            };
            let Some(pages) = m.memory_usage_in_pages(&usage) else { continue };
            checked += 1;
            if pages * PAGE < a.working {
                below.push(format!("{}: {m}: {} < {}", path.display(), pages * PAGE, a.working));
            }
        }
    }
    assert!(checked > 0, "no application of a module with an estimate");
    assert!(below.is_empty(), "estimates below what was taken:\n{}", below.join("\n"));
}
