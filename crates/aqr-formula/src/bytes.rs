//! The bytes of a stream ([`Bytes`]): owned, shared, sunk or not made yet.

use super::*;

/// The bytes of a [`Stream`]: its own, or shared with whoever else holds them (a file mapped
/// into memory, which the system may drop and read again as it likes, rather than a copy of
/// it). Changing them makes them its own first. Or bytes written to a sink already
/// (aqr-archive's `Sink`), of a stream of it: their length only, slices of the stream being
/// where they are in the sink's stream; reading or changing them is a bug. Or bytes not made
/// yet ([`Bytes::lazy`]): their length, and what makes them ([`Bytes::made`]) where they are
/// needed, each time, so that whoever needs them holds them only as long as it does.
#[derive(Clone)]
pub struct Bytes(Repr);

#[derive(Clone)]
enum Repr {
    Owned(Vec<u8>),
    Shared(Arc<dyn AsRef<[u8]> + Send + Sync>),
    Sunk { stream: usize, len: usize },
    Lazy(Arc<dyn Fn() -> Bytes + Send + Sync>, usize),
}

impl Bytes {
    pub fn shared(data: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Bytes {
        Bytes(Repr::Shared(data))
    }

    /// Stream `stream` of a sink, of `len` bytes so far ([`Bytes`]).
    pub fn sunk(stream: usize, len: usize) -> Bytes {
        Bytes(Repr::Sunk { stream, len })
    }

    /// Bytes of `len`, not made yet: `make` makes them where they are needed ([`Bytes::made`]).
    pub fn lazy(len: usize, make: impl Fn() -> Bytes + Send + Sync + 'static) -> Bytes {
        Bytes(Repr::Lazy(Arc::new(make), len))
    }

    /// Whether the bytes are not made yet ([`Bytes::lazy`]).
    pub fn is_lazy(&self) -> bool {
        matches!(self.0, Repr::Lazy(..))
    }

    /// The bytes, made where they were not yet (each time: they are not kept), else shared.
    pub fn made(&self) -> Bytes {
        match &self.0 {
            Repr::Lazy(make, len) => {
                let b = make();
                assert_eq!(b.len(), *len, "bytes made of the length told");
                b
            }
            _ => self.clone(),
        }
    }

    /// The stream of the sink these bytes are, if they are sunk.
    pub fn sunk_in(&self) -> Option<usize> {
        match self.0 {
            Repr::Sunk { stream, .. } => Some(stream),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Owned(v) => v.len(),
            Repr::Shared(d) => (**d).as_ref().len(),
            &Repr::Sunk { len, .. } | &Repr::Lazy(_, len) => len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the bytes are shared rather than its own: changing them would copy them all.
    /// (Bytes sunk are not.)
    pub fn is_shared(&self) -> bool {
        matches!(self.0, Repr::Shared(_))
    }

    /// The bytes shared from now on, without a copy: a clone shares them rather than copies
    /// them.
    pub fn share(&mut self) {
        if let Repr::Owned(v) = &mut self.0 {
            self.0 = Repr::Shared(Arc::new(std::mem::take(v)));
        }
    }

    /// The bytes as its own, copied first where they are shared.
    pub fn to_mut(&mut self) -> &mut Vec<u8> {
        match &self.0 {
            Repr::Shared(d) => self.0 = Repr::Owned(d.as_ref().as_ref().to_vec()),
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
            Repr::Owned(_) => {}
        }
        let Repr::Owned(v) = &mut self.0 else { unreachable!() };
        v
    }

    /// Its first `len` bytes only.
    pub fn prefix(self, len: usize) -> Bytes {
        match self.0 {
            Repr::Owned(mut v) => {
                v.truncate(len);
                Bytes(Repr::Owned(v))
            }
            Repr::Shared(d) if len >= (*d).as_ref().len() => Bytes(Repr::Shared(d)),
            Repr::Shared(d) => Bytes::shared(Arc::new(Slice(d, 0..len))),
            Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    /// Bytes `range` of it: sharing them where they are shared, else a copy.
    pub fn slice(&self, range: std::ops::Range<usize>) -> Bytes {
        match &self.0 {
            Repr::Owned(v) => Bytes(Repr::Owned(v[range].to_vec())),
            Repr::Shared(d) => {
                assert!(range.start <= range.end && range.end <= (**d).as_ref().len(), "a slice within");
                Bytes::shared(Arc::new(Slice(d.clone(), range)))
            }
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    pub fn into_vec(self) -> Vec<u8> {
        match self.0 {
            Repr::Owned(v) => v,
            Repr::Shared(d) => d.as_ref().as_ref().to_vec(),
            Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }

    pub fn extend_from_slice(&mut self, d: &[u8]) {
        self.to_mut().extend_from_slice(d)
    }

    pub fn truncate(&mut self, len: usize) {
        if len < self.len() {
            self.to_mut().truncate(len)
        }
    }

    pub fn shrink_to_fit(&mut self) {
        if let Repr::Owned(v) = &mut self.0 {
            v.shrink_to_fit()
        }
    }
}

/// Reading bytes sunk ([`Bytes::sunk`]): a bug.
#[cold]
fn sunk(stream: usize) -> ! {
    panic!("the bytes of stream {stream} of the sink are not at hand")
}

/// Reading bytes not made ([`Bytes::lazy`]) as they are: a bug.
#[cold]
fn lazy() -> ! {
    panic!("bytes not made yet read as they are")
}

/// Bytes of bytes shared ([`Bytes::prefix`], [`Bytes::slice`]).
struct Slice(Arc<dyn AsRef<[u8]> + Send + Sync>, std::ops::Range<usize>);

impl AsRef<[u8]> for Slice {
    fn as_ref(&self) -> &[u8] {
        &(*self.0).as_ref()[self.1.clone()]
    }
}

// (bytes shared are only read)
impl std::panic::UnwindSafe for Bytes {}
impl std::panic::RefUnwindSafe for Bytes {}

impl Default for Bytes {
    fn default() -> Bytes {
        Bytes(Repr::Owned(Vec::new()))
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Bytes {
        Bytes(Repr::Owned(v))
    }
}

impl From<Bytes> for Vec<u8> {
    fn from(b: Bytes) -> Vec<u8> {
        b.into_vec()
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Owned(v) => v,
            Repr::Shared(d) => d.as_ref().as_ref(),
            &Repr::Sunk { stream, .. } => sunk(stream),
            Repr::Lazy(..) => lazy(),
        }
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::ops::DerefMut for Bytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.to_mut()
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Bytes) -> bool {
        match (&self.0, &other.0) {
            (Repr::Sunk { stream: a, len: m }, Repr::Sunk { stream: b, len: n }) => (a, m) == (b, n),
            (Repr::Sunk { .. }, _) | (_, Repr::Sunk { .. }) => false,
            (Repr::Lazy(a, m), Repr::Lazy(b, n)) => Arc::ptr_eq(a, b) && m == n,
            (Repr::Lazy(..), _) | (_, Repr::Lazy(..)) => false,
            _ => **self == **other,
        }
    }
}

impl Eq for Bytes {}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        **self == *other
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for Bytes {
    fn eq(&self, other: &&[u8; N]) -> bool {
        **self == other[..]
    }
}

impl PartialEq<Vec<u8>> for Bytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        **self == other[..]
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::Sunk { stream, len } => write!(f, "Sunk {{ stream: {stream}, len: {len} }}"),
            Repr::Lazy(_, len) => write!(f, "Lazy {{ len: {len} }}"),
            _ => fmt::Debug::fmt(&**self, f),
        }
    }
}
