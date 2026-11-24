//! Tool result specs: one declaration per result type yields both the
//! `outputSchema` `tools/list` advertises and the JSON a call returns, so
//! the two cannot drift.
//!
//! An [`Output`] type declares its fields in [`Output::declare`], each
//! through [`Shown::required`] or [`Shown::optional`] with a [`Show`] kind
//! and an accessor. Walked for the schema, every field is recorded and no
//! accessor runs; walked over a value, each accessor reads the field and
//! the kind writes it. A field cannot be written without being declared,
//! nor declared as one kind and written as another.

use std::marker::PhantomData;

use serde_json::{Map, Value, json};

use super::SchemaWalk;

/// A tool's result, or an object nested in it.
pub(crate) trait Output {
    fn declare(fields: &mut Shown<'_, Self>);
}

/// What a result field holds: its schema and how its value is written.
pub(crate) trait Show {
    type Value;

    /// The field's JSON Schema, without its description.
    fn schema(&self) -> Map<String, Value>;

    fn value(&self, value: &Self::Value) -> Value;
}

/// The fields of one result object, read in the walk that
/// [`output_schema`] or [`to_value`] runs.
pub(crate) struct Shown<'w, T: ?Sized> {
    walk: Walk<'w, T>,
}

enum Walk<'w, T: ?Sized> {
    Schema(&'w mut SchemaWalk),
    Value {
        value: &'w T,
        object: &'w mut Map<String, Value>,
    },
}

impl<T: ?Sized> Shown<'_, T> {
    /// A field every result holds.
    pub(crate) fn required<K: Show>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
        get: impl Fn(&T) -> &K::Value,
    ) {
        match &mut self.walk {
            Walk::Schema(walk) => {
                declare(walk, name, description, kind);
                walk.required.push(name.into());
            }
            Walk::Value { value, object } => {
                object.insert(name.into(), kind.value(get(value)));
            }
        }
    }

    /// A field a result holds when `get` gives it a value.
    pub(crate) fn optional<K: Show>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
        get: impl Fn(&T) -> Option<&K::Value>,
    ) {
        match &mut self.walk {
            Walk::Schema(walk) => declare(walk, name, description, kind),
            Walk::Value { value, object } => {
                if let Some(field) = get(value) {
                    object.insert(name.into(), kind.value(field));
                }
            }
        }
    }
}

fn declare(walk: &mut SchemaWalk, name: &str, description: &str, kind: &impl Show) {
    let mut schema = kind.schema();
    schema.insert("description".into(), description.into());
    walk.properties.insert(name.into(), schema.into());
}

/// The JSON Schema of `O`: an object of its declared fields, closed to any
/// other key.
pub(crate) fn output_schema<O: Output>() -> Map<String, Value> {
    let mut walk = SchemaWalk::default();
    O::declare(&mut Shown {
        walk: Walk::Schema(&mut walk),
    });
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    schema.insert("properties".into(), walk.properties.into());
    if !walk.required.is_empty() {
        schema.insert("required".into(), walk.required.into());
    }
    schema.insert("additionalProperties".into(), false.into());
    schema
}

/// `value` as the JSON object its declaration describes.
pub(crate) fn to_value<O: Output>(value: &O) -> Value {
    let mut object = Map::new();
    O::declare(&mut Shown {
        walk: Walk::Value {
            value,
            object: &mut object,
        },
    });
    Value::Object(object)
}

// -- Kinds ----------------------------------------------------------------

/// A string.
pub(crate) struct Str;

impl Show for Str {
    type Value = String;

    fn schema(&self) -> Map<String, Value> {
        object(json!({ "type": "string" }))
    }

    fn value(&self, value: &String) -> Value {
        value.as_str().into()
    }
}

/// A count.
pub(crate) struct Count;

impl Show for Count {
    type Value = usize;

    fn schema(&self) -> Map<String, Value> {
        object(json!({ "type": "integer", "minimum": 0 }))
    }

    fn value(&self, value: &usize) -> Value {
        (*value).into()
    }
}

