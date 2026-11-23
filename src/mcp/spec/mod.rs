//! Tool argument specs: one declaration per tool yields both the input
//! schema `tools/list` advertises and the parser the tool's calls go
//! through, so the two cannot drift.
//!
//! An [`Args`] type declares its fields in [`Args::declare`], reading each
//! one through [`Fields::required`] or [`Fields::optional`] with a [`Kind`].
//! That one function runs two ways: walked for the schema, every field it
//! reads is recorded (and the value it builds from defaults is dropped);
//! walked over a call's arguments, it reads them. A field cannot be read
//! without being declared; each bounded kind holds its limit as a field,
//! so no declaration can leave one out; and a key the declaration does not
//! read is refused. A refusal names the field by its path, `ops[2].name`.
//!
//! Text limits are bytes, from `store::limits`. The schema states them as
//! `maxLength`, which counts characters: every string within the byte
//! limit meets it, and a non-ASCII string can meet it and still be refused.

pub(crate) mod kinds;
pub(crate) mod tool;

use std::fmt;

use serde_json::{Map, Value};

pub(crate) use tool::{TOOLS, find};

// -- Declarations ---------------------------------------------------------

/// A tool's arguments, or an object nested in them. `Default` is the value
/// the schema walk builds and drops; no call ever sees it.
pub(crate) trait Args: Default {
    fn declare(fields: &mut Fields<'_, '_>) -> Self;
}

/// What a field holds: its schema and how a JSON value becomes it.
pub(crate) trait Kind {
    type Out: Default;

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
struct SchemaWalk {
    properties: Map<String, Value>,
    required: Vec<Value>,
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
    pub(crate) fn required<K: Kind>(
        &mut self,
        name: &'static str,
        description: &'static str,
        kind: &K,
    ) -> K::Out {
        self.field(name, description, kind, Presence::Required)
            .unwrap_or_default()
    }

    pub(crate) fn optional<K: Kind>(
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
pub(crate) fn input_schema<A: Args>() -> Map<String, Value> {
    declared_schema(A::declare, None)
}

/// The object schema of the fields `declare` reads, closed to any other
/// key. `tag` (a union's) leads the properties `required` lists.
fn declared_schema<T>(
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
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    schema.insert("properties".into(), walk.properties.into());
    if !walk.required.is_empty() {
        schema.insert("required".into(), walk.required.into());
    }
    schema.insert("additionalProperties".into(), false.into());
    schema
}

/// `object` read as `A`, refused at its first fault in declaration order,
/// then at its first undeclared key in key order.
pub(crate) fn parse<A: Args>(
    object: &Map<String, Value>,
    at: At<'_>,
) -> Result<A, InvalidArgument> {
    read_fields(object, at, &[], A::declare)
}

/// `object` read by `declare`, with `skip` (a union's tag) taken as read.
fn read_fields<T>(
    object: &Map<String, Value>,
    at: At<'_>,
    skip: &[&'static str],
    declare: impl FnOnce(&mut Fields<'_, '_>) -> T,
) -> Result<T, InvalidArgument> {
    let mut walk = ParseWalk {
        object,
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

// -- Refusals -------------------------------------------------------------

/// Where a value sits in a call's arguments, rendered as `ops[2].name`.
#[derive(Clone, Copy)]
pub(crate) enum At<'a> {
    Root,
    Field(&'a At<'a>, &'a str),
    Item(&'a At<'a>, usize),
}

impl fmt::Display for At<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => Ok(()),
            Self::Field(Self::Root, name) => f.write_str(name),
            Self::Field(parent, name) => write!(f, "{parent}.{name}"),
            Self::Item(parent, index) => write!(f, "{parent}[{index}]"),
        }
    }
}

/// An argument the spec refuses, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvalidArgument {
    /// The argument's path, `ops[2].name`.
    pub field: String,
    pub fault: Fault,
}

impl InvalidArgument {
    fn new(at: At<'_>, fault: Fault) -> Self {
        Self {
            field: at.to_string(),
            fault,
        }
    }
}

impl fmt::Display for InvalidArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.field, self.fault)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    Unknown,
    Missing,
    /// Of another JSON type; names the one the spec holds, "a string".
    Expected(&'static str),
    TooLong {
        bytes: usize,
        max_bytes: usize,
    },
    TooMany {
        items: usize,
        max_items: usize,
    },
    OutOfRange {
        min: i64,
        max: i64,
    },
    NotOneOf(Vec<&'static str>),
    ControlCharacter,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("is not declared by this tool"),
            Self::Missing => f.write_str("is required"),
            Self::Expected(kind) => write!(f, "must be {kind}"),
            Self::TooLong { bytes, max_bytes } => {
                write!(f, "is {bytes} bytes, over the limit of {max_bytes}")
            }
            Self::TooMany { items, max_items } => {
                write!(f, "has {items} items, over the limit of {max_items}")
            }
            Self::OutOfRange { min, max } => write!(f, "must be an integer from {min} to {max}"),
            Self::NotOneOf(names) => write!(f, "must be one of: {}", names.join(", ")),
            Self::ControlCharacter => {
                f.write_str("contains a control character other than tab or a line break")
            }
        }
    }
}

