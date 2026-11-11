#!/bin/sh
# Validates `aqr-ld merge` (crates/aqr-ld/src/pemerge.rs): merges trivial native modules
# (demo/modadd, demo/modxor; out[i] = in[i] + 7 and in[i] ^ 0x5a) into a stub executable
# (demo/stub) as sections, and checks that each merged subset carries exactly the modules it was
# given and that they produce the right bytes. Windows x86-64 only.
set -eu
cd "$(dirname "$0")/../.."
cargo build --release -p aqr-ld -p aqr-pemerge-stub
# (the modules must not unwind: panic=abort keeps them free of the exception runtime)
RUSTFLAGS="-Cpanic=abort" cargo build --release -p aqr-pemerge-modadd -p aqr-pemerge-modxor

ld=target/release/aqr-ld.exe
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cp target/release/aqr-pemerge-stub.exe "$dir/stub.exe"
cp target/release/aqr_pemerge_modadd.dll "$dir/modadd.dll"
cp target/release/aqr_pemerge_modxor.dll "$dir/modxor.dll"
printf 'Hello, Aqueous! 0123456789 the quick brown fox jumps.' > "$dir/in"

fail() { echo "pemerge-test: $1" >&2; exit 1; }
# transforms in POSIX: add 7 / xor 0x5a, byte by byte, as hex
check() { # check FILE EXPR  (EXPR applied to each byte value b)
    python - "$dir/in" "$1" "$2" <<'PY' || fail "output $1 wrong"
import sys
i = open(sys.argv[1], 'rb').read()
o = open(sys.argv[2], 'rb').read()
b = 0  # noqa
want = bytes(eval(sys.argv[3]) & 0xff for b in i)
sys.exit(0 if o == want else 1)
PY
}

# before the merge: no modules
[ -z "$("$dir/stub.exe" list)" ] || fail "the bare stub lists modules"
"$dir/stub.exe" run add "$dir/in" "$dir/x" && fail "the bare stub ran a module" || true

# each subset carries exactly its modules
"$ld" merge --stub "$dir/stub.exe" --out "$dir/add.exe" --apply aqr_mod_apply --info - add="$dir/modadd.dll"
[ "$("$dir/add.exe" list)" = "add" ] || fail "{add} lists wrong"
"$dir/add.exe" run add "$dir/in" "$dir/o"; check "$dir/o" "b+7"
"$dir/add.exe" run xor "$dir/in" "$dir/x" && fail "{add} ran xor" || true

"$ld" merge --stub "$dir/stub.exe" --out "$dir/xor.exe" --apply aqr_mod_apply --info - xor="$dir/modxor.dll"
[ "$("$dir/xor.exe" list)" = "xor" ] || fail "{xor} lists wrong"
"$dir/xor.exe" run xor "$dir/in" "$dir/o"; check "$dir/o" "b^0x5a"
"$dir/xor.exe" run add "$dir/in" "$dir/x" && fail "{xor} ran add" || true

"$ld" merge --stub "$dir/stub.exe" --out "$dir/both.exe" --apply aqr_mod_apply --info - add="$dir/modadd.dll" xor="$dir/modxor.dll"
[ "$(printf '%s\n' $("$dir/both.exe" list) | sort | tr '\n' ' ')" = "add xor " ] || fail "{add,xor} lists wrong"
"$dir/both.exe" run add "$dir/in" "$dir/oa"; check "$dir/oa" "b+7"
"$dir/both.exe" run xor "$dir/in" "$dir/ox"; check "$dir/ox" "b^0x5a"

echo "pemerge-test: ok (subsets {}, {add}, {xor}, {add,xor} each carry and run exactly their modules)"
