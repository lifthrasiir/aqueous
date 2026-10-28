//! Values as bytes, for the results of tasks on WebAssembly ([`crate::pool::par_map_apart`]),
//! which give back bytes from memories of their own.

/// A value that is put as bytes and got back from them.
pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    /// The value at the start of `b`, which moves past it; `None` if `b` is not one.
    fn get(b: &mut &[u8]) -> Option<Self>;
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (x, rest) = b.split_at_checked(n)?;
    *b = rest;
    Some(x)
}

macro_rules! ints {
    ($($t:ty)*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }

            fn get(b: &mut &[u8]) -> Option<Self> {
                Some(<$t>::from_le_bytes(take(b, size_of::<$t>())?.try_into().unwrap()))
            }
        }
    )*};
}

ints!(u8 u16 u32 u64 i8 i16 i32 i64);

impl Wire for usize {
    fn put(&self, out: &mut Vec<u8>) {
        (*self as u64).put(out)
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        u64::get(b)?.try_into().ok()
    }
}

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self as u8)
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        match u8::get(b)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Wire for () {
    fn put(&self, _: &mut Vec<u8>) {}

    fn get(_: &mut &[u8]) -> Option<Self> {
        Some(())
    }
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        out.extend_from_slice(self.as_bytes());
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let n = usize::get(b)?;
        String::from_utf8(take(b, n)?.to_vec()).ok()
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for x in self {
            x.put(out);
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let n = usize::get(b)?;
        // (no more than the bytes left could hold: a value is a byte at least, or none)
        let mut v = Vec::with_capacity(n.min(b.len()));
        for _ in 0..n {
            v.push(T::get(b)?);
        }
        Some(v)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.is_some().put(out);
        if let Some(x) = self {
            x.put(out);
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        Some(if bool::get(b)? { Some(T::get(b)?) } else { None })
    }
}

impl<T: Wire, E: Wire> Wire for Result<T, E> {
    fn put(&self, out: &mut Vec<u8>) {
        self.is_ok().put(out);
        match self {
            Ok(x) => x.put(out),
            Err(e) => e.put(out),
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        Some(if bool::get(b)? { Ok(T::get(b)?) } else { Err(E::get(b)?) })
    }
}

impl<T: Wire, const N: usize> Wire for [T; N] {
    fn put(&self, out: &mut Vec<u8>) {
        for x in self {
            x.put(out);
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let v: Vec<T> = (0..N).map(|_| T::get(b)).collect::<Option<_>>()?;
        v.try_into().ok()
    }
}

macro_rules! tuples {
    ($(($($x:ident)*))*) => {$(
        impl<$($x: Wire),*> Wire for ($($x,)*) {
            #[allow(non_snake_case)]
            fn put(&self, out: &mut Vec<u8>) {
                let ($($x,)*) = self;
                $($x.put(out);)*
            }

            fn get(b: &mut &[u8]) -> Option<Self> {
                Some(($($x::get(b)?,)*))
            }
        }
    )*};
}

tuples!((A B) (A B C) (A B C D));

#[cfg(test)]
mod tests {
    use super::*;

    fn round<T: Wire + PartialEq + std::fmt::Debug>(x: T) {
        let mut b = Vec::new();
        x.put(&mut b);
        let mut s = &b[..];
        assert_eq!(T::get(&mut s), Some(x));
        assert!(s.is_empty());
        // cut short, not a value
        if !b.is_empty() {
            assert_eq!(T::get(&mut &b[..b.len() - 1]), None);
        }
    }

    #[test]
    fn values() {
        round(0x1234_5678_9abc_def0u64);
        round(-3i16);
        round(usize::MAX);
        round(vec![(1u8, true), (2, false)]);
        round(Some(String::from("aé")));
        round(Ok::<_, String>((vec![vec![1u32], vec![]], 5usize)));
        round(Err::<u8, _>(String::from("bad")));
        round(vec![[1u8, 2, 3], [4, 5, 6]]);
        round(([-1i16; 64], [7i32; 4]));
        assert_eq!(bool::get(&mut &[2][..]), None);
        // a length beyond the bytes
        assert_eq!(Vec::<u8>::get(&mut &[255, 255, 255, 255, 255, 255, 255, 127][..]), None);
    }
}
