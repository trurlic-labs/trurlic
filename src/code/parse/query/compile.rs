//! Reading a query file by the contract (`mod.rs`): each capture name,
//! property and predicate the contract does not name is refused, and each
//! pattern is read into its definer or found a mark pattern.

use std::collections::BTreeSet;

use tree_sitter::{CaptureQuantifier, Language, Query};

use super::{Capture, Container, Definer, Growth, QueryFault, Rules};
use crate::code::index::{DefinitionKind, Visibility};

/// How often a pattern captures a capture of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    Never,
    Once,
    /// On several nodes, each once.
    Many,
    Otherwise,
}

/// The definition-pattern properties, each read once.
const PROPERTIES: [&str; 13] = [
    "kind",
    "kind.in",
    "container",
    "within",
    "definition.via",
    "definition.to",
    "definition.wrap",
    "definition.preceding",
    "exclusive",
    "name.separator",
    "name.skip",
    "name.text",
    "visibility",
];

impl Rules {
    /// The rules of `source`, a query of `grammar`.
    pub(in crate::code) fn compile(grammar: Language, source: &str) -> Result<Self, QueryFault> {
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
            precedings: Vec::new(),
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
            (Count::Once, Count::Once | Count::Many, settings) => self
                .definer(pattern, &settings.unwrap_or_default())
                .map(Some),
            (Count::Never, Count::Never, Some(settings)) => Err(QueryFault::UnknownProperty {
                pattern,
                key: settings.keys().next().cloned().unwrap_or_default(),
            }),
            // Every other mix: one of the two without the other, or either
            // more than once.
            (Count::Never | Count::Once | Count::Many | Count::Otherwise, _, _) => {
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
            (Some(CaptureQuantifier::OneOrMore), None) => Count::Many,
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
        let growth = self.growth(&read)?;
        let wrap = read
            .node_kinds(&self.grammar, "definition.wrap")?
            .map(|kinds| intern(&mut self.wraps, kinds));
        let preceding = read
            .node_kinds(&self.grammar, "definition.preceding")?
            .map(|kinds| intern(&mut self.precedings, kinds));
        Ok(Definer {
            kind,
            kind_in: read.kind_in()?,
            container: read.flag("container", true)?,
            within: read.within()?,
            growth,
            wrap,
            preceding,
            exclusive: read.flag("exclusive", false)?,
            separator: settings.get("name.separator").cloned(),
            skip: read
                .node_kinds(&self.grammar, "name.skip")?
                .unwrap_or_default(),
            text: settings.get("name.text").cloned(),
            visibility: read.visibility()?,
        })
    }

    /// The growth `read` states, by its place in [`Rules::growths`];
    /// `None` for none.
    fn growth(&mut self, read: &Properties<'_>) -> Result<Option<usize>, QueryFault> {
        match (
            read.node_kinds(&self.grammar, "definition.via")?,
            read.node_kinds(&self.grammar, "definition.to")?,
        ) {
            (via, Some(to)) => Ok(Some(intern(
                &mut self.growths,
                Growth {
                    via: via.unwrap_or_default(),
                    to,
                },
            ))),
            (Some(_), None) => Err(read.bad("definition.via")),
            (None, None) => Ok(None),
        }
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
                    Container::Top | Container::Unnamed => None,
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

    /// The flag `key` sets: `unset` when the pattern does not set it, the
    /// other of `"true"` and `"false"` when it does.
    fn flag(&self, key: &str, unset: bool) -> Result<bool, QueryFault> {
        match (self.settings.get(key).map(String::as_str), unset) {
            (None, _) => Ok(unset),
            (Some("false"), true) => Ok(false),
            (Some("true"), false) => Ok(true),
            (Some(_), _) => Err(self.bad(key)),
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

    fn visibility(&self) -> Result<Option<Visibility>, QueryFault> {
        self.settings
            .get("visibility")
            .map(|name| Visibility::of_name(name).ok_or_else(|| self.bad("visibility")))
            .transpose()
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
        "export.name" => Capture::Export,
        "scope.unnamed" => Capture::Unnamed,
        "scope.none" => Capture::NoScope,
        "hash.unit" => Capture::Unit,
        "hash.tokens" => Capture::Tokens,
        "visibility.through" => Capture::Through,
        "hash.chars" => Capture::Chars,
        "hash.delimiter" => Capture::Delimiter,
        "hash.join" => Capture::Join,
        "hash.string" => Capture::String,
        "hash.escape" => Capture::Escape,
        "hash.literal" => Capture::Literal,
        "hash.skip" => Capture::Skip,
        "hash.group" => Capture::Group,
        _ if name.starts_with('_') => Capture::Predicate,
        _ => match (
            name.strip_prefix("visibility.within."),
            name.strip_prefix("visibility.after."),
        ) {
            (Some(within), _) => Visibility::of_name(within).map(Capture::Within),
            (None, Some(after)) => Visibility::of_name(after).map(Capture::After),
            (None, None) => name
                .strip_prefix("visibility.")
                .and_then(Visibility::of_name)
                .map(Capture::Visibility),
        }
        .ok_or_else(|| QueryFault::UnknownCapture(name.to_owned()))?,
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
                "(identifier) @visibility.within.secret".to_owned(),
                QueryFault::UnknownCapture("visibility.within.secret".to_owned()),
            ),
            (
                "(identifier) @visibility.after.secret".to_owned(),
                QueryFault::UnknownCapture("visibility.after.secret".to_owned()),
            ),
            (
                format!("{FUNCTION} {kind} (#set! name.skip \"arguments\"))"),
                QueryFault::UnknownNodeKind {
                    pattern: 0,
                    name: "arguments".to_owned(),
                },
            ),
            (
                format!(
                    "((function_definition name: (_) @definition body: (_) @definition) @name {kind})"
                ),
                QueryFault::NotADefinition { pattern: 0 },
            ),
            (
                "(identifier) @scope.named".to_owned(),
                QueryFault::UnknownCapture("scope.named".to_owned()),
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
                format!("{FUNCTION} {kind} (#set! exclusive \"false\"))"),
                bad(0, "exclusive"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! container \"true\"))"),
                bad(0, "container"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! name.separator \" \"))"),
                bad(0, "name.separator"),
            ),
            (
                format!("{FUNCTION} {kind} (#set! definition.preceding \"decorators\"))"),
                QueryFault::UnknownNodeKind {
                    pattern: 0,
                    name: "decorators".to_owned(),
                },
            ),
            (
                "((identifier) @export.names)".to_owned(),
                QueryFault::UnknownCapture("export.names".to_owned()),
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
             ((identifier) @visibility.private @_n (#match? @_n \"^_\"))\n\
             ((class_definition name: (_) @name) @definition (#set! kind \"class\") \
             (#set! exclusive \"true\") (#set! name.separator \".\") \
             (#set! definition.preceding \"decorator comment\"))\n\
             ([(identifier) @export.name (string) @hash.tokens])\n\
             ([(lambda) @scope.unnamed (block) @visibility.within.public (string) @hash.chars])\n\
             (lambda) @scope.none\n\
             ((call function: (identifier) @name arguments: (argument_list (identifier) @name)) \
             @definition (#set! kind \"function\") (#set! name.skip \"keyword_argument\") \
             (#set! name.text \"default\"))\n\
             (class_definition body: (block . (_) @visibility.after.protected))\n\
             (if_statement) @visibility.through\n"
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
        let claimed = rules.patterns[4].as_ref().unwrap();
        assert!(claimed.exclusive && !function.exclusive);
        assert_eq!(claimed.separator.as_deref(), Some("."));
        assert_eq!(claimed.preceding, Some(0));
        assert_eq!(rules.precedings[0].len(), 2);
        assert!(rules.captures.contains(&Capture::Export));
        assert!(rules.captures.contains(&Capture::Tokens));
        assert!(rules.captures.contains(&Capture::Unnamed));
        assert!(
            rules
                .captures
                .contains(&Capture::Within(Visibility::Public))
        );
        assert!(
            rules
                .captures
                .contains(&Capture::Visibility(Visibility::Private))
        );
        let named_twice = rules.patterns[8].as_ref().unwrap();
        assert_eq!(named_twice.skip.len(), 1);
        assert_eq!(named_twice.text.as_deref(), Some("default"));
        assert!(rules.captures.contains(&Capture::Chars));
        assert!(rules.captures.contains(&Capture::NoScope));
        assert!(rules.captures.contains(&Capture::Through));
        assert!(
            rules
                .captures
                .contains(&Capture::After(Visibility::Protected))
        );
    }
}
