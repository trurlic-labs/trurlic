//! The kinds a field can hold: each states its JSON Schema and turns a
//! JSON value into its type, and each bounded kind holds its limit as a
//! field.

use std::marker::PhantomData;

use serde_json::{Map, Value, json};

use super::{
    Args, At, Fault, Fields, InvalidArgument, Kind, declared_schema, input_schema, parse,
    read_fields, schema,
};

/// A string of at most `max_bytes`. Its characters are the store's to judge:
/// every text an op writes is held to `store::model::text`, a name to the
/// name rule and an anchor to its own, so a refusal names the rule it
/// breaks.
pub(crate) struct Text {
    pub max_bytes: usize,
}

impl Kind for Text {
    type Out = String;

    fn schema(&self) -> Map<String, Value> {
        schema([
            ("type", "string".into()),
            ("maxLength", self.max_bytes.into()),
        ])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<String, InvalidArgument> {
        let text = value
            .as_str()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("a string")))?;
        if text.len() > self.max_bytes {
            return Err(InvalidArgument::new(
                at,
                Fault::TooLong {
                    bytes: text.len(),
                    max_bytes: self.max_bytes,
                },
            ));
        }
        Ok(text.to_owned())
    }
}

/// One of `values`, named by its string.
pub(crate) struct Choice<T: 'static> {
    pub values: &'static [(&'static str, T)],
}

impl<T: Copy> Kind for Choice<T> {
    type Out = T;

    fn schema(&self) -> Map<String, Value> {
        let names: Vec<&str> = self.values.iter().map(|(name, _)| *name).collect();
        schema([("type", "string".into()), ("enum", names.into())])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<T, InvalidArgument> {
        let name = value
            .as_str()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("a string")))?;
        self.values
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|&(_, choice)| choice)
            .ok_or_else(|| {
                let names = self.values.iter().map(|(name, _)| *name).collect();
                InvalidArgument::new(at, Fault::NotOneOf(names))
            })
    }
}

/// An array of at most `max_items` items, each of kind `item`.
pub(crate) struct List<K> {
    pub item: K,
    pub max_items: usize,
}

impl<K: Kind> Kind for List<K> {
    type Out = Vec<K::Out>;

    fn schema(&self) -> Map<String, Value> {
        schema([
            ("type", "array".into()),
            ("items", self.item.schema().into()),
            ("maxItems", self.max_items.into()),
        ])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<Vec<K::Out>, InvalidArgument> {
        let items = value
            .as_array()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("an array")))?;
        if items.len() > self.max_items {
            return Err(InvalidArgument::new(
                at,
                Fault::TooMany {
                    items: items.len(),
                    max_items: self.max_items,
                },
            ));
        }
        items
            .iter()
            .enumerate()
            .map(|(index, item)| self.item.parse(item, At::Item(&at, index)))
            .collect()
    }
}

/// A nested object whose fields `A` declares.
pub(crate) struct Object<A>(PhantomData<fn() -> A>);

impl<A> Object<A> {
    pub(crate) const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<A: Args> Kind for Object<A> {
    type Out = A;

    fn schema(&self) -> Map<String, Value> {
        input_schema::<A>()
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<A, InvalidArgument> {
        let object = value
            .as_object()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("an object")))?;
        parse(object, at)
    }
}

/// An object whose `tag` field names one of `variants`, which declares the
/// object's other fields.
pub(crate) struct Union<T: 'static> {
    pub tag: &'static str,
    pub variants: &'static [Variant<T>],
}

/// One shape of a [`Union`]: its tag value and the fields it declares.
pub(crate) struct Variant<T> {
    pub name: &'static str,
    pub declare: fn(&mut Fields<'_, '_>) -> T,
}

impl<T> Kind for Union<T> {
    type Out = T;

    fn schema(&self) -> Map<String, Value> {
        let shapes: Vec<Value> = self
            .variants
            .iter()
            .map(|variant| {
                let tag = json!({ "type": "string", "const": variant.name });
                Value::Object(declared_schema(variant.declare, Some((self.tag, tag))))
            })
            .collect();
        schema([("type", "object".into()), ("oneOf", shapes.into())])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<T, InvalidArgument> {
        let object = value
            .as_object()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("an object")))?;
        let tag_at = At::Field(&at, self.tag);
        let name = object
            .get(self.tag)
            .ok_or_else(|| InvalidArgument::new(tag_at, Fault::Missing))?
            .as_str()
            .ok_or_else(|| InvalidArgument::new(tag_at, Fault::Expected("a string")))?;
        let variant = self
            .variants
            .iter()
            .find(|variant| variant.name == name)
            .ok_or_else(|| {
                let names = self.variants.iter().map(|variant| variant.name).collect();
                InvalidArgument::new(tag_at, Fault::NotOneOf(names))
            })?;
        read_fields(object, at, &[self.tag], variant.declare)
    }
}
