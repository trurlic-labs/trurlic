//! Tool results capped at a byte budget.
//!
//! A payload that fits is sent as serialized. One that does not keeps its
//! shape: every array and string reachable from the root through objects
//! shrinks to one [`budget::level`], arrays by trailing items and strings by
//! trailing characters, and a `truncated` array added to the root names each
//! cut by JSON pointer with what it omitted. Array items are kept or dropped
//! whole, so every kept value is one the tool produced. Sizes are bytes of
//! the payload's compact serialization, the text the client receives.
//!
//! Each unit, and each array item in it, is measured only until it passes
//! the budget, so a cut costs in proportion to the budget, not to the
//! payload. Only a payload whose skeleton alone exceeds the budget is
//! measured whole, to report the bytes it omitted.

use std::io;

use serde_json::{Map, Value, json};

use crate::budget::{self, Shrinkable};
use crate::console::diag;

/// The root key that lists the cuts. No tool payload uses it.
const NOTICE_KEY: &str = "truncated";

/// Upper bound on the bytes `,"truncated":[]` adds to the root object.
const NOTICE_FRAME: usize = 15;

/// Bytes between the size checkpoints of a string, so a cut scans at most
/// this many bytes past a binary search.
const CHECKPOINT: usize = 64;

/// Serialize `payload` within `max_bytes`, cutting it as the module
/// describes when it does not fit. A payload that is not an object is
/// wrapped as `{"value": payload}` before it is cut.
pub(crate) fn fit_payload(payload: &Value, max_bytes: usize) -> String {
    if let Some(text) = serialize_within(payload, max_bytes) {
        return text;
    }

    let wrapped;
    let root = match payload {
        Value::Object(map) => map,
        other => {
            // clone: a non-object payload is wrapped so the notice has a root.
            wrapped = Map::from_iter([("value".to_owned(), other.clone())]);
            &wrapped
        }
    };
    let mut units = Vec::new();
    for (key, value) in root {
        collect(value, &mut pointer_to(key), &mut units, max_bytes);
    }
    match cut_root(root, &units, object_skeleton_len(root), max_bytes) {
        Some(cut) if cut.len() <= max_bytes => cut,
        cut => {
            if let Some(cut) = cut {
                diag!(
                    "trurlic: a cut tool result took {} of {max_bytes} bytes; sent as omitted",
                    cut.len()
                );
            }
            omitted_whole(serialized_len(payload, usize::MAX))
        }
    }
}

/// `payload` serialized, or `None` once it passes `max_bytes`: a payload
/// over the budget is never serialized whole.
fn serialize_within(payload: &Value, max_bytes: usize) -> Option<String> {
    let mut sink = Bounded {
        bytes: Vec::with_capacity(128),
        max_bytes,
    };
    // The only error a Value can raise here is the sink refusing bytes.
    serde_json::to_writer(&mut sink, payload).ok()?;
    String::from_utf8(sink.bytes).ok()
}

/// Cut `message` at a character boundary so that it and the note of the
/// bytes it lost fit in `max_bytes`.
pub(crate) fn fit_message(message: &str, max_bytes: usize) -> String {
    if message.len() <= max_bytes {
        return message.to_owned();
    }
    let note = |omitted: usize| format!("\n[truncated: {omitted} bytes omitted]");
    let keep = floor_char_boundary(message, max_bytes.saturating_sub(note(message.len()).len()));
    let head = message.split_at_checked(keep).map_or("", |(head, _)| head);
    format!("{head}{}", note(message.len() - keep))
}

/// Bytes `text` takes inside a serialized JSON string, quotes excluded.
pub(crate) fn json_str_len(text: &str) -> usize {
    text.bytes().map(escaped_len).sum()
}

/// Bytes a UTF-8 byte takes in a serialized JSON string. serde_json escapes
/// only `"`, `\` and control characters; the bytes of a non-ASCII character
/// are written as they are.
const fn escaped_len(byte: u8) -> usize {
    match byte {
        b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
        0x00..=0x1f => 6,
        _ => 1,
    }
}

