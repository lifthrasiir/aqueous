//! Planes: a surface's pixels as its blocks decode (FORMAT.md, "Planes"), of `C` channels of a
//! sample type ([`Px`]: 8 or 16 bits), and what predicts a pixel from them: the pixels before it
//! ([`Plane::causal`]) and the next smaller mipmap upsampled ([`Plane::upsampled`]).

/// A sample of a plane: 8 or 16 bits, its upsampling summed in a type twice as wide.
pub trait Px: Copy + Default + Send + Sync + 'static {
    type Wide: Copy + Default + Send + Sync + 'static;
    fn get(self) -> i32;
    fn of(v: i32) -> Self;
    /// `3 · a + b`, of two samples.
    fn across(a: Self, b: Self) -> Self::Wide;
    /// `(3 · a + b + 8) >> 4`, of two sums across.
    fn down(a: Self::Wide, b: Self::Wide) -> Self;
}

impl Px for u8 {
    type Wide = u16;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn of(v: i32) -> u8 {
        v as u8
    }
    #[inline(always)]
    fn across(a: u8, b: u8) -> u16 {
        3 * a as u16 + b as u16
    }
    #[inline(always)]
    fn down(a: u16, b: u16) -> u8 {
        ((3 * a + b + 8) >> 4) as u8
    }
}

impl Px for u16 {
    type Wide = u32;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn of(v: i32) -> u16 {
        v as u16
    }
    #[inline(always)]
    fn across(a: u16, b: u16) -> u32 {
        3 * a as u32 + b as u32
    }
    #[inline(always)]
    fn down(a: u32, b: u32) -> u16 {
        ((3 * a + b + 8) >> 4) as u16
    }
}

/// A surface's pixels of `C` channels as its blocks decode, its sides whole blocks.
pub struct Plane<T: Px, const C: usize> {
    pub w: usize,
    pub h: usize,
    px: Vec<[T; C]>,
}

/// Buffers that upsampling works in, kept to be used again: the columns of each x, and the
/// rows summed across.
pub struct Up<W> {
    cols: Vec<(u32, u32)>,
    across: Vec<W>,
}

impl<W: Copy + Default> Up<W> {
    /// Room for upsamplings to `cols` columns of `across` values.
    pub fn with_capacity(cols: usize, across: usize) -> Self {
        Up { cols: Vec::with_capacity(cols), across: Vec::with_capacity(across) }
    }

    /// Bytes that [`Up::with_capacity`] takes of [`aqr_api::stack`].
    pub fn plan(cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        block(8 * cols as u64) + block((size_of::<W>() * across) as u64)
    }
}

/// The values of `across` that upsampling a plane of `h` rows to the rows from `y0`, `h1` of
/// them, takes for each value of a row (as [`Plane::upsampled`] does).
pub fn across_rows(h: usize, y0: usize, h1: usize) -> usize {
    let lo = (y0 / 2).min(h - 1).saturating_sub(1);
    let hi = ((y0 + h1).saturating_sub(1) / 2 + 1).min(h - 1);
    hi + 1 - lo
}

impl<T: Px, const C: usize> Plane<T, C> {
    /// A plane of `w` × `h` pixels in `px` (cleared, and filled within its room).
    pub fn new_in(mut px: Vec<[T; C]>, w: usize, h: usize) -> Self {
        px.clear();
        px.resize(w * h, [T::default(); C]);
        Plane { w, h, px }
    }

    /// The plane's buffer, to be used again.
    pub fn into_px(self) -> Vec<[T; C]> {
        self.px
    }

    /// The plane grown by `h` rows below it (within its room); the first of them.
    pub fn grow(&mut self, h: usize) -> usize {
        let y0 = self.h;
        self.px.resize(self.px.len() + self.w * h, [T::default(); C]);
        self.h += h;
        y0
    }

    #[inline(always)]
    pub fn at(&self, x: usize, y: usize) -> [T; C] {
        self.px[y * self.w + x]
    }

