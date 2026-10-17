//! `StableHash`: hashing values through a byte encoding that is the same on every platform, build
//! and engine version.
//!
//! `core::hash::Hash` is unsuitable for anything persisted: integers go out in host byte order,
//! `usize` has the host's width, and the standard library may change how its types write
//! themselves. [`StableHash`] fixes the encoding instead:
//!
//! | value                          | bytes                                                        |
//! |--------------------------------|--------------------------------------------------------------|
//! | `u8`..`u128`, `i8`..`i128`     | little-endian, at their width                                 |
//! | `usize`, `isize`               | as `u64` / `i64`                                              |
//! | `bool`                         | one byte, 0 or 1                                              |
//! | `char`                         | its scalar value as `u32`                                     |
//! | `f32`, `f64`                   | the IEEE bits (`to_bits`): `-0.0 != 0.0`, NaN payloads count  |
//! | `str`, `[T]`, `[T; N]`, `Vec`  | length as `u64`, then the elements                            |
//! | tuples, structs                | the fields in order, nothing between them                     |
//! | `Option`, `Result`, enums      | discriminant as `i64`, then the variant's fields              |
//! | `&T`, `Box<T>`                 | as `T`                                                        |
//!
//! `Option` uses `None = 0, Some = 1`; `Result` uses `Ok = 0, Err = 1`. `#[derive(StableHash)]`
//! requires every enum variant to have an explicit discriminant (`Idle = 0`, or with
//! `#[repr(u8)]` for variants with fields), so reordering variants cannot change a hash. Field
//! names are not hashed: renaming is free, reordering or retyping fields changes the hash.
//!
//! **The encoding is part of cook keys** (and of anything else derived from these hashes): a
//! change re-cooks every asset at best and silently aliases stale data at worst. It must never
//! change silently; `tests/stable_hash.rs` pins it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::{Crc32, Guid, Xxh3, Xxh64};

/// A byte sink for [`StableHash`]. Only [`write`](Self::write) is required; the provided methods
/// define the encoding and must not be overridden with different bytes.
pub trait StableHasher {
    fn write(&mut self, bytes: &[u8]);

    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.write(&[v]);
    }

    #[inline]
    fn write_u16(&mut self, v: u16) {
        self.write(&v.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.write(&v.to_le_bytes());
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }

    #[inline]
    fn write_u128(&mut self, v: u128) {
        self.write(&v.to_le_bytes());
    }

    /// A length or count prefix, as `u64`.
    #[inline]
    fn write_len(&mut self, len: usize) {
        self.write_u64(len as u64);
    }

    /// Length-prefixed UTF-8.
    #[inline]
    fn write_str(&mut self, s: &str) {
        self.write_len(s.len());
        self.write(s.as_bytes());
    }

    /// An enum discriminant, as `i64`.
    #[inline]
    fn write_discriminant(&mut self, d: i64) {
        self.write_u64(d as u64);
    }
}

impl<H: StableHasher + ?Sized> StableHasher for &mut H {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        (**self).write(bytes);
    }
}

impl StableHasher for Xxh3 {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

impl StableHasher for Xxh64 {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

impl StableHasher for Crc32 {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

/// The encoded bytes themselves, e.g. to inspect or store a key.
impl StableHasher for Vec<u8> {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A value with a platform- and build-independent byte encoding; see the [module docs](self).
/// Derive it with `#[derive(StableHash)]`.
pub trait StableHash {
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H);