/// The largest char boundary of `text` at or below `index`.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    (0..=index)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0)
}

/// `{"truncated":[{"at":"","omitted_bytes":N}]}`: the whole payload omitted,
/// for a payload whose skeleton alone exceeds the budget.
fn omitted_whole(bytes: usize) -> String {
    json!({ NOTICE_KEY: [{ "at": "", "omitted_bytes": bytes }] }).to_string()
}

/// Cut every unit of `root` to the highest level that fits, or `None` when
/// the skeleton does not fit even with every unit emptied.
fn cut_root(
    root: &Map<String, Value>,
    units: &[Unit<'_>],
    skeleton: usize,
    max_bytes: usize,
) -> Option<String> {
    let level = budget::level(units, skeleton + NOTICE_FRAME, max_bytes)?;

    let mut cuts = units.iter().map(|unit| unit.cut(level));
    let mut kept = Map::with_capacity(root.len() + 1);
    let mut notice = Vec::new();
    for (key, value) in root {
        // clone: the cut payload is a new map; its keys are the original's.
        kept.insert(key.clone(), rebuild(value, &mut cuts, &mut notice));
    }
    if !notice.is_empty() {
        kept.insert(NOTICE_KEY.to_owned(), Value::Array(notice));
    }
    Some(Value::Object(kept).to_string())
}

/// Bytes of `map` serialized with every unit [`collect`] finds in it emptied.
fn object_skeleton_len(map: &Map<String, Value>) -> usize {
    let entries = map.iter().fold(0usize, |sum, (key, value)| {
        // `"key":` and the value.
        sum.saturating_add(json_str_len(key) + 3)
            .saturating_add(skeleton_len(value))
    });
    entries + 2 + map.len().saturating_sub(1)
}

fn skeleton_len(value: &Value) -> usize {
    match value {
        Value::Object(map) => object_skeleton_len(map),
        Value::Array(_) | Value::String(_) => 2,
        Value::Null | Value::Bool(_) | Value::Number(_) => serialized_len(value, usize::MAX),
    }
}

/// `/key` with `~` and `/` escaped as RFC 6901 requires.
fn pointer_to(key: &str) -> String {
    format!("/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Push a unit for every array and string under `value`, descending
/// through objects only, in the order [`rebuild`] visits them.
fn collect<'a>(
    value: &'a Value,
    pointer: &mut String,
    units: &mut Vec<Unit<'a>>,
    max_bytes: usize,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let parent = pointer.len();
                pointer.push_str(&pointer_to(key));
                collect(child, pointer, units, max_bytes);
                pointer.truncate(parent);
            }
        }
        // clone: each unit owns the pointer that names it in the notice.
        Value::Array(items) => units.push(Unit::items(pointer.clone(), items, max_bytes)),
        Value::String(text) => units.push(Unit::text(pointer.clone(), text, max_bytes)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Rebuild `value`, applying the next cut to every unit [`collect`] found
/// and recording the cuts that omit something in `notice`.
fn rebuild<'u>(
    value: &Value,
    cuts: &mut impl Iterator<Item = Cut<'u>>,
    notice: &mut Vec<Value>,
) -> Value {
    match value {
        // clone: the cut payload is built anew; its keys are the original's.
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| (key.clone(), rebuild(child, cuts, notice)))
                .collect(),
        ),
        // collect pushed one unit per array and string in this order, so
        // the iterator runs out only past the last of them.
        Value::Array(_) | Value::String(_) => match cuts.next() {
            Some(cut) => cut.apply(notice),
            // clone: never reached, see above.
            None => value.clone(),
        },
        // clone: a scalar is copied into the cut payload as it is.
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

/// An array or string that can shrink, with the pointer that names it.
///
/// A unit is measured only until its size passes the budget: no longer
/// prefix can be kept, so `full` of a larger unit is some size past the
/// budget, which every search treats alike.
struct Unit<'a> {
    pointer: String,
    shape: Shape<'a>,
    full: usize,
    note: usize,
}

