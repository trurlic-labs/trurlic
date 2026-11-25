//! A language's query file and the contract the general resolver holds it
//! to. tree-sitter compiles a query against its grammar and refuses an
//! unknown node kind or field, a syntax error and an impossible shape;
//! it accepts any capture name, property and predicate it does not
//! evaluate itself, so the contract refuses each of those here: a query
//! is read as written or not at all.
//!
//! A pattern is a definition pattern or a mark pattern, and every pattern
//! has one root node (the resolver starts each pattern at one node).
//!
//! - A definition pattern captures `@definition`, the node its span is,
//!   and `@name`, the node its own path segment is, once each, and sets
//!   its `kind`. Its other properties: `kind.in "class:method"` (its kind
//!   in a container of a kind), `container "false"` (what its span holds
//!   is not under it), `within "top class"` (kept only at the file's top
//!   or in a container of those kinds), `definition.via` and
//!   `definition.to` (its span grows from the captured node up through
//!   ancestors of the `via` kinds to the first of a `to` kind, or it is
//!   dropped), `definition.wrap` (its span grows over each ancestor of
//!   those kinds right above it), and `visibility` (its visibility when no
//!   mark states one).
//! - A mark pattern captures any of `@role.test` (a definition whose name
//!   lies inside is test code), `@visibility.<name>` (a definition whose
//!   span holds the node, and the node its name, has that visibility; the
//!   innermost such mark wins), `@hash.unit` (one token whole),
//!   `@hash.skip` (left out of every hash) and `@hash.group` (a statement
//!   or block, whose extent the hash holds), and sets no property.
//!
//! A capture whose name starts with `_` is read by a text predicate alone
//! (`#eq?`, `#match?`, `#any-of?` and their `not-` forms, which tree-sitter
//! evaluates). Kinds and visibilities are named as `DefinitionKind::name`
//! and `Visibility::name` write them.

use std::collections::BTreeSet;
use std::fmt;

use tree_sitter::{CaptureQuantifier, Language, Query, QueryError};

use super::kinds::{DefinitionKind, Visibility};

/// What a capture of a query is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Capture {
    Definition,
    Name,
    Test,
    Visibility(Visibility),
    Unit,
    Skip,
    Group,
    /// Read by a text predicate alone.
    Predicate,
}

/// Where a definition may stand: at the file's top, or in a container of
/// a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    Top,
    Kind(DefinitionKind),
}

/// How a definition pattern makes a definition of what it captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Definer {
    pub(super) kind: DefinitionKind,
    /// Its kind in a container of a kind, by the container's.
    pub(super) kind_in: Vec<(DefinitionKind, DefinitionKind)>,
    /// What its span holds is under it.
    pub(super) container: bool,
    /// The containers it is kept in; `None` for every one.
    pub(super) within: Option<Vec<Container>>,
    /// The growth of its span, by its place in [`Rules::growths`].
    pub(super) growth: Option<usize>,
    /// The wrapping of its span, by its place in [`Rules::wraps`].
    pub(super) wrap: Option<usize>,
    pub(super) visibility: Option<Visibility>,
}

impl Definer {
    /// Whether it is kept in `container`.
    pub(super) fn admits(&self, container: Container) -> bool {
        self.within
            .as_ref()
            .is_none_or(|within| within.contains(&container))
    }

    /// Its kind in `container`.
    pub(super) fn kind_in(&self, container: Container) -> DefinitionKind {
        self.kind_in
            .iter()
            .find(|(outer, _)| Container::Kind(*outer) == container)
            .map_or(self.kind, |(_, kind)| *kind)
    }
}

/// A span's growth: from the captured node up through ancestors of the
/// `via` kinds to the first of a `to` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Growth {
    pub(super) via: Vec<u16>,
    pub(super) to: Vec<u16>,
}

/// A language's query, compiled and read by its contract.
#[derive(Debug)]
pub(super) struct Rules {
    pub(super) grammar: Language,
    pub(super) query: Query,
    /// By capture index.
    pub(super) captures: Vec<Capture>,
    /// By pattern index: the definer of a definition pattern.
    pub(super) patterns: Vec<Option<Definer>>,
    /// The distinct growths of the patterns.
    pub(super) growths: Vec<Growth>,
    /// The distinct sets of kinds a span wraps over.
    pub(super) wraps: Vec<Vec<u16>>,
}

