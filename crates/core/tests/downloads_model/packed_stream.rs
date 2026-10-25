//! Streaming reader for the existing packed wire format. The encoder and old
//! Value-tree decoder remain independent proof oracles in the parent module.
use super::{DICT, Reader, World, token};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IntoDeserializer, Visitor};
use std::fmt;

#[derive(Debug)]
struct Error(String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Error {}
impl de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}
impl From<super::ModelError> for Error {
    fn from(value: super::ModelError) -> Self {
        Self(value.to_string())
    }
}
fn fail(message: &str) -> Error {
    Error(message.into())
}
struct Decoder<'de> {
    reader: Reader<'de>,
    depth: usize,
}
impl<'de> Decoder<'de> {
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        self.depth += 1;
        let result = if self.depth > 128 {
            Err(fail("packed nesting exceeds model contract"))
        } else {
            f(self)
        };
        self.depth -= 1;
        result
    }
    fn text(&mut self, tag: u8) -> Result<&'de str, Error> {
        match tag {
            4 => DICT
                .get(
                    usize::try_from(self.reader.var()?)
                        .map_err(|_| fail("dictionary index overflow"))?,
                )
                .copied()
                .ok_or_else(|| fail("unknown dictionary token")),
            5 => {
                let n = self.reader.len()?;
                let end = self.reader.at + n;
                let value = std::str::from_utf8(&self.reader.bytes[self.reader.at..end])
                    .map_err(|_| fail("invalid packed UTF8"))?;
                self.reader.at = end;
                if token(value).is_some() {
                    return Err(fail("noncanonical literal dictionary string"));
                }
                Ok(value)
            }
            _ => Err(fail("non-string packed key")),
        }
    }
    fn key(&mut self) -> Result<&'de str, Error> {
        self.nested(|this| {
            let tag = this.reader.byte()?;
            this.text(tag)
        })
    }
}
struct Sequence<'a, 'de> {
    decoder: &'a mut Decoder<'de>,
    left: usize,
}
impl<'de> de::SeqAccess<'de> for &mut Sequence<'_, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        let value = self.decoder.nested(|de| seed.deserialize(de))?;
        self.left -= 1;
        Ok(Some(value))
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}
struct Map<'a, 'de> {
    decoder: &'a mut Decoder<'de>,
    left: usize,
    previous: Option<&'de str>,
    pending: bool,
}
impl<'de> de::MapAccess<'de> for &mut Map<'_, 'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.pending {
            return Err(fail("packed map value not consumed"));
        }
        if self.left == 0 {
            return Ok(None);
        }
        let key = self.decoder.key()?;
        if self.previous.is_some_and(|p| p >= key) {
            return Err(fail("noncanonical packed key order"));
        }
        self.previous = Some(key);
        self.pending = true;
        seed.deserialize(de::value::BorrowedStrDeserializer::<Error>::new(key))
            .map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        if !self.pending {
            return Err(fail("packed map key missing"));
        }
        let value = self.decoder.nested(|de| seed.deserialize(de))?;
        self.left -= 1;
        self.pending = false;
        Ok(value)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}
struct Enum<'a, 'de> {
    decoder: &'a mut Decoder<'de>,
    key: &'de str,
    consumed: &'a mut bool,
}
impl<'a, 'de> de::EnumAccess<'de> for Enum<'a, 'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let key = seed.deserialize(de::value::BorrowedStrDeserializer::<Error>::new(self.key))?;
        Ok((key, self))
    }
}
impl<'de> de::VariantAccess<'de> for Enum<'_, 'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        *self.consumed = true;
        self.decoder.nested(|de| <()>::deserialize(de))
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        *self.consumed = true;
        self.decoder.nested(|de| seed.deserialize(de))
    }
    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        *self.consumed = true;
        self.decoder
            .nested(|de| de::Deserializer::deserialize_tuple(de, len, visitor))
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        *self.consumed = true;
        self.decoder
            .nested(|de| de::Deserializer::deserialize_struct(de, "", fields, visitor))
    }
}
impl<'de> de::Deserializer<'de> for &mut Decoder<'de> {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.depth > 128 {
            return Err(fail("packed nesting exceeds model contract"));
        }
        match self.reader.byte()? {
            0 => visitor.visit_unit(),
            1 => visitor.visit_bool(false),
            2 => visitor.visit_bool(true),
            3 => visitor.visit_u64(self.reader.var()?),
            tag @ (4 | 5) => visitor.visit_borrowed_str(self.text(tag)?),
            6 => {
                let n = self.reader.len()?;
                let mut access = Sequence {
                    decoder: self,
                    left: n,
                };
                let result = visitor.visit_seq(&mut access)?;
                if access.left != 0 {
                    return Err(fail("packed sequence not fully consumed"));
                }
                Ok(result)
            }
            7 => {
                let n = self.reader.len()?;
                let mut access = Map {
                    decoder: self,
                    left: n,
                    previous: None,
                    pending: false,
                };
                let result = visitor.visit_map(&mut access)?;
                if access.left != 0 || access.pending {
                    return Err(fail("packed map not fully consumed"));
                }
                Ok(result)
            }
            8 => {
                let n = self.reader.var()?;
                if n == 0 || n > (1u64 << 63) {
                    return Err(fail("invalid packed negative integer"));
                }
                visitor.visit_i64(if n == 1u64 << 63 {
                    i64::MIN
                } else {
                    -(n as i64)
                })
            }
            _ => Err(fail("unknown packed tag")),
        }
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.reader.bytes.get(self.reader.at) == Some(&0) {
            self.reader.at += 1;
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.reader.byte()? {
            tag @ (4 | 5) => visitor.visit_enum(self.text(tag)?.into_deserializer()),
            7 => {
                if self.reader.len()? != 1 {
                    return Err(fail("packed enum requires one variant"));
                }
                let key = self.key()?;
                let mut consumed = false;
                let value = visitor.visit_enum(Enum {
                    decoder: self,
                    key,
                    consumed: &mut consumed,
                })?;
                if !consumed {
                    return Err(fail("packed enum value not consumed"));
                }
                Ok(value)
            }
            _ => Err(fail("packed enum requires string or map")),
        }
    }
    fn deserialize_f32<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(fail("packed model does not support floating numbers"))
    }
    fn deserialize_f64<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(fail("packed model does not support floating numbers"))
    }
    // IgnoredAny still walks every byte using the same canonical validators.
    serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any }
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, super::ModelError> {
    let mut decoder = Decoder {
        reader: Reader { bytes, at: 0 },
        depth: 0,
    };
    let result = (|| {
        if decoder.reader.byte()? != 1 {
            return Err(fail("unknown packed version"));
        }
        let value = T::deserialize(&mut decoder)?;
        if decoder.reader.at != bytes.len() {
            return Err(fail("noncanonical or trailing packed bytes"));
        }
        Ok(value)
    })();
    result.map_err(|error: Error| super::err(&error.to_string()))
}

pub(super) fn world(bytes: &[u8]) -> Result<World, super::ModelError> {
    decode(bytes)
}
