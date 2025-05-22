use alloc::{string::String, vec::Vec};
use core::{cmp, fmt, marker::PhantomData, ops::Deref};

use serde::{de, de::Error, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    buffer::{Deserializable, Slice},
    layout::{ArcLayout, Layout, LayoutMut},
    utils::try_as_bytes,
    ArcSlice, ArcSliceMut,
};

const MAX_DESERIALIZE_SIZE: usize = 1 << 12;

fn serialize_slice<S: Serialize + Slice + ?Sized, Ser: Serializer>(
    slice: &S,
    serializer: Ser,
) -> Result<Ser::Ok, Ser::Error> {
    match try_as_bytes(slice) {
        Some(b) => serializer.serialize_bytes(b),
        None => slice.serialize(serializer),
    }
}

impl<S: Serialize + Slice + ?Sized, L: Layout> Serialize for ArcSlice<S, L> {
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: Serializer,
    {
        serialize_slice(self.deref(), serializer)
    }
}

impl<S: Serialize + Slice + ?Sized, L: LayoutMut, const UNIQUE: bool> Serialize
    for ArcSliceMut<S, L, UNIQUE>
{
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: Serializer,
    {
        serialize_slice(self.deref(), serializer)
    }
}

trait IntoArcSlice<S: Slice + ?Sized> {
    fn from_slice(slice: &S) -> Self;
    fn from_vec(vec: S::Vec) -> Self;
    fn from_arc_slice_mut(slice: ArcSliceMut<S, ArcLayout<false, false>>) -> Self;
}

impl<S: Slice + ?Sized, L: Layout> IntoArcSlice<S> for ArcSlice<S, L> {
    fn from_slice(slice: &S) -> Self {
        ArcSlice::new_bytes(slice)
    }
    fn from_vec(vec: S::Vec) -> Self {
        ArcSlice::new_byte_vec(vec)
    }
    fn from_arc_slice_mut(slice: ArcSliceMut<S, ArcLayout<false, false>>) -> Self {
        slice.freeze()
    }
}

impl<S: Slice + ?Sized, L: LayoutMut> IntoArcSlice<S> for ArcSliceMut<S, L> {
    fn from_slice(slice: &S) -> Self {
        ArcSliceMut::new_bytes(slice)
    }
    fn from_vec(vec: S::Vec) -> Self {
        ArcSliceMut::new_byte_vec(vec)
    }
    fn from_arc_slice_mut(slice: ArcSliceMut<S, ArcLayout<false, false>>) -> Self {
        slice.with_layout()
    }
}

struct ArcSliceVisitor<S: Slice + ?Sized, T>(PhantomData<(S::Vec, T)>);

impl<'de, S: Slice + Deserializable + ?Sized, T: IntoArcSlice<S>> de::Visitor<'de>
    for ArcSliceVisitor<S, T>
where
    S::Item: for<'a> Deserialize<'a>,
    S::TryFromSliceError: fmt::Display,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(S::expected())
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match S::deserialize_from_str(v) {
            Some(s) => Ok(T::from_slice(s)),
            None => Err(de::Error::invalid_type(de::Unexpected::Str(v), &self)),
        }
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match S::deserialize_from_string(v) {
            Ok(s) => Ok(T::from_vec(s)),
            Err(v) => Err(de::Error::invalid_type(de::Unexpected::Str(&v), &self)),
        }
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match S::deserialize_from_bytes(v) {
            Some(slice) => Ok(T::from_slice(slice)),
            None => Err(de::Error::invalid_type(de::Unexpected::Bytes(v), &self)),
        }
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match S::deserialize_from_byte_buf(v) {
            Ok(vec) => Ok(T::from_vec(vec)),
            Err(v) => Err(de::Error::invalid_type(de::Unexpected::Bytes(&v), &self)),
        }
    }

    fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
    where
        V: de::SeqAccess<'de>,
    {
        if !S::try_deserialize_from_seq() {
            return Err(de::Error::invalid_type(de::Unexpected::Seq, &self));
        }
        let capacity = cmp::min(
            seq.size_hint().unwrap_or(0),
            MAX_DESERIALIZE_SIZE / core::mem::size_of::<S::Item>(),
        );
        let mut slice = ArcSliceMut::<[S::Item], ArcLayout<false, false>>::with_capacity(capacity);
        while let Some(item) = seq.next_element()? {
            slice.push(item);
        }
        Ok(T::from_arc_slice_mut(
            ArcSliceMut::try_from_arc_slice_mut(slice)
                .map_err(|(err, _)| de::Error::custom(err))?,
        ))
    }
}

impl<'de, S: Slice + Deserializable + ?Sized, L: Layout> Deserialize<'de> for ArcSlice<S, L>
where
    S::Item: for<'a> Deserialize<'a>,
    S::TryFromSliceError: fmt::Display,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        S::deserialize(deserializer, ArcSliceVisitor::<S, Self>(PhantomData))
    }
}

impl<'de, S: Slice + Deserializable + ?Sized, L: LayoutMut> Deserialize<'de> for ArcSliceMut<S, L>
where
    S::Item: for<'a> Deserialize<'a>,
    S::TryFromSliceError: fmt::Display,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        S::deserialize(deserializer, ArcSliceVisitor::<S, Self>(PhantomData))
    }
}

#[cfg(feature = "inlined")]
const _: () = {
    use crate::inlined::SmallArcSlice;

    impl<S: Serialize + Slice<Item = u8> + ?Sized, L: Layout> Serialize for SmallArcSlice<S, L> {
        fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
        where
            Ser: Serializer,
        {
            serialize_slice(self.deref(), serializer)
        }
    }

    impl<S: Slice<Item = u8> + ?Sized, L: Layout> IntoArcSlice<S> for SmallArcSlice<S, L> {
        fn from_slice(slice: &S) -> Self {
            SmallArcSlice::new(slice)
        }
        fn from_vec(vec: S::Vec) -> Self {
            ArcSlice::<S, L>::from_vec(vec).into()
        }
        fn from_arc_slice_mut(slice: ArcSliceMut<S, ArcLayout<false, false>>) -> Self {
            slice.freeze().into()
        }
    }

    impl<'de, S: Slice<Item = u8> + Deserializable + ?Sized, L: LayoutMut> Deserialize<'de>
        for SmallArcSlice<S, L>
    where
        S::TryFromSliceError: fmt::Display,
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            S::deserialize(deserializer, ArcSliceVisitor::<S, Self>(PhantomData))
        }
    }
};
