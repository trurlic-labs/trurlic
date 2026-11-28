//! The kinds a field can hold: each states its JSON Schema and turns a
//! JSON value into its type, and each bounded kind holds its limit as a
//! field.

use std::marker::PhantomData;

use serde_json::{Map, Value, json};

use super::args::{Args, Fields, Kind, declared_schema, input_schema, parse, read_fields, schema};
use super::invalid::{At, Fault, InvalidArgument};
use crate::sources::{MAX_QUERY_BYTES, MIN_QUERY_BYTES, Query};

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

/// A search's query: its parse is [`Query::parse`], the boundary, whose
/// refusal is a [`Fault::Query`]. Its schema states that parse for a
/// reader: the lengths are its bounds in bytes of UTF-8, which a schema's
/// count of characters states exactly for ASCII alone, and the pattern
/// excludes the control characters it refuses.
pub(crate) struct QueryText;

/// The characters [`Query::parse`] refuses, as `char::is_control` names
/// them: U+0000 to U+001F and U+007F to U+009F. A schema's pattern of a
/// text that holds none.
const NO_CONTROL: &str = "^[^\\u0000-\\u001F\\u007F-\\u009F]*$";

impl Kind for QueryText {
    type Out = Query;

    fn schema(&self) -> Map<String, Value> {
        schema([
            ("type", "string".into()),
            ("minLength", MIN_QUERY_BYTES.into()),
            ("maxLength", MAX_QUERY_BYTES.into()),
            ("pattern", NO_CONTROL.into()),
        ])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<Query, InvalidArgument> {
        let text = value
            .as_str()
            .ok_or_else(|| InvalidArgument::new(at, Fault::Expected("a string")))?;
        Query::parse(text).map_err(|fault| InvalidArgument::new(at, Fault::Query(fault)))
    }
}

/// A place in a result's order: an integer from 0.
pub(crate) struct Position;

impl Kind for Position {
    type Out = usize;

    fn schema(&self) -> Map<String, Value> {
        schema([("type", "integer".into()), ("minimum", 0.into())])
    }

    fn parse(&self, value: &Value, at: At<'_>) -> Result<usize, InvalidArgument> {
        let position = value.as_u64().and_then(|n| usize::try_from(n).ok());
        position.ok_or_else(|| InvalidArgument::new(at, Fault::Expected("an integer from 0")))
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
pub(in crate::tools) struct List<K> {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sources::QueryFault;

    /// Every field kind, in one spec.
    #[derive(Debug, PartialEq)]
    struct Probe {
        name: String,
        depth: Depth,
        tags: Vec<String>,
        anchors: Vec<Anchor>,
        scope: Option<Scope>,
        ops: Vec<Op>,
        position: usize,
        query: Query,
    }

    #[derive(Debug, Default, Clone, Copy, PartialEq)]
    enum Depth {
        #[default]
        Brief,
        Full,
    }

    #[derive(Debug, PartialEq)]
    struct Anchor {
        file: String,
        symbol: Option<String>,
    }

    #[derive(Debug, PartialEq)]
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
                position: fields.required("position", "Where to start.", &Position),
                query: fields.required("query", "What to find.", &QueryText),
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
            "tags": ["a", "b"],
            "anchors": [{ "file": "a.rs", "symbol": "f" }, { "file": "b.rs" }],
            "scope": { "component": "store" },
            "ops": [
                { "op": "record", "choice": "Sign" },
                { "op": "remove", "name": "old", "repoint_to": "new" },
            ],
            "position": 3,
            "query": "store",
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
                position: 3,
                query: Query::parse("store").unwrap(),
            }
        );
    }

    #[test]
    fn optional_fields_may_be_absent() {
        let mut arguments = valid();
        arguments.as_object_mut().unwrap().remove("scope");
        assert_eq!(probe(&arguments).unwrap().scope, None);
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
                with("/scope", Some(Value::Null)),
                "scope",
                Fault::Expected("an object"),
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
                "position of another type",
                with("/position", Some(json!("3"))),
                "position",
                Fault::Expected("an integer from 0"),
            ),
            (
                "negative position",
                with("/position", Some(json!(-1))),
                "position",
                Fault::Expected("an integer from 0"),
            ),
            (
                "query the search parses no query of",
                with("/query", Some(json!(""))),
                "query",
                Fault::Query(QueryFault::Empty),
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
                "tags": { "type": "array", "items": name, "maxItems": 2, "description": "Tags." },
                "anchors": { "type": "array", "items": anchor, "maxItems": 3, "description": "Where it applies." },
                "scope": scope,
                "ops": ops,
                "position": { "type": "integer", "minimum": 0, "description": "Where to start." },
                "query": {
                    "type": "string",
                    "minLength": MIN_QUERY_BYTES,
                    "maxLength": MAX_QUERY_BYTES,
                    "pattern": NO_CONTROL,
                    "description": "What to find.",
                },
            }),
            json!([
                "name", "depth", "tags", "anchors", "ops", "position", "query"
            ]),
        );
        assert_eq!(Value::Object(input_schema::<Probe>()), expected);
    }

    /// Each character either side of the bounds of the controls a query
    /// may not hold, with the ASCII and the multi-byte text beside them:
    /// the schema's pattern admits a text exactly when the parse does.
    #[test]
    fn the_query_s_pattern_admits_exactly_what_its_parse_admits() {
        let pattern = QueryText.schema()["pattern"].as_str().unwrap().to_owned();
        let regex = regex_automata::hybrid::regex::Regex::new(&pattern).unwrap();
        let mut cache = regex.create_cache();
        let boundaries = [
            '\u{0}', '\u{1f}', ' ', '~', '\u{7f}', '\u{80}', '\u{9f}', '\u{a0}', 'e', '\u{e9}',
            '\u{2028}', '\u{feff}',
        ];

        for at in boundaries {
            let text = format!("a{at}b");
            let by_schema = regex.is_match(&mut cache, text.as_str());

            assert_eq!(by_schema, Query::parse(&text).is_ok(), "{:?}", at);
        }
    }
}