/// One of `all`, written by its `name`.
pub(crate) struct Tag<E: 'static> {
    pub all: &'static [E],
    pub name: fn(E) -> &'static str,
}

impl<E: Copy> Show for Tag<E> {
    type Value = E;

    fn schema(&self) -> Map<String, Value> {
        let names: Vec<&str> = self.all.iter().map(|&each| (self.name)(each)).collect();
        object(json!({ "type": "string", "enum": names }))
    }

    fn value(&self, value: &E) -> Value {
        (self.name)(*value).into()
    }
}

/// An array, each item of kind `item`.
pub(crate) struct Items<K> {
    pub item: K,
}

impl<K: Show> Show for Items<K> {
    type Value = Vec<K::Value>;

    fn schema(&self) -> Map<String, Value> {
        object(json!({ "type": "array", "items": self.item.schema() }))
    }

    fn value(&self, value: &Vec<K::Value>) -> Value {
        value.iter().map(|item| self.item.value(item)).collect()
    }
}

/// A nested object whose fields `O` declares.
pub(crate) struct Nested<O>(PhantomData<fn() -> O>);

impl<O> Nested<O> {
    pub(crate) const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<O: Output> Show for Nested<O> {
    type Value = O;

    fn schema(&self) -> Map<String, Value> {
        output_schema::<O>()
    }

    fn value(&self, value: &O) -> Value {
        to_value(value)
    }
}

/// The schema `literal` holds; every kind writes its schema as an object.
fn object(literal: Value) -> Map<String, Value> {
    match literal {
        Value::Object(schema) => schema,
        // Not taken: every caller passes a `json!` object literal.
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Mood {
        Calm,
        Busy,
    }

    impl Mood {
        const fn as_str(self) -> &'static str {
            match self {
                Self::Calm => "calm",
                Self::Busy => "busy",
            }
        }
    }

    struct Report {
        name: String,
        count: usize,
        mood: Option<Mood>,
        parts: Vec<Part>,
    }

    struct Part {
        label: String,
    }

    impl Output for Report {
        fn declare(fields: &mut Shown<'_, Self>) {
            fields.required("name", "The name.", &Str, |report| &report.name);
            fields.required("count", "How many.", &Count, |report| &report.count);
            let mood = Tag {
                all: &[Mood::Calm, Mood::Busy],
                name: Mood::as_str,
            };
            fields.optional("mood", "The mood.", &mood, |report| report.mood.as_ref());
            let parts = Items {
                item: Nested::<Part>::new(),
            };
            fields.required("parts", "The parts.", &parts, |report| &report.parts);
        }
    }

    impl Output for Part {
        fn declare(fields: &mut Shown<'_, Self>) {
            fields.required("label", "Its label.", &Str, |part| &part.label);
        }
    }

    #[test]
    fn the_schema_declares_every_field_and_requires_the_required() {
        let part = json!({
            "type": "object",
            "properties": { "label": { "type": "string", "description": "Its label." } },
            "required": ["label"],
            "additionalProperties": false,
        });
        assert_eq!(
            Value::Object(output_schema::<Report>()),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The name." },
                    "count": { "type": "integer", "minimum": 0, "description": "How many." },
                    "mood": { "type": "string", "enum": ["calm", "busy"], "description": "The mood." },
                    "parts": { "type": "array", "items": part, "description": "The parts." },
                },
                "required": ["name", "count", "parts"],
                "additionalProperties": false,
            })
        );
    }

    #[test]
    fn a_value_writes_its_fields_and_leaves_out_an_absent_optional() {
        let mut report = Report {
            name: "status".into(),
            count: 2,
            mood: Some(Mood::Busy),
            parts: vec![Part {
                label: "one".into(),
            }],
        };
        assert_eq!(
            to_value(&report),
            json!({ "name": "status", "count": 2, "mood": "busy", "parts": [{ "label": "one" }] })
        );
        report.mood = None;
        assert!(to_value(&report).get("mood").is_none());
    }
}
