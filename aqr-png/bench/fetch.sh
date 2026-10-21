#!/bin/sh
# Pinned checkouts and builds of the tools compared by the benchmark, into the workspace's third_party/.
# Needs cmake (brew install cmake).
set -eu
cd "$(dirname "$0")/../.."
mkdir -p third_party
fetch() {
    [ -d "third_party/$1" ] || git clone -q "$2" "third_party/$1"
    git -C "third_party/$1" checkout -q "$3"
}
fetch preflate-rs https://github.com/microsoft/preflate-rs.git 3bcd33441849a8a25d2b26077128f4e3a88d071b
fetch precomp-cpp https://github.com/schnaader/precomp-cpp.git a11b077
fetch paq8px https://github.com/hxim/paq8px.git c84f576

# precomp: its Unix paths are keyed on __unix / __linux, which clang on macOS lacks.
(cd third_party/precomp-cpp &&
    cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
        "-DCMAKE_C_FLAGS=-D__unix -D__linux -Wno-implicit-function-declaration" \
        "-DCMAKE_CXX_FLAGS=-D__unix -D__linux" >/dev/null &&
    cmake --build build -j >/dev/null)

# paq8px: drop GCC-only flags, and disambiguate two std::min calls for clang.
(cd third_party/paq8px &&
    sed -i.orig -e '/-floop-strip-mine/d' -e '/-fgcse-sm/d' CMakeLists.txt &&
    sed -i.orig 's/= min(warmup, MAX_RECORD_LENGTH)/= std::min<size_t>(warmup, MAX_RECORD_LENGTH)/' src/model/SimilarityModel.cpp &&
    sed -i.orig 's/= min(warmup, MAX_MATCH_DISTANCE)/= std::min<size_t>(warmup, MAX_MATCH_DISTANCE)/' src/model/SimilarityModelPair.cpp &&
    cmake -S . -B build-cm -DCMAKE_BUILD_TYPE=Release -DNATIVECPU=ON >/dev/null &&
    cmake --build build-cm -j >/dev/null)