    #[inline(always)]
    pub fn set(&mut self, x: usize, y: usize, v: [T; C]) {
        self.px[y * self.w + x] = v;
    }

    /// The plane of the rows `rows` of a surface `w` pixels wide from this one, of the next
    /// smaller mipmap, bilinearly: each pixel (x, y) of the surface is between four of this one,
    /// (x/2, y/2) and the one beside it towards (x, y) across and down (the sides repeated),
    /// weighted 9, 3, 3 and 1. In `px`, with `up`. The width and the rows are multiples of `Q`
    /// (which the compiler then knows, and vectorizes with).
    #[cfg_attr(target_family = "wasm", target_feature(enable = "simd128"))]
    pub fn upsampled<const Q: usize>(
        &self,
        w: usize,
        rows: std::ops::Range<usize>,
        px: Vec<[T; C]>,
        up: &mut Up<T::Wide>,
    ) -> Self {
        debug_assert!(w.is_multiple_of(Q) && rows.start.is_multiple_of(Q) && rows.end.is_multiple_of(Q));
        let (w, y0, h) = (w / Q * Q, rows.start / Q * Q, rows.len() / Q * Q);
        let near = |v: usize, n: usize| {
            let c = (v / 2).min(n - 1);
            (c, if v.is_multiple_of(2) { c.saturating_sub(1) } else { (c + 1).min(n - 1) })
        };
        // across (3 · nearest + other), of the rows of this one that the rows taken are between,
        // then down (3 · nearest + other): 9, 3, 3, 1
        let lo = (y0 / 2).min(self.h - 1).saturating_sub(1);
        let hi = ((y0 + h).saturating_sub(1) / 2 + 1).min(self.h - 1);
        // (the columns of each x taken once for all rows; then rows of channels flat, which
        // the compiler vectorizes)
        let cols = &mut up.cols;
        cols.clear();
        cols.extend((0..w).map(|x| near(x, self.w)).map(|(c, n)| (c as u32, n as u32)));
        let wc = w * C;
        let across = &mut up.across;
        across.clear();
        across.resize((hi + 1 - lo) * wc, T::Wide::default());
        for (r, row) in across.chunks_exact_mut(wc).enumerate() {
            let src = &self.px[(lo + r) * self.w..][..self.w];
            for (o, &(c, n)) in row.as_chunks_mut::<C>().0.iter_mut().zip(cols.iter()) {
                let (c, n) = (c as usize, n as usize);
                for k in 0..C {
                    o[k] = T::across(src[c][k], src[n][k]);
                }
            }
        }
        let mut px = px;
        px.clear();
        px.resize(w * h, [T::default(); C]);
        for (y, row) in px.chunks_exact_mut(w).enumerate() {
            let (c, n) = near(y0 + y, self.h);
            let (a, b) = (&across[(c - lo) * wc..][..wc], &across[(n - lo) * wc..][..wc]);
            for ((o, &a), &b) in row.as_flattened_mut().iter_mut().zip(a).zip(b) {
                *o = T::down(a, b);
            }
        }
        Plane { w, h, px }
    }

    /// Pixel (x, y) predicted by those before it: by MED (LOCO-I's median edge detector) of the
    /// left, upper and upper left ones, else the one there is of the left and upper ones.
    #[inline(always)]
    pub fn causal(&self, x: usize, y: usize) -> Option<[T; C]> {
        let i = y * self.w + x;
        match (x > 0, y > 0) {
            (true, true) => {
                let (a, b, c) = (self.px[i - 1], self.px[i - self.w], self.px[i - self.w - 1]);
                Some(std::array::from_fn(|k| T::of(med(a[k].get(), b[k].get(), c[k].get()))))
            }
            (true, false) => Some(self.px[i - 1]),
            (false, true) => Some(self.px[i - self.w]),
            (false, false) => None,
        }
    }
}

/// MED: the median of `a`, `b` and `a + b − c` (without branches: pixels are not predictable).
#[inline(always)]
pub fn med(a: i32, b: i32, c: i32) -> i32 {
    a.min(b).max(a.max(b).min(a + b - c))
}
