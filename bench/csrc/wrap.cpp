// C ABI over preflate's whole-buffer API. Buffers are malloc'ed; free with pf_free.
#include <cstdlib>
#include <cstring>
#include <vector>
#include "preflate.h"

static unsigned char* dup(const std::vector<unsigned char>& v, size_t* n) {
    *n = v.size();
    unsigned char* p = (unsigned char*)malloc(v.size() ? v.size() : 1);
    if (p && !v.empty()) memcpy(p, v.data(), v.size());
    return p;
}

extern "C" int pf_decode(const unsigned char* raw, size_t raw_len, unsigned char** plain,
                         size_t* plain_len, unsigned char** diff, size_t* diff_len) {
    try {
        std::vector<unsigned char> in(raw, raw + raw_len), out, d;
        if (!preflate_decode(out, d, in)) return 0;
        *plain = dup(out, plain_len);
        *diff = dup(d, diff_len);
        return 1;
    } catch (...) {
        return 0;
    }
}

extern "C" int pf_reencode(const unsigned char* diff, size_t diff_len, const unsigned char* plain,
                           size_t plain_len, unsigned char** raw, size_t* raw_len) {
    try {
        std::vector<unsigned char> d(diff, diff + diff_len), p(plain, plain + plain_len), out;
        if (!preflate_reencode(out, d, p)) return 0;
        *raw = dup(out, raw_len);
        return 1;
    } catch (...) {
        return 0;
    }
}

extern "C" void pf_free(unsigned char* p) { free(p); }
