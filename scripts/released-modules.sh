#!/bin/zsh
# Regenerates crates/aqr-formula/src/released.rs: the SHA-512/256 of the modules of the releases
# that are no longer in crates/aqr-modules/src/known.rs. A decoder accepts the earlier Apply
# instruction (opcode 11, reserved) only for these modules.
#
# Run it at each release, after adding the new tag to `revs`, and commit the result (the modules
# of the release are those of its known.rs, and the next rebuild makes them leave the current one).
# Usage: scripts/released-modules.sh
set -eu
cd "${0:A:h}/.."

# The revisions that were released, oldest first:
# - 9e9476c2 is v0.0.20261028, the first public release (posted on encode.su), which has no tag;
#   its modules were in aqr-modules/ at the root.
revs=(9e9476c2 v0.0.20261031 v0.0.20261101 v0.0.20261105)

out=crates/aqr-formula/src/released.rs
now=$(mktemp)
rows=$(mktemp)
trap 'rm -f $now $rows' EXIT

# known.rs's entries as `HASH NAME@VERSION`
entries() {
    awk '/name: "/ { gsub(/.*name: "|",.*/, ""); n = $0 }
         /version: / { gsub(/.*version: |,.*/, ""); v = $0 }
         /sha512_256: hex\("/ { gsub(/.*hex\("|"\).*/, ""); print $0, n "@" v }'
}

entries < crates/aqr-modules/src/known.rs | cut -d' ' -f1 | sort > $now
for rev in $revs; do
    # (the crate was moved under crates/ after the first release)
    for file in crates/aqr-modules/src/known.rs aqr-modules/src/known.rs; do
        if git cat-file -e "${rev}:${file}" 2>/dev/null; then
            git show "${rev}:${file}" | entries | awk -v r=$rev '{ print $0, r }' >> $rows
            break
        fi
    done || true
done
[[ -s $rows ]] || { echo "released-modules.sh: no module found in $revs" >&2; exit 1; }

{
    cat <<'HEADER'
// Made by scripts/released-modules.sh; run it at each release (after adding the tag to its list).
//
// The SHA-512/256 of the Wasm modules of the releases that crates/aqr-modules/src/known.rs does
// not have any more, as lowercase hexadecimal, sorted, with the module and the first revision
// that had it. The releases are v0.0.20261028 (the first public release, posted on encode.su: it
// has no tag, and is the commit 9e9476c2), v0.0.20261031, v0.0.20261101 and v0.0.20261105.
//
// The decoder accepts an Apply instruction of the reserved opcode 11 (without task limits, the
// memory limit of the whole application) only for these modules, and does not enforce its memory
// limit (the limits were estimates then).

pub const RELEASED: &[&str] = &[
HEADER
    # first revision of each hash, not in the current known.rs, sorted by hash
    sort -s -k1,1 $rows | awk '!seen[$1]++' | while read -r hash name rev; do
        grep -q "^$hash\$" $now || printf '    "%s", // %s (%s)\n' $hash $name $rev
    done
    echo "];"
} > $out
echo "$out: $(grep -c '^    "' $out) modules"