    /// The elements of a slice, without its length (the caller writes that). Integers override
    /// this to write the whole slice at once; the bytes are the same.
    #[inline]
    fn stable_hash_slice<H: StableHasher + ?Sized>(data: &[Self], h: &mut H)
    where
        Self: Sized,
    {
        for x in data {
            x.stable_hash(h);
        }
    }
}

/// XXH3-64 of `value`'s stable encoding.
pub fn stable_hash_64<T: StableHash + ?Sized>(value: &T) -> u64 {
    let mut h = Xxh3::new();
    value.stable_hash(&mut h);
    h.digest()
}

/// XXH3-128 of `value`'s stable encoding: for keys where 64 bits could collide (cook keys).
pub fn stable_hash_128<T: StableHash + ?Sized>(value: &T) -> u128 {
    let mut h = Xxh3::new();
    value.stable_hash(&mut h);
    h.digest128()
}

/// The whole slice as bytes when that is the encoding: single bytes anywhere, wider integers on
/// little-endian hosts.
macro_rules! int_impls {
    ($($t:ty => $write:ident as $as:ty),* $(,)?) => {$(
        impl StableHash for $t {
            #[inline]
            fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
                h.$write(*self as $as);
            }

            #[inline]
            fn stable_hash_slice<H: StableHasher + ?Sized>(data: &[Self], h: &mut H) {
                if cfg!(target_endian = "little") || size_of::<$t>() == 1 {
                    // SAFETY: integers have no padding and every byte is initialized; the bytes
                    // are each element's little-endian encoding, in order.
                    let bytes = unsafe {
                        core::slice::from_raw_parts(data.as_ptr().cast::<u8>(), size_of_val(data))
                    };
                    h.write(bytes);
                } else {
                    for x in data {
                        x.stable_hash(h);
                    }
                }
            }
        }
    )*};
}

int_impls! {
    u8 => write_u8 as u8,
    u16 => write_u16 as u16,
    u32 => write_u32 as u32,
    u64 => write_u64 as u64,
    u128 => write_u128 as u128,
    i8 => write_u8 as u8,
    i16 => write_u16 as u16,
    i32 => write_u32 as u32,
    i64 => write_u64 as u64,
    i128 => write_u128 as u128,
}

// Not via `int_impls!`: the slice fast path would write the host width.
impl StableHash for usize {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u64(*self as u64);
    }
}

impl StableHash for isize {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u64(*self as i64 as u64);
    }
}

impl StableHash for bool {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u8(*self as u8);
    }
}

impl StableHash for char {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u32(*self as u32);
    }
}

impl StableHash for f32 {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u32(self.to_bits());
    }
}

impl StableHash for f64 {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u64(self.to_bits());
    }
}

impl StableHash for str {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_str(self);
    }
}

impl StableHash for String {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_str(self);
    }
}

impl<T: StableHash> StableHash for [T] {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_len(self.len());
        T::stable_hash_slice(self, h);
    }
}

/// Length-prefixed like a slice, so `[T; N]`, `&[T]` and `Vec<T>` with equal elements agree.
impl<T: StableHash, const N: usize> StableHash for [T; N] {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        self.as_slice().stable_hash(h);
    }
}

impl<T: StableHash> StableHash for Vec<T> {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        self.as_slice().stable_hash(h);
    }
}

impl<T: StableHash + ?Sized> StableHash for &T {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        (**self).stable_hash(h);
    }
}

impl<T: StableHash + ?Sized> StableHash for &mut T {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        (**self).stable_hash(h);
    }
}

impl<T: StableHash + ?Sized> StableHash for Box<T> {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        (**self).stable_hash(h);
    }
}

impl<T: StableHash> StableHash for Option<T> {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        match self {
            None => h.write_discriminant(0),
            Some(v) => {
                h.write_discriminant(1);
                v.stable_hash(h);
            }
        }
    }
}

impl<T: StableHash, E: StableHash> StableHash for Result<T, E> {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        match self {
            Ok(v) => {
                h.write_discriminant(0);
                v.stable_hash(h);
            }
            Err(e) => {
                h.write_discriminant(1);
                e.stable_hash(h);
            }
        }
    }
}

/// Writes nothing.
impl<T: ?Sized> StableHash for PhantomData<T> {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, _: &mut H) {}
}

/// The `u128`, little-endian like any other.
impl StableHash for Guid {
    #[inline]
    fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
        h.write_u128(self.as_u128());
    }
}

macro_rules! tuple_impls {
    ($(($($t:ident $i:tt),*))*) => {$(
        impl<$($t: StableHash),*> StableHash for ($($t,)*) {
            #[inline]
            #[allow(unused_variables)]
            fn stable_hash<H: StableHasher + ?Sized>(&self, h: &mut H) {
                $(self.$i.stable_hash(h);)*
            }
        }
    )*};
}

tuple_impls! {
    ()
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, I 7)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, I 7, J 8)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, I 7, J 8, K 9)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, I 7, J 8, K 9, L 10)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, I 7, J 8, K 9, L 10, M 11)
}