enum Shape<'a> {
    /// `ends[k]`: bytes of the first `k + 1` items and the commas between
    /// them, up to the first that passes the budget, which is counted only
    /// past it.
    Items {
        items: &'a [Value],
        ends: Vec<usize>,
    },
    /// `checkpoints[k]`: escaped bytes of the first `k * CHECKPOINT` bytes,
    /// up to the first that passes the budget.
    Text {
        text: &'a str,
        checkpoints: Vec<usize>,
    },
}

/// A unit at the chosen level: whole, or the number of items or bytes of
/// text it keeps.
struct Cut<'u> {
    unit: &'u Unit<'u>,
    keep: Option<usize>,
}

impl<'a> Unit<'a> {
    fn items(pointer: String, items: &'a [Value], max_bytes: usize) -> Self {
        let mut ends = Vec::new();
        let mut end = 0usize;
        for (index, item) in items.iter().enumerate() {
            end = end.saturating_add(usize::from(index > 0));
            end = end.saturating_add(serialized_len(item, max_bytes.saturating_sub(end)));
            ends.push(end);
            if end > max_bytes {
                break;
            }
        }
        let note = note_len(&pointer, "omitted_items", items.len());
        Self {
            pointer,
            full: ends.last().copied().unwrap_or(0),
            shape: Shape::Items { items, ends },
            note,
        }
    }

    fn text(pointer: String, text: &'a str, max_bytes: usize) -> Self {
        let mut checkpoints = Vec::new();
        let mut full = 0usize;
        for (index, &byte) in text.as_bytes().iter().enumerate() {
            if index % CHECKPOINT == 0 {
                if full > max_bytes {
                    break;
                }
                checkpoints.push(full);
            }
            full = full.saturating_add(escaped_len(byte));
        }
        let note = note_len(&pointer, "omitted_bytes", text.len());
        Self {
            pointer,
            shape: Shape::Text { text, checkpoints },
            full,
            note,
        }
    }

    /// Items or bytes this unit keeps within `limit`, with their size.
    fn keep_within(&self, limit: usize) -> (usize, usize) {
        match &self.shape {
            Shape::Items { ends, .. } => {
                let count = ends.partition_point(|&end| end <= limit);
                let bytes = count.checked_sub(1).and_then(|last| ends.get(last));
                (count, bytes.copied().unwrap_or(0))
            }
            Shape::Text { text, checkpoints } => {
                let block = checkpoints
                    .partition_point(|&size| size <= limit)
                    .saturating_sub(1);
                let mut end = block * CHECKPOINT;
                let mut size = checkpoints.get(block).copied().unwrap_or(0);
                for &byte in text.as_bytes().get(end..).unwrap_or_default() {
                    let next = size + escaped_len(byte);
                    if next > limit {
                        break;
                    }
                    (end, size) = (end + 1, next);
                }
                // The bytes of a character cut in half are non-ASCII, one each.
                let boundary = floor_char_boundary(text, end);
                (boundary, size - (end - boundary))
            }
        }
    }

    fn cut(&self, level: usize) -> Cut<'_> {
        Cut {
            unit: self,
            keep: budget::limit(self, level).map(|limit| self.keep_within(limit).0),
        }
    }
}

impl Shrinkable for Unit<'_> {
    fn full(&self) -> usize {
        self.full
    }

    fn note(&self) -> usize {
        self.note
    }

    fn kept(&self, limit: usize) -> usize {
        self.keep_within(limit).1
    }
}

impl Cut<'_> {
    /// The kept value, recording the omission in `notice` when there is one.
    fn apply(self, notice: &mut Vec<Value>) -> Value {
        let pointer = &self.unit.pointer;
        match (&self.unit.shape, self.keep) {
            (Shape::Items { items, .. }, Some(count)) => {
                let (head, tail) = items.split_at(count.min(items.len()));
                notice.push(json!({ "at": pointer, "omitted_items": tail.len() }));
                Value::Array(head.to_vec())
            }
            (Shape::Text { text, .. }, Some(end)) => {
                let (head, tail) = text.split_at(floor_char_boundary(text, end));
                notice.push(json!({ "at": pointer, "omitted_bytes": tail.len() }));
                Value::String(head.to_owned())
            }
            (Shape::Items { items, .. }, None) => Value::Array(items.to_vec()),
            (Shape::Text { text, .. }, None) => Value::String((*text).to_owned()),
        }
    }
}

