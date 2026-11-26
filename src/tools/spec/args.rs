//! A tool's arguments, declared once: an [`Args`] type declares its fields
//! in [`Args::declare`], reading each one through [`Fields::required`] or
//! [`Fields::optional`] with a [`Kind`]. That one function runs two ways:
//! walked for the schema, every field it reads is recorded (and the value
//! it builds from defaults is dropped); walked over a call's arguments, it
//! reads them. A field cannot be read without being declared; each bounded
//! kind holds its limit as a field, so no declaration can leave one out;
//! and a key the declaration does not read is refused. A refusal names the
//! field by its path, `ops[2].name`.

use serde_json::{Map, Value};

use super::invalid::{At, Fault, InvalidArgument};

/// A tool's arguments, or an object nested in them.
pub(crate) trait Args {
    fn declare(fields: &mut Fields<'_, '_>) -> Self;
}

/// What a field holds: its schema and how a JSON value becomes it.
pub(in crate::tools) trait Kind {
    /// A required field's kind also gives the `Default` the schema walk
    /// builds; a list's items and an optional field need none.
    type Out;

    /// The field's JSON Schema, without its description.
    fn schema(&self) -> Map<String, Value>;

    fn parse(&self, value: &Value, at: At<'_>) -> Result<Self::Out, InvalidArgument>;
}

/// The fields of one object, read in the walk that [`input_schema`] or
/// [`parse`] runs.
pub(crate) struct Fields<'w, 'a> {
    at: At<'a>,
    walk: Walk<'w, 'a>,
}

enum Walk<'w, 'a> {
    Schema(&'w mut SchemaWalk),
    Parse(&'w mut ParseWalk<'a>),
}

#[derive(Default)]
pub(super) struct SchemaWalk {
    pub(super) properties: Map<String, Value>,
    pub(super) required: Vec<Value>,
}

impl SchemaWalk {
    /// The object schema of the fields the walk recorded, closed to any
    /// other key; without a required field it has no `required` list.
    pub(super) fn into_object_schema(self) -> Map<String, Value> {
        let mut schema = Map::new();
        schema.insert("type".into(), "object".into());
        schema.insert("properties".into(), self.properties.into());
        if !self.required.is_empty() {
            schema.insert("required".into(), self.required.into());
        }
        schema.insert("additionalProperties".into(), false.into());
        schema
    }
}

/// The schema of one kind, written out key by key.
pub(super) fn schema<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

struct ParseWalk<'a> {
    object: &'a Map<String, Value>,
    declared: Vec<&'static str>,
    /// The first fault, in declaration order; the walk reads no further.
    fault: Option<InvalidArgument>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Presence {
    Required,
    Optional,
}

impl Fields<'_, '_> {
    pub(in crate::tools) fn required<K: Kind<Out: Default>>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
    ) -> K::Out {
        self.field(name, description, kind, Presence::Required)
            .unwrap_or_default()
    }

    pub(in crate::tools) fn optional<K: Kind>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
    ) -> Option<K::Out> {
        self.field(name, description, kind, Presence::Optional)
    }

    fn field<K: Kind>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
        presence: Presence,
    ) -> Option<K::Out> {
        let at = At::Field(&self.at, name);
        match &mut self.walk {
            Walk::Schema(walk) => {
                let mut schema = kind.schema();
                schema.insert("description".into(), description.into());
                walk.properties.insert(name.into(), schema.into());
                if presence == Presence::Required {
                    walk.required.push(name.into());
                }
                None
            }
            Walk::Parse(walk) => {
                walk.declared.push(name);
                if walk.fault.is_some() {
                    return None;
                }
                let parsed = match walk.object.get(name) {
                    Some(value) => kind.parse(value, at).map(Some),
                    None if presence == Presence::Optional => Ok(None),
                    None => Err(InvalidArgument::new(at, Fault::Missing)),
                };
                parsed.unwrap_or_else(|fault| {
                    walk.fault = Some(fault);
                    None
                })
            }
        }
    }
}

/// The JSON Schema of `A`: an object of its declared fields, closed to any
/// other key.
pub(super) fn input_schema<A: Args>() -> Map<String, Value> {
    declared_schema(A::declare, None)
}

/// The object schema of the fields `declare` reads, closed to any other
/// key. `tag` (a union's) leads the properties `required` lists.
pub(super) fn declared_schema<T>(
    declare: impl FnOnce(&mut Fields<'_, '_>) -> T,
    tag: Option<(&'static str, Value)>,
) -> Map<String, Value> {
    let mut walk = SchemaWalk::default();
    declare(&mut Fields {
        at: At::Root,
        walk: Walk::Schema(&mut walk),
    });
    if let Some((name, schema)) = tag {
        walk.properties.insert(name.into(), schema);
        walk.required.insert(0, name.into());
    }
    walk.into_object_schema()
}

/// `object` read as `A`, refused at its first fault in declaration order,
/// then at its first undeclared key in key order.
pub(in crate::tools) fn parse<A: Args>(
    object: &Map<String, Value>,
    at: At<'_>,
) -> Result<A, InvalidArgument> {
    read_fields(object, at, &[], A::declare)
}

/// `object` read by `declare`, with `skip` (a union's tag) taken as read.
pub(super) fn read_fields<T>(
    object: &Map<String, Value>,
    at: At<'_>,
    skip: &[&'static str],
    declare: impl FnOnce(&mut Fields<'_, '_>) -> T,
) -> Result<T, InvalidArgument> {
    let mut walk = ParseWalk {
        object,
        // clone: the walk adds each field it declares to the names it skips.
        declared: skip.to_vec(),
        fault: None,
    };
    let value = declare(&mut Fields {
        at,
        walk: Walk::Parse(&mut walk),
    });
    if let Some(fault) = walk.fault {
        return Err(fault);
    }
    match object
        .keys()
        .find(|key| !walk.declared.contains(&key.as_str()))
    {
        Some(key) => Err(InvalidArgument::new(At::Field(&at, key), Fault::Unknown)),
        None => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_spec_without_required_fields_has_no_required_list() {
        #[derive(Debug)]
        struct Empty;
        impl Args for Empty {
            fn declare(_: &mut Fields<'_, '_>) -> Self {
                Self
            }
        }
        assert_eq!(
            Value::Object(input_schema::<Empty>()),
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        );
        let refused = parse::<Empty>(json!({ "x": 1 }).as_object().unwrap(), At::Root);
        assert_eq!(refused.unwrap_err().field, "x");
    }
}
