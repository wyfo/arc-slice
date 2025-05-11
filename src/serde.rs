use alloc::{string::String, vec::Vec};
use core::{cmp, fmt, marker::PhantomData};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    layout::{AnyBufferLayout, Layout, LayoutMut},
    macros::is,
    utils::{try_transmute, try_transmute_slice, UnwrapChecked},
    ArcSlice, ArcSliceMut, ArcStr,
};

const MAX_DESERIALIZE_SIZE_HINT: usize = 1 << 12;

fn serialize_slice<T, S>(slice: &[T], serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize + Send + Sync + 'static,
    S: Serializer,
{
    match try_transmute_slice(slice) {
        Some(b) => serializer.serialize_bytes(b),
        None => serializer.collect_seq(slice),
    }
}

impl<T: Serialize + Send + Sync + 'static, L: Layout> Serialize for ArcSlice<T, L> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_slice(self, serializer)
    }
}

impl<T: Serialize + Send + Sync + 'static, L: LayoutMut, const UNIQUE: bool> Serialize
    for ArcSliceMut<T, L, UNIQUE>
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_slice(self, serializer)
    }
}

impl<L: Layout> Serialize for ArcStr<L> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self)
    }
}

struct ArcSliceVisitor<T, S>(PhantomData<(T, S)>);

impl<
        'de,
        T: Deserialize<'de> + Copy + Send + Sync + 'static,
        S: for<'a> From<&'a [T]> + From<Vec<T>>,
    > de::Visitor<'de> for ArcSliceVisitor<T, S>
{
    type Value = S;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(if is!(T, u8) { "bytes" } else { "sequence" })
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match try_transmute_slice(v) {
            Some(s) => Ok(s.into()),
            None => Err(de::Error::invalid_type(de::Unexpected::Bytes(v), &self)),
        }
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match try_transmute::<_, Vec<T>>(v) {
            Ok(v) => Ok(v.into()),
            Err(v) => Err(de::Error::invalid_type(de::Unexpected::Bytes(&v), &self)),
        }
    }

    fn visit_seq<V>(self, mut seq: V) -> Result<Self::Value, V::Error>
    where
        V: de::SeqAccess<'de>,
    {
        if is!(T, u8) {
            return Err(de::Error::invalid_type(de::Unexpected::Seq, &self));
        }
        let capa = cmp::min(seq.size_hint().unwrap_or(0), MAX_DESERIALIZE_SIZE_HINT);
        let mut values: Vec<T> = Vec::with_capacity(capa);
        while let Some(value) = seq.next_element()? {
            values.push(value);
        }
        Ok(values.into())
    }
}

fn deserialize_arc_slice<'de, T, S, D>(deserializer: D) -> Result<S, D::Error>
where
    T: Deserialize<'de> + Copy + Send + Sync + 'static,
    S: for<'a> From<&'a [T]> + From<Vec<T>>,
    D: Deserializer<'de>,
{
    let visitor = ArcSliceVisitor(PhantomData);
    if is!(T, u8) {
        deserializer.deserialize_byte_buf(visitor)
    } else {
        deserializer.deserialize_seq(visitor)
    }
}

impl<'de, T: Deserialize<'de> + Copy + Send + Sync + 'static, L: AnyBufferLayout> Deserialize<'de>
    for ArcSlice<T, L>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_arc_slice(deserializer)
    }
}

impl<'de, T: Deserialize<'de> + Copy + Send + Sync + 'static, L: LayoutMut + AnyBufferLayout>
    Deserialize<'de> for ArcSliceMut<T, L>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_arc_slice(deserializer)
    }
}

struct ArcStrVisitor<L: Layout>(PhantomData<L>);

impl<L: AnyBufferLayout> de::Visitor<'_> for ArcStrVisitor<L> {
    type Value = ArcStr<L>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(v.parse().unwrap_checked())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(v.into())
    }
}

impl<'de, L: AnyBufferLayout> Deserialize<'de> for ArcStr<L> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ArcStrVisitor(PhantomData))
    }
}

#[cfg(feature = "inlined")]
const _: () = {
    use crate::inlined::{SmallArcBytes, SmallArcStr};
    impl<L: Layout> Serialize for SmallArcBytes<L> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.serialize_bytes(self)
        }
    }

    impl<L: Layout> Serialize for SmallArcStr<L> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.serialize_str(self)
        }
    }

    struct SmallArcBytesVisitor<L>(PhantomData<L>);

    impl<L: AnyBufferLayout> de::Visitor<'_> for SmallArcBytesVisitor<L> {
        type Value = SmallArcBytes<L>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "bytes")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(SmallArcBytes::new(v))
        }

        fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(v.into())
        }
    }

    impl<'de, L: AnyBufferLayout> Deserialize<'de> for SmallArcBytes<L> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_byte_buf(SmallArcBytesVisitor(PhantomData))
        }
    }

    struct SmallArcStrVisitor<L>(PhantomData<L>);

    impl<L: AnyBufferLayout> de::Visitor<'_> for SmallArcStrVisitor<L> {
        type Value = SmallArcStr<L>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "string")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(v.parse().unwrap_checked())
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(v.into())
        }
    }

    impl<'de, L: AnyBufferLayout> Deserialize<'de> for SmallArcStr<L> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_byte_buf(SmallArcStrVisitor(PhantomData))
        }
    }
};