/// Bytes of `{"at":<pointer>,"<field>":<count>}` and the comma before it.
fn note_len(pointer: &str, field: &str, count: usize) -> usize {
    serialized_len(&json!({ "at": pointer, field: count }), usize::MAX) + 1
}

/// Bytes of `value` serialized, or `cap + 1` once they pass `cap`, so a
/// value larger than `cap` is not serialized whole. The sink refusing bytes
/// is the only error serializing a `Value` can raise.
fn serialized_len(value: &Value, cap: usize) -> usize {
    let mut sink = ByteCount { bytes: 0, cap };
    serde_json::to_writer(&mut sink, value).map_or(cap.saturating_add(1), |()| sink.bytes)
}

/// A byte count that refuses to grow past `cap`.
struct ByteCount {
    bytes: usize,
    cap: usize,
}

impl io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > self.cap {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A buffer that refuses to grow past `max_bytes`.
struct Bounded {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl io::Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + bytes.len() > self.max_bytes {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    use super::*;

    fn fit(payload: &Value, max_bytes: usize) -> Value {
        let text = fit_payload(payload, max_bytes);
        assert!(text.len() <= max_bytes, "{} > {max_bytes}", text.len());
        serde_json::from_str(&text).unwrap()
    }

    /// Every kept array is a prefix of the original's items, every kept
    /// string a prefix of the original, everything else equal.
    fn assert_cut_from(original: &Value, kept: &Value) {
        match (original, kept) {
            (Value::Object(original), Value::Object(kept)) => {
                for (key, value) in kept {
                    if let Some(source) = original.get(key) {
                        assert_cut_from(source, value);
                    }
                }
            }
            (Value::Array(original), Value::Array(kept)) => {
                assert!(original.starts_with(kept));
            }
            (Value::String(original), Value::String(kept)) => {
                assert!(original.starts_with(kept.as_str()));
            }
            (original, kept) => assert_eq!(original, kept),
        }
    }

    /// The notice names each cut and counts exactly what it omitted.
    fn assert_notice_matches(original: &Value, kept: &Value) {
        let Some(notice) = kept.get(NOTICE_KEY) else {
            return;
        };
        for entry in notice.as_array().unwrap() {
            let at = entry["at"].as_str().unwrap();
            let (source, cut) = (original.pointer(at).unwrap(), kept.pointer(at).unwrap());
            if let Some(omitted) = entry.get("omitted_items") {
                let lost = source.as_array().unwrap().len() - cut.as_array().unwrap().len();
                assert_eq!(omitted, lost, "{at}");
            } else {
                let lost = source.as_str().unwrap().len() - cut.as_str().unwrap().len();
                assert_eq!(entry["omitted_bytes"], lost, "{at}");
            }
        }
    }

    #[test]
    fn a_payload_that_fits_is_sent_as_serialized() {
        let payload = json!({ "name": "auth", "decisions": ["a", "b"] });
        let text = serde_json::to_string(&payload).unwrap();
        assert_eq!(fit_payload(&payload, text.len()), text);
    }

    #[test]
    fn escaped_lengths_match_serde_json_for_every_character() {
        for character in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            let text = character.to_string();
            let serialized = serde_json::to_string(&text).unwrap();
            assert_eq!(json_str_len(&text), serialized.len() - 2, "{character:?}");
        }
    }

