// coefdump: read DCT coefficients with libjpeg-turbo.
//   coefdump -b N file.jpg      : time N in-memory jpeg_read_coefficients passes (Huffman decode only)
//   coefdump file.jpg out.bin   : dump coefficients
// Dump format (little endian): u32 ncomp; per comp: u32 bw,bh,hs,vs; u16 q[64] (natural order);
// then per comp: i16 coef[bh][bw][64] (natural order).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <jpeglib.h>

static unsigned char *slurp(const char *p, size_t *n) {
  FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(1); }
  fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET);
  unsigned char *b = malloc(*n); if (fread(b, 1, *n, f) != *n) exit(1); fclose(f); return b;
}

static jvirt_barray_ptr *readc(struct jpeg_decompress_struct *d, unsigned char *b, size_t n) {
  jpeg_mem_src(d, b, n);
  jpeg_read_header(d, TRUE);
  return jpeg_read_coefficients(d);
}

int main(int argc, char **argv) {
  struct jpeg_decompress_struct d; struct jpeg_error_mgr e;
  d.err = jpeg_std_error(&e); jpeg_create_decompress(&d);
  if (argc == 4 && !strcmp(argv[1], "-b")) {
    int iters = atoi(argv[2]); size_t n; unsigned char *b = slurp(argv[3], &n);
    struct timespec t0, t1; clock_gettime(CLOCK_MONOTONIC, &t0);
    for (int i = 0; i < iters; i++) { readc(&d, b, n); jpeg_finish_decompress(&d); }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double s = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) * 1e-9;
    printf("%zu %.6f %.1f\n", n, s / iters, n * (double)iters / s / 1e6);
    return 0;
  }
  if (argc != 3) { fprintf(stderr, "usage\n"); return 2; }
  size_t n; unsigned char *b = slurp(argv[1], &n);
  jvirt_barray_ptr *ca = readc(&d, b, n);
  FILE *o = fopen(argv[2], "wb"); uint32_t nc = d.num_components; fwrite(&nc, 4, 1, o);
  for (int c = 0; c < d.num_components; c++) {
    jpeg_component_info *ci = &d.comp_info[c];
    uint32_t h[4] = { ci->width_in_blocks, ci->height_in_blocks, ci->h_samp_factor, ci->v_samp_factor };
    fwrite(h, 4, 4, o);
    uint16_t q[64]; for (int k = 0; k < 64; k++) q[k] = ci->quant_table->quantval[k];
    fwrite(q, 2, 64, o);
  }
  for (int c = 0; c < d.num_components; c++) {
    jpeg_component_info *ci = &d.comp_info[c];
    for (JDIMENSION y = 0; y < ci->height_in_blocks; y++) {
      JBLOCKARRAY row = (*d.mem->access_virt_barray)((j_common_ptr)&d, ca[c], y, 1, FALSE);
      fwrite(row[0], sizeof(JBLOCK), ci->width_in_blocks, o);
    }
  }
  fclose(o); jpeg_finish_decompress(&d); return 0;
}