/// Why a query file breaks its contract.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum QueryFault {
    Compile(QueryError),
    UnknownCapture(String),
    NotRooted { pattern: usize },
    UnknownPredicate { pattern: usize, operator: String },
    UnknownProperty { pattern: usize, key: String },
    BadValue { pattern: usize, key: String },
    NotADefinition { pattern: usize },
    UnknownNodeKind { pattern: usize, name: String },
    UnknownContainer { pattern: usize, name: String },
}

/// How often a pattern captures a capture of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    Never,
    Once,
    Otherwise,
}

impl fmt::Display for QueryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "does not compile: {error}"),
            Self::UnknownCapture(name) => {
                write!(f, "captures `@{name}`, which the resolver does not read")
            }
            Self::NotRooted { pattern } => {
                write!(f, "pattern {pattern} has more than one root node")
            }
            Self::UnknownPredicate { pattern, operator } => write!(
                f,
                "pattern {pattern} uses `#{operator}`, which the resolver does not evaluate"
            ),
            Self::UnknownProperty { pattern, key } => write!(
                f,
                "pattern {pattern} sets `{key}`, which the resolver does not read there"
            ),
            Self::BadValue { pattern, key } => write!(
                f,
                "pattern {pattern} sets `{key}` to a value the resolver does not read"
            ),
            Self::NotADefinition { pattern } => write!(
                f,
                "pattern {pattern} does not capture `@definition` and `@name` once each"
            ),
            Self::UnknownNodeKind { pattern, name } => write!(
                f,
                "pattern {pattern} names `{name}`, which is no kind of a node of the grammar's trees"
            ),
            Self::UnknownContainer { pattern, name } => write!(
                f,
                "pattern {pattern} names the container `{name}`, which no pattern defines"
            ),
        }
    }
}

/// The definition-pattern properties, each read once.
const PROPERTIES: [&str; 8] = [
    "kind",
    "kind.in",
    "container",
    "within",
    "definition.via",
    "definition.to",
    "definition.wrap",
    "visibility",
];

impl Rules {
    /// The rules of `source`, a query of `grammar`.
    pub(super) fn compile(grammar: Language, source: &str) -> Result<Self, QueryFault> {
        let query = Query::new(&grammar, source).map_err(QueryFault::Compile)?;
        let captures = query
            .capture_names()
            .iter()
            .map(|name| capture_of(name))
            .collect::<Result<Vec<_>, _>>()?;
        let mut rules = Self {
            grammar,
            query,
            captures,
            patterns: Vec::new(),
            growths: Vec::new(),
            wraps: Vec::new(),
        };
        for pattern in 0..rules.query.pattern_count() {
            let definer = rules.pattern(pattern)?;
            rules.patterns.push(definer);
        }
        rules.check_containers()?;
        Ok(rules)
    }

    /// The definer of pattern `pattern`, `None` for a mark pattern.
    fn pattern(&mut self, pattern: usize) -> Result<Option<Definer>, QueryFault> {
        if !self.query.is_pattern_rooted(pattern) {
            return Err(QueryFault::NotRooted { pattern });
        }
        if let Some(predicate) = self.query.general_predicates(pattern).first() {
            return Err(QueryFault::UnknownPredicate {
                pattern,
                operator: predicate.operator.to_string(),
            });
        }
        if let Some((property, _)) = self.query.property_predicates(pattern).first() {
            return Err(QueryFault::UnknownPredicate {
                pattern,
                operator: format!("is? {}", property.key),
            });
        }
        let settings = self.settings(pattern)?;
        let definition = self.count(pattern, Capture::Definition);
        let name = self.count(pattern, Capture::Name);
        match (definition, name, settings) {
            (Count::Never, Count::Never, None) => Ok(None),
            (Count::Once, Count::Once, settings) => self
                .definer(pattern, &settings.unwrap_or_default())
                .map(Some),
            (Count::Never, Count::Never, Some(settings)) => Err(QueryFault::UnknownProperty {
                pattern,
                key: settings.keys().next().cloned().unwrap_or_default(),
            }),
            // Every other mix: one of the two without the other, or either
            // more than once.
            (Count::Never | Count::Once | Count::Otherwise, _, _) => {
                Err(QueryFault::NotADefinition { pattern })
            }
        }
    }