    #[test]
    fn a_cut_keeps_the_shape_and_names_every_omission() {
        let decisions: Vec<Value> = (0..100)
            .map(|i| json!({ "name": format!("decision-{i:03}"), "choice": "x".repeat(40) }))
            .collect();
        let payload = json!({
            "name": "auth",
            "count": 100,
            "rules": ["one", "two"],
            "decisions": decisions,
            "brief": "line \"quoted\" \u{fc}n\u{ef}c\u{f6}d\u{e9}\n".repeat(200),
        });

        let kept = fit(&payload, 2_000);

        assert_eq!(kept["name"], "auth");
        assert_eq!(kept["count"], 100);
        assert_eq!(kept["rules"], json!(["one", "two"]));
        assert_cut_from(&payload, &kept);
        assert_notice_matches(&payload, &kept);
        let cut: Vec<&str> = kept[NOTICE_KEY]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["at"].as_str().unwrap())
            .collect();
        assert_eq!(cut, ["/brief", "/decisions"]);
        assert!(!kept["decisions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn nested_objects_are_cut_inside_and_named_by_pointer() {
        let payload = json!({ "a/b": { "c~d": "y".repeat(1_000) } });
        let kept = fit(&payload, 200);
        assert_eq!(kept[NOTICE_KEY][0]["at"], "/a~1b/c~0d");
        assert_notice_matches(&payload, &kept);
    }

    #[test]
    fn a_skeleton_over_the_budget_is_sent_as_omitted() {
        let payload = Value::Object((0..50).map(|i| (format!("key-{i}"), json!(i))).collect());
        let size = payload.to_string().len();
        let kept = fit(&payload, 100);
        assert_eq!(
            kept,
            json!({ NOTICE_KEY: [{ "at": "", "omitted_bytes": size }] })
        );
    }

    #[test]
    fn a_payload_that_is_not_an_object_is_wrapped_before_it_is_cut() {
        let payload = json!((0..100).collect::<Vec<u32>>());
        let kept = fit(&payload, 120);
        assert_cut_from(&payload, &kept["value"]);
        assert_eq!(kept[NOTICE_KEY][0]["at"], "/value");
        assert_notice_matches(&json!({ "value": payload }), &kept);
    }

    #[test]
    fn a_long_message_is_cut_at_a_character_boundary_with_its_loss() {
        let message = "\u{e9}".repeat(100);
        let fitted = fit_message(&message, 61);
        assert!(fitted.len() <= 61, "{}", fitted.len());
        let (head, note) = fitted.split_once('\n').unwrap();
        assert!(message.starts_with(head));
        assert_eq!(
            note,
            format!("[truncated: {} bytes omitted]", message.len() - head.len())
        );
        assert_eq!(fit_message("short", 61), "short");
    }

    fn random_text(rng: &mut StdRng) -> String {
        const ALPHABET: [char; 8] = ['a', 'z', ' ', '"', '\\', '\n', '\u{1}', '\u{1F600}'];
        let len = rng.gen_range(0..300);
        (0..len)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())])
            .collect()
    }

    fn random_value(rng: &mut StdRng, depth: u32) -> Value {
        match rng.gen_range(0..if depth == 0 { 3 } else { 5 }) {
            0 => json!(rng.gen_range(0..100_000)),
            1 => json!(random_text(rng)),
            2 => Value::Null,
            3 => Value::Array(
                (0..rng.gen_range(0..12))
                    .map(|_| random_value(rng, depth - 1))
                    .collect(),
            ),
            _ => random_object(rng, depth - 1),
        }
    }

    fn random_object(rng: &mut StdRng, depth: u32) -> Value {
        Value::Object(
            (0..rng.gen_range(0..6))
                .map(|i| (format!("k{i}/~"), random_value(rng, depth)))
                .collect(),
        )
    }

    #[test]
    fn random_payloads_fit_every_budget_and_keep_prefixes() {
        let mut rng = StdRng::seed_from_u64(0x7472_75726c);
        let mut cuts = 0;
        for _ in 0..1_000 {
            let payload = random_object(&mut rng, 3);
            let size = payload.to_string().len();
            let max_bytes = rng.gen_range(64..size.max(65) + 64);
            let kept = fit(&payload, max_bytes);
            if kept == json!({ NOTICE_KEY: [{ "at": "", "omitted_bytes": size }] }) {
                continue;
            }
            assert_cut_from(&payload, &kept);
            assert_notice_matches(&payload, &kept);
            cuts += usize::from(kept.get(NOTICE_KEY).is_some());
        }
        // Most budgets fall below the payload's size.
        assert!(cuts > 500, "{cuts} cuts");
    }
}