// -- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::kinds::{Choice, Flag, Integer, List, Object, Text, Union, Variant};
    use super::*;

    /// Every field kind the 0.4.0 tools need, in one spec.
    #[derive(Debug, Default, PartialEq)]
    struct Probe {
        name: String,
        depth: Depth,
        full: Option<bool>,
        limit: Option<i64>,
        tags: Vec<String>,
        anchors: Vec<Anchor>,
        scope: Option<Scope>,
        ops: Vec<Op>,
    }

    #[derive(Debug, Default, Clone, Copy, PartialEq)]
    enum Depth {
        #[default]
        Brief,
        Full,
    }

    #[derive(Debug, Default, PartialEq)]
    struct Anchor {
        file: String,
        symbol: Option<String>,
    }

    #[derive(Debug, Default, PartialEq)]
    struct Scope {
        component: String,
    }

    #[derive(Debug, PartialEq)]
    enum Op {
        Record {
            choice: String,
        },
        Remove {
            name: String,
            repoint_to: Option<String>,
        },
    }

    impl Default for Op {
        fn default() -> Self {
            Self::Record {
                choice: String::new(),
            }
        }
    }

    const NAME: Text = Text { max_bytes: 8 };

    impl Args for Probe {
        fn declare(fields: &mut Fields<'_, '_>) -> Self {
            Self {
                name: fields.required("name", "A name.", &NAME),
                depth: fields.required(
                    "depth",
                    "How much to show.",
                    &Choice {
                        values: &[("brief", Depth::Brief), ("full", Depth::Full)],
                    },
                ),
                full: fields.optional("full", "Everything.", &Flag),
                limit: fields.optional("limit", "At most this many.", &Integer { min: 1, max: 50 }),
                tags: fields.required(
                    "tags",
                    "Tags.",
                    &List {
                        item: NAME,
                        max_items: 2,
                    },
                ),
                anchors: fields.required(
                    "anchors",
                    "Where it applies.",
                    &List {
                        item: Object::<Anchor>::new(),
                        max_items: 3,
                    },
                ),
                scope: fields.optional("scope", "Narrows the probe.", &Object::<Scope>::new()),
                ops: fields.required(
                    "ops",
                    "The changes.",
                    &List {
                        item: Union {
                            tag: "op",
                            variants: &[
                                Variant {
                                    name: "record",
                                    declare: |fields| Op::Record {
                                        choice: fields.required("choice", "The rule.", &NAME),
                                    },
                                },
                                Variant {
                                    name: "remove",
                                    declare: |fields| Op::Remove {
                                        name: fields.required("name", "The decision.", &NAME),
                                        repoint_to: fields.optional(
                                            "repoint_to",
                                            "Its heir.",
                                            &NAME,
                                        ),
                                    },
                                },
                            ],
                        },
                        max_items: 4,
                    },
                ),
            }
        }
    }

    impl Args for Anchor {
        fn declare(fields: &mut Fields<'_, '_>) -> Self {
            Self {
                file: fields.required("file", "A path.", &NAME),
                symbol: fields.optional("symbol", "A symbol in it.", &NAME),
            }
        }
    }

    impl Args for Scope {
        fn declare(fields: &mut Fields<'_, '_>) -> Self {
            Self {
                component: fields.required("component", "A component.", &NAME),
            }
        }
    }

    fn valid() -> Value {
        json!({
            "name": "auth",
            "depth": "full",
            "full": true,
            "limit": 50,
            "tags": ["a", "b"],
            "anchors": [{ "file": "a.rs", "symbol": "f" }, { "file": "b.rs" }],
            "scope": { "component": "store" },
            "ops": [
                { "op": "record", "choice": "Sign" },
                { "op": "remove", "name": "old", "repoint_to": "new" },
            ],
        })
    }

    fn probe(arguments: &Value) -> Result<Probe, InvalidArgument> {
        parse::<Probe>(arguments.as_object().unwrap(), At::Root)
    }

    /// `valid()` with the value at `pointer` replaced, or removed for `None`.
    fn with(pointer: &str, value: Option<Value>) -> Value {
        let mut arguments = valid();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let target = arguments.pointer_mut(parent).unwrap();
        match (target, value) {
            (Value::Object(object), Some(value)) => {
                object.insert(key.into(), value);
            }
            (Value::Object(object), None) => {
                object.remove(key);
            }
            (Value::Array(items), Some(value)) => items[key.parse::<usize>().unwrap()] = value,
            (target, value) => panic!("cannot set {pointer} in {target} to {value:?}"),
        }
        arguments
    }

    #[test]
    fn a_valid_call_parses_into_its_type() {
        let parsed = probe(&valid()).unwrap();
        assert_eq!(
            parsed,
            Probe {
                name: "auth".into(),
                depth: Depth::Full,
                full: Some(true),
                limit: Some(50),
                tags: vec!["a".into(), "b".into()],
                anchors: vec![
                    Anchor {
                        file: "a.rs".into(),
                        symbol: Some("f".into())
                    },
                    Anchor {
                        file: "b.rs".into(),
                        symbol: None
                    },
                ],
                scope: Some(Scope {
                    component: "store".into()
                }),
                ops: vec![
                    Op::Record {
                        choice: "Sign".into()
                    },
                    Op::Remove {
                        name: "old".into(),
                        repoint_to: Some("new".into())
                    },
                ],
            }
        );
    }

    #[test]
    fn optional_fields_may_be_absent() {
        let mut arguments = valid();
        for key in ["full", "limit", "scope"] {
            arguments.as_object_mut().unwrap().remove(key);
        }
        let parsed = probe(&arguments).unwrap();
        assert_eq!(
            (parsed.full, parsed.limit, parsed.scope),
            (None, None, None)
        );
    }

    /// Each malformed shape is refused with the field it breaks.
    #[test]
    fn each_malformed_shape_is_refused_naming_its_field() {
        let long = "x".repeat(9);
        let cases: Vec<(&str, Value, &str, Fault)> = vec![
            (
                "unknown key",
                with("/extra", Some(json!(1))),
                "extra",
                Fault::Unknown,
            ),
            (
                "unknown nested key",
                with("/anchors/1/line", Some(json!(3))),
                "anchors[1].line",
                Fault::Unknown,
            ),
            (
                "unknown key in a union variant",
                with("/ops/1/choice", Some(json!("x"))),
                "ops[1].choice",
                Fault::Unknown,
            ),
            (
                "missing required",
                with("/name", None),
                "name",
                Fault::Missing,
            ),
            (
                "missing nested required",
                with("/anchors/0/file", None),
                "anchors[0].file",
                Fault::Missing,
            ),
            (
                "string of another type",
                with("/name", Some(json!(5))),
                "name",
                Fault::Expected("a string"),
            ),
            (
                "null for an optional field",
                with("/full", Some(Value::Null)),
                "full",
                Fault::Expected("a boolean"),
            ),
            (
                "boolean of another type",
                with("/full", Some(json!("yes"))),
                "full",
                Fault::Expected("a boolean"),
            ),
            (
                "integer as a fraction",
                with("/limit", Some(json!(1.5))),
                "limit",
                Fault::Expected("an integer"),
            ),
            (
                "integer below its bound",
                with("/limit", Some(json!(0))),
                "limit",
                Fault::OutOfRange { min: 1, max: 50 },
            ),
            (
                "integer past i64",
                with("/limit", Some(json!(u64::MAX))),
                "limit",
                Fault::OutOfRange { min: 1, max: 50 },
            ),
            (
                "unknown enum value",
                with("/depth", Some(json!("deep"))),
                "depth",
                Fault::NotOneOf(vec!["brief", "full"]),
            ),
            (
                "text over its limit",
                with("/name", Some(json!(long))),
                "name",
                Fault::TooLong {
                    bytes: 9,
                    max_bytes: 8,
                },
            ),
            (
                "text with a control character",
                with("/name", Some(json!("a\u{7}b"))),
                "name",
                Fault::ControlCharacter,
            ),
            (
                "array over its limit",
                with("/tags", Some(json!(["a", "b", "c"]))),
                "tags",
                Fault::TooMany {
                    items: 3,
                    max_items: 2,
                },
            ),
            (
                "array of another type",
                with("/tags", Some(json!("a"))),
                "tags",
                Fault::Expected("an array"),
            ),
            (
                "array item of another type",
                with("/tags/1", Some(json!(2))),
                "tags[1]",
                Fault::Expected("a string"),
            ),
            (
                "nested object of another type",
                with("/scope", Some(json!(["store"]))),
                "scope",
                Fault::Expected("an object"),
            ),
            (
                "array of objects holding another type",
                with("/anchors/0", Some(json!("a.rs"))),
                "anchors[0]",
                Fault::Expected("an object"),
            ),
            (
                "union without its tag",
                with("/ops/0/op", None),
                "ops[0].op",
                Fault::Missing,
            ),
            (
                "union with an unknown tag",
                with("/ops/0/op", Some(json!("merge"))),
                "ops[0].op",
                Fault::NotOneOf(vec!["record", "remove"]),
            ),
            (
                "union tag of another type",
                with("/ops/0/op", Some(json!(1))),
                "ops[0].op",
                Fault::Expected("a string"),
            ),
            (
                "union variant missing a field",
                with("/ops/1/name", None),
                "ops[1].name",
                Fault::Missing,
            ),
        ];
        for (case, arguments, field, fault) in cases {
            let refused = probe(&arguments).expect_err(case);
            assert_eq!(
                refused,
                InvalidArgument {
                    field: field.into(),
                    fault
                },
                "{case}"
            );
            assert!(
                refused.to_string().starts_with(&format!("`{field}` ")),
                "{case}"
            );
        }
    }

    /// A field fault is reported before an unknown key, and faults in
    /// declaration order.
    #[test]
    fn the_first_fault_in_declaration_order_is_reported() {
        let mut arguments = with("/extra", Some(json!(1)));
        arguments["depth"] = json!("deep");
        arguments["tags"] = json!(1);
        assert_eq!(probe(&arguments).unwrap_err().field, "depth");
    }

    #[test]
    fn the_schema_declares_every_field_with_its_limit() {
        let name = json!({ "type": "string", "maxLength": 8 });
        let described = |description: &str| {
            let mut schema = name.clone();
            schema["description"] = json!(description);
            schema
        };
        let object = |properties: Value, required: Value| {
            json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            })
        };
        let anchor = object(
            json!({ "file": described("A path."), "symbol": described("A symbol in it.") }),
            json!(["file"]),
        );
        let mut scope = object(
            json!({ "component": described("A component.") }),
            json!(["component"]),
        );
        scope["description"] = json!("Narrows the probe.");
        let ops = json!({
            "type": "array",
            "maxItems": 4,
            "description": "The changes.",
            "items": {
                "type": "object",
                "oneOf": [
                    object(
                        json!({
                            "op": { "type": "string", "const": "record" },
                            "choice": described("The rule."),
                        }),
                        json!(["op", "choice"]),
                    ),
                    object(
                        json!({
                            "op": { "type": "string", "const": "remove" },
                            "name": described("The decision."),
                            "repoint_to": described("Its heir."),
                        }),
                        json!(["op", "name"]),
                    ),
                ],
            },
        });
        let expected = object(
            json!({
                "name": described("A name."),
                "depth": { "type": "string", "enum": ["brief", "full"], "description": "How much to show." },
                "full": { "type": "boolean", "description": "Everything." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "At most this many." },
                "tags": { "type": "array", "items": name, "maxItems": 2, "description": "Tags." },
                "anchors": { "type": "array", "items": anchor, "maxItems": 3, "description": "Where it applies." },
                "scope": scope,
                "ops": ops,
            }),
            json!(["name", "depth", "tags", "anchors", "ops"]),
        );
        assert_eq!(Value::Object(input_schema::<Probe>()), expected);
    }

    #[test]
    fn a_spec_without_required_fields_has_no_required_list() {
        #[derive(Debug, Default)]
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
