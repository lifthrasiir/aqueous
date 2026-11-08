//! The memory limits an application declares hold for the modules run natively as they do as
//! WebAssembly: the plan of the application (what a Wasm instance would take at most) is checked
//! against the limits before native code runs, and a plan that cannot be worked out runs the
//! module as WebAssembly, where they are enforced.

#![cfg(feature = "wasm")]

use aqr_decompress::{Native, Wasm, built};
use aqr_formula::aqr::{Failure, Runner};
use aqr_formula::{Module, NO_LIMIT, PAGE};

fn wasm_bytes(m: Module) -> Vec<u8> {
    built(m).expect("built").read_wasm().unwrap()
}

type Outputs = aqr_formula::aqr::Decoded<Vec<Vec<u8>>>;

/// `Native` and `Wasm` apply `m` alike, whatever the limits.
fn both(m: Module, params: &[u8], inputs: &[&[u8]], sizes: &[u64], limit: u64, tasks: &[u64]) -> (Outputs, Outputs) {
    let w = wasm_bytes(m);
    let (n, x) = (Native::default(), Wasm);
    let (n, x) = (n.load(&w).unwrap(), x.load(&w).unwrap());
    (n.apply(params, &[], inputs, sizes, limit, tasks), x.apply(params, &[], inputs, sizes, limit, tasks))
}

fn kind(r: &aqr_formula::aqr::Decoded<Vec<Vec<u8>>>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(Failure::Resource(_)) => "resource",
        Err(Failure::Invalid(_)) => "invalid",
        Err(_) => "other",
    }
}

/// An honest application (the plan as its limit, which aqr-compress declares) passes natively
/// and as Wasm to the same bytes; one declaring less than its memories fails in both as a
/// resource failure, with the same message, before anything is made.
#[test]
fn declared_limit_holds_natively() {
    let m = aqr_bcj::formula::DELTA;
    let data: Vec<u8> = (0..300_000u32).map(|i| ((i * 7) >> 3) as u8).collect();
    let n = data.len() as u64;
    let plan = aqr_decompress::needed_by(m, &[2], &[], &[n], &[n], &Vec::<&[u8]>::new()).unwrap().unwrap();
    assert!(plan.instance > PAGE, "{plan:?}");
    let (nat, wasm) = both(m, &[2], &[&data], &[n], plan.instance, &plan.tasks);
    assert!(nat.is_ok() && nat == wasm, "{:?} {:?}", nat.as_ref().err(), wasm.as_ref().err());
    // (a limit with room, and the earlier Apply's, which none enforces)
    for limit in [plan.instance * 2, NO_LIMIT] {
        let (nat, wasm) = both(m, &[2], &[&data], &[n], limit, &plan.tasks);
        assert!(nat.is_ok() && nat == wasm, "{limit}");
    }
    // (less than the memories imported alone, then than the plan: the Wasm host and native code both refuse)
    for limit in [PAGE, 2 * PAGE, plan.instance / 2] {
        let (nat, wasm) = both(m, &[2], &[&data], &[n], limit, &plan.tasks);
        assert_eq!(kind(&nat), "resource", "{limit}: {nat:?}");
        assert_eq!(nat, wasm, "{limit}");
    }
}

/// A plan reading the inputs' contents is worked out from them when the inputs are at hand;
/// where they make no sense to it, it is not known, and the module runs as WebAssembly: the
/// failure is Wasm's own, the same as for a module run there.
#[test]
fn plan_needing_contents() {
    let m = aqr_lzx::formula::MODULE;
    let none: Vec<&[u8]> = Vec::new();
    // (the plan asks for the records and the index: not worked out without them)
    assert_eq!(aqr_decompress::needed_by(m, &[], &[], &[1, 1, 1, 1], &[10], &none), Err(aqr_decompress::NoPlan));
    let junk: [&[u8]; 4] = [b"d", b"r", b"k", b"i"];
    for limit in [1 << 20, PAGE] {
        let (nat, wasm) = both(m, &[], &junk, &[10], limit, &[]);
        assert!(nat.is_err(), "{limit}");
        assert_eq!(nat, wasm, "{limit}");
    }
}

/// An archive of a flate-set application (a part module whose plan reads its records and index,
/// pulled from the streams natively): declaring its plan decodes natively and as Wasm to the same
/// bytes; declaring less of the instance or of its tasks fails in both as a resource failure.
#[test]
fn archive_declaring_less_fails_alike() {
    use aqr_flate::formula::Derived;
    use aqr_formula::aqr::{self, Encoder, Level};
    use aqr_formula::{Formula, Node};
    let text: Vec<u8> = (0..60_000).flat_map(|i| format!("line {} of {}\n", i % 211, i % 17).into_bytes()).collect();
    let raw = aqr_flate::zlib::compress(&text, aqr_flate::zlib::Params { level: 6, ..Default::default() }).unwrap();
    let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
    let archive = |limit: Option<(u64, Vec<u64>)>| {
        let mut f = Formula::default();
        let (d, r) = (f.push("raw", data.clone()), f.push("flate/record", record.clone()));
        let index = f.push("flate/index", aqr_flate::module::index(&[(data.len(), record.len(), raw.len())]));
        let mut a = aqr_flate::formula::apply(aqr_flate::DEFAULT_PROFILE, d, r, index, raw.len() as u64, Derived::None)
            .unwrap();
        if let Some((m, t)) = limit {
            (a.memory_limit, a.task_limits) = (m, t);
        }
        f.root = Node::Apply(Box::new(a));
        f
    };
    // (the plan, worked out from the contents, as aqr-compress declares it)
    let index = aqr_flate::module::index(&[(data.len(), record.len(), raw.len())]);
    let contents: Vec<&[u8]> = vec![&data, &record, &index];
    let f = archive(None);
    let Node::Apply(a) = &f.root else { unreachable!() };
    let resources: Vec<u64> = a.resources.iter().map(|r| r.len() as u64).collect();
    let lens: Vec<u64> = contents.iter().map(|c| c.len() as u64).collect();
    let plan = aqr_decompress::needed_by(a.module, &a.params, &resources, &lens, &[a.len], &contents).unwrap().unwrap();
    assert_eq!(plan.tasks.len(), 1, "{plan:?}");
    let enc = Encoder::new(Level::Zstd(3)).with_modules(aqr_decompress::wasm_of);
    let decode = |limit: u64, task: u64| {
        let c = enc.encode(&archive(Some((limit, vec![task]))), None).unwrap();
        (aqr::decode(&c, &Native::default()), aqr::decode(&c, &Wasm))
    };
    let (n, w) = decode(plan.instance, plan.tasks[0]);
    assert!(n.as_ref().is_ok_and(|n| *n == raw) && n == w, "{:?} {:?}", n.err(), w.err());
    for (limit, task) in [(plan.instance / 2, plan.tasks[0]), (plan.instance, PAGE), (PAGE, PAGE)] {
        let (n, w) = decode(limit, task);
        assert!(matches!(n, Err(Failure::Resource(_))), "{limit} {task}: {:?}", n.map(drop));
        assert_eq!(n, w, "{limit} {task}");
    }
}
