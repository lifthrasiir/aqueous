#!/bin/sh
# Pinned checkouts of the tools compared by the benchmark, into the workspace's third_party/.
set -eu
cd "$(dirname "$0")/../.."
mkdir -p third_party
fetch() {
    [ -d "third_party/$1" ] || git clone -q "$2" "third_party/$1"
    git -C "third_party/$1" checkout -q "$3"
}
fetch preflate https://github.com/deus-libri/preflate.git 609eefaa96ac6c51d7b1a3fb29e0ed94d0f3623e
fetch preflate-rs https://github.com/microsoft/preflate-rs.git 3bcd33441849a8a25d2b26077128f4e3a88d071b