    /// How often pattern `pattern` captures a capture that is `wanted`.
    fn count(&self, pattern: usize, wanted: Capture) -> Count {
        let quantifiers = self.query.capture_quantifiers(pattern);
        let mut captured = self
            .captures
            .iter()
            .zip(quantifiers)
            .filter(|(capture, quantifier)| {
                **capture == wanted && **quantifier != CaptureQuantifier::Zero
            })
            .map(|(_, quantifier)| *quantifier);
        match (captured.next(), captured.next()) {
            (None, _) => Count::Never,
            (Some(CaptureQuantifier::One), None) => Count::Once,
            // Quantified (`?`, `*`, `+`), or captured under two names.
            (Some(_), _) => Count::Otherwise,
        }
    }

    /// The properties pattern `pattern` sets, by key; `None` for none.
    fn settings(&self, pattern: usize) -> Result<Option<Settings>, QueryFault> {
        let mut settings = Settings::new();
        for property in self.query.property_settings(pattern) {
            let key = property.key.to_string();
            let unknown = !PROPERTIES.contains(&key.as_str()) || property.capture_id.is_some();
            let value = property
                .value
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let Some(value) = value.filter(|_| !unknown && !settings.contains_key(&key)) else {
                return Err(match unknown {
                    true => QueryFault::UnknownProperty { pattern, key },
                    false => QueryFault::BadValue { pattern, key },
                });
            };
            settings.insert(key, value.to_owned());
        }
        Ok((!settings.is_empty()).then_some(settings))
    }

    /// The definer of definition pattern `pattern`, which sets `settings`.
    fn definer(&mut self, pattern: usize, settings: &Settings) -> Result<Definer, QueryFault> {
        let read = Properties { pattern, settings };
        let kind = read.kind("kind")?.ok_or(QueryFault::BadValue {
            pattern,
            key: "kind".to_owned(),
        })?;
        let growth = match (
            read.node_kinds(&self.grammar, "definition.via")?,
            read.node_kinds(&self.grammar, "definition.to")?,
        ) {
            (via, Some(to)) => Some(intern(
                &mut self.growths,
                Growth {
                    via: via.unwrap_or_default(),
                    to,
                },
            )),
            (Some(_), None) => return Err(read.bad("definition.via")),
            (None, None) => None,
        };
        let wrap = read
            .node_kinds(&self.grammar, "definition.wrap")?
            .map(|kinds| intern(&mut self.wraps, kinds));
        Ok(Definer {
            kind,
            kind_in: read.kind_in()?,
            container: match settings.get("container").map(String::as_str) {
                None => true,
                Some("false") => false,
                Some(_) => return Err(read.bad("container")),
            },
            within: read.within()?,
            growth,
            wrap,
            visibility: match settings.get("visibility") {
                None => None,
                Some(name) => {
                    Some(Visibility::of_name(name).ok_or_else(|| read.bad("visibility"))?)
                }
            },
        })
    }

