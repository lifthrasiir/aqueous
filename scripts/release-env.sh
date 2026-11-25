#!/bin/sh
# The environment of release.yml's build jobs, as lines for $GITHUB_ENV:
#
#   scripts/release-env.sh >> "$GITHUB_ENV"
#
# - STEM, what the assets are named by (aqr-STEM.exe, ...): of a tag v0.0.YYYYMMDD the date
#   alone (aqr-20261031.exe), as the first releases' assets were named by hand before the
#   workflow; of any other tag its version without the `v` (v0.1.0 -> aqr-0.1.0.exe,
#   v0.1.0-rc.1 -> aqr-0.1.0-rc.1.exe); not for a tag, the commit's short hash.
# - AQR_VERSION, for a tag only: the binaries' version (crates/aqr-version).
#
# A tag is taken only as vMAJOR.MINOR.PATCH, with a prerelease `-[0-9A-Za-z.]+` at most (no
# build metadata: `+` in an asset's name and URL is trouble), so that a mistyped tag fails here,
# before anything is built. lifthrasiir/homebrew-aqueous's update.sh names the assets the same
# way: change both together.
set -eu
if [ "${GITHUB_REF_TYPE:-}" != tag ]; then
    echo "STEM=$(git rev-parse --short HEAD)"
    exit 0
fi
tag=$GITHUB_REF_NAME
if ! printf '%s\n' "$tag" | grep -Eqx 'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.]+)?'; then
    echo "$0: not a tag of a release (vMAJOR.MINOR.PATCH[-PRERELEASE]): $tag" >&2
    exit 1
fi
if printf '%s\n' "$tag" | grep -Eqx 'v0\.0\.[0-9]{8}'; then
    echo "STEM=${tag#v0.0.}"
else
    echo "STEM=${tag#v}"
fi
echo "AQR_VERSION=$tag"