    /// Refuse a container a `within` or `kind.in` names that no pattern
    /// defines.
    fn check_containers(&self) -> Result<(), QueryFault> {
        let defined: BTreeSet<DefinitionKind> = self
            .patterns
            .iter()
            .flatten()
            .flat_map(|definer| {
                std::iter::once(definer.kind).chain(definer.kind_in.iter().map(|(_, kind)| *kind))
            })
            .collect();
        for (pattern, definer) in self.patterns.iter().enumerate() {
            let Some(definer) = definer else {
                continue;
            };
            let named = definer
                .within
                .iter()
                .flatten()
                .filter_map(|container| match container {
                    Container::Top => None,
                    Container::Kind(kind) => Some(*kind),
                })
                .chain(definer.kind_in.iter().map(|(outer, _)| *outer));
            if let Some(missing) = named.into_iter().find(|kind| !defined.contains(kind)) {
                return Err(QueryFault::UnknownContainer {
                    pattern,
                    name: missing.name().to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// The properties a pattern sets, by key.
type Settings = std::collections::BTreeMap<String, String>;

/// The properties of one definition pattern, read.
struct Properties<'a> {
    pattern: usize,
    settings: &'a Settings,
}

impl Properties<'_> {
    fn bad(&self, key: &str) -> QueryFault {
        QueryFault::BadValue {
            pattern: self.pattern,
            key: key.to_owned(),
        }
    }

    fn words(&self, key: &str) -> Option<impl Iterator<Item = &str>> {
        self.settings.get(key).map(|value| value.split_whitespace())
    }

    fn unknown_kind(&self, name: &str) -> QueryFault {
        QueryFault::UnknownNodeKind {
            pattern: self.pattern,
            name: name.to_owned(),
        }
    }

    /// The definition kind `key` names.
    fn kind(&self, key: &str) -> Result<Option<DefinitionKind>, QueryFault> {
        self.settings
            .get(key)
            .map(|name| DefinitionKind::of_name(name).ok_or_else(|| self.bad(key)))
            .transpose()
    }

    /// The node kinds `key` names, each a named kind of `grammar`.
    fn node_kinds(&self, grammar: &Language, key: &str) -> Result<Option<Vec<u16>>, QueryFault> {
        let Some(words) = self.words(key) else {
            return Ok(None);
        };
        let mut kinds = Vec::new();
        for name in words {
            // A supertype (`expression`) names a class of kinds; no node of
            // a tree is of it, so a growth to it would reach nothing.
            match grammar.id_for_node_kind(name, true) {
                0 => return Err(self.unknown_kind(name)),
                id if grammar.node_kind_is_supertype(id) => return Err(self.unknown_kind(name)),
                id => kinds.push(id),
            }
        }
        Ok(Some(kinds))
    }

    fn kind_in(&self) -> Result<Vec<(DefinitionKind, DefinitionKind)>, QueryFault> {
        let Some(words) = self.words("kind.in") else {
            return Ok(Vec::new());
        };
        words
            .map(|pair| {
                let (outer, kind) = pair.split_once(':').ok_or_else(|| self.bad("kind.in"))?;
                let named = |name| DefinitionKind::of_name(name).ok_or_else(|| self.bad("kind.in"));
                Ok((named(outer)?, named(kind)?))
            })
            .collect()
    }

    fn within(&self) -> Result<Option<Vec<Container>>, QueryFault> {
        let Some(words) = self.words("within") else {
            return Ok(None);
        };
        words
            .map(|name| match name {
                "top" => Ok(Container::Top),
                name => DefinitionKind::of_name(name)
                    .map(Container::Kind)
                    .ok_or_else(|| self.bad("within")),
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }
}

/// What the capture `name` is.
fn capture_of(name: &str) -> Result<Capture, QueryFault> {
    Ok(match name {
        "definition" => Capture::Definition,
        "name" => Capture::Name,
        "role.test" => Capture::Test,
        "hash.unit" => Capture::Unit,
        "hash.skip" => Capture::Skip,
        "hash.group" => Capture::Group,
        _ if name.starts_with('_') => Capture::Predicate,
        _ => match name
            .strip_prefix("visibility.")
            .and_then(Visibility::of_name)
        {
            Some(visibility) => Capture::Visibility(visibility),
            None => return Err(QueryFault::UnknownCapture(name.to_owned())),
        },
    })
}

/// The place of `value` in `values`, added when it is not there.
fn intern<T: PartialEq>(values: &mut Vec<T>, value: T) -> usize {
    match values.iter().position(|known| *known == value) {
        Some(at) => at,
        None => {
            values.push(value);
            values.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python() -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn fault(source: &str) -> QueryFault {
        Rules::compile(python(), source).unwrap_err()
    }

    const FUNCTION: &str = "((function_definition name: (identifier) @name) @definition";

    fn bad(pattern: usize, key: &str) -> QueryFault {
        QueryFault::BadValue {
            pattern,
            key: key.to_owned(),
        }
    }

    #[test]
    fn a_query_is_refused_for_each_break_of_its_contract() {
        let kind = "(#set! kind \"function\")";
        let cases = [
            (
                "(identifier) @frobnicate".to_owned(),
                QueryFault::UnknownCapture("frobnicate".to_owned()),
            ),
            (
                "(identifier) @visibility.secret".to_owned(),
                QueryFault::UnknownCapture("visibility.secret".to_owned()),
            ),
            (
                "((identifier) @_n (#frobnicate! @_n))".to_owned(),
                QueryFault::UnknownPredicate {
                    pattern: 0,
                    operator: "frobnicate!".to_owned(),
                },
            ),
            (
                "((identifier) @_n (#is? local))".to_owned(),
                QueryFault::UnknownPredicate {
                    pattern: 0,
                    operator: "is? local".to_owned(),
                },
            ),
            (
                format!("((identifier) @hash.unit {kind})"),
                QueryFault::UnknownProperty {
                    pattern: 0,
                    key: "kind".to_owned(),
                },
            ),
            (
                format!("{FUNCTION} (#set! kidn \"function\"))"),
                QueryFault::UnknownProperty {
                    pattern: 0,
                    key: "kidn".to_owned(),
                },
            ),
            (format!("{FUNCTION})"), bad(0, "kind")),
            (
                format!("{FUNCTION} (#set! kind \"klass\"))"),
                bad(0, "kind"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! container \"no\"))"),
                bad(0, "container"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! definition.via \"block\"))"),
                bad(0, "definition.via"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! visibility \"crate-ish\"))"),
                bad(0, "visibility"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! definition.wrap \"decorated\"))"),
                QueryFault::UnknownNodeKind {
                    pattern: 0,
                    name: "decorated".to_owned(),
                },
            ),
            (
                format!("{FUNCTION} {kind} (#set! definition.to \"expression\"))"),
                QueryFault::UnknownNodeKind {
                    pattern: 0,
                    name: "expression".to_owned(),
                },
            ),
            (
                format!("{FUNCTION} {kind} (#set! within \"top struct\"))"),
                QueryFault::UnknownContainer {
                    pattern: 0,
                    name: "struct".to_owned(),
                },
            ),
            (
                format!("((function_definition name: (identifier) @name) {kind})"),
                QueryFault::NotADefinition { pattern: 0 },
            ),
            (
                format!("((block (expression_statement)* @name) @definition {kind})"),
                QueryFault::NotADefinition { pattern: 0 },
            ),
            // A predicate outside the pattern's parentheses is a pattern
            // of its own: the definition pattern before it sets no kind.
            (format!("{FUNCTION}) {kind}"), bad(0, "kind")),
            (
                "((identifier) @hash.unit . (identifier) @hash.unit)".to_owned(),
                QueryFault::NotRooted { pattern: 0 },
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(fault(&source), expected, "{source}");
        }
        assert!(matches!(
            fault("(nonesuch) @hash.unit"),
            QueryFault::Compile(_)
        ));
    }

    #[test]
    fn a_query_within_its_contract_reads_each_pattern() {
        let source = format!(
            "{FUNCTION} (#set! kind \"function\") (#set! kind.in \"class:method\") \
             (#set! definition.wrap \"decorated_definition\") (#set! visibility \"public\"))\n\
             ((class_definition name: (identifier) @name) @definition (#set! kind \"class\"))\n\
             ((assignment left: (identifier) @name) @definition (#set! kind \"variable\") \
             (#set! container \"false\") (#set! within \"top class\") \
             (#set! definition.via \"assignment\") (#set! definition.to \"expression_statement\"))\n\
             ((identifier) @visibility.private @_n (#match? @_n \"^_\"))\n"
        );
        let rules = Rules::compile(python(), &source).unwrap();

        let function = rules.patterns[0].as_ref().unwrap();
        assert_eq!(
            function.kind_in(Container::Kind(DefinitionKind::Class)),
            DefinitionKind::Method
        );
        assert_eq!(function.kind_in(Container::Top), DefinitionKind::Function);
        assert_eq!(function.visibility, Some(Visibility::Public));
        assert_eq!(function.wrap, Some(0));
        let variable = rules.patterns[2].as_ref().unwrap();
        assert!(!variable.container);
        assert!(variable.admits(Container::Top));
        assert!(!variable.admits(Container::Kind(DefinitionKind::Function)));
        assert_eq!(variable.growth, Some(0));
        assert_eq!(rules.patterns[3], None);
        assert!(
            rules
                .captures
                .contains(&Capture::Visibility(Visibility::Private))
        );
    }
}
