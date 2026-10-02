//! Step prompt text whose listings shrink to fit a byte budget.
//!
//! A prompt is fixed text (the preamble, the step's instructions, the
//! protocol) around listings of decisions and patterns. Rendering within a
//! budget keeps all the fixed text and cuts the listings to one
//! [`budget::level`], so a short listing stays whole while a long one keeps
//! its first entries and ends with a line counting what it left out. The
//! caller's `measure` sizes the text, so the budget is counted in the
//! transport's bytes.

use crate::budget::{self, Shrinkable};

#[derive(Debug, Default)]
pub(crate) struct PromptText {
    parts: Vec<Part>,
}

#[derive(Debug)]
enum Part {
    Text(String),
    Listing {
        /// Plural noun for the entries, used in the omission line.
        noun: &'static str,
        entries: Vec<String>,
    },
}

impl PromptText {
    pub(crate) fn push_str(&mut self, text: &str) {
        if let Some(Part::Text(last)) = self.parts.last_mut() {
            last.push_str(text);
        } else {
            self.parts.push(Part::Text(text.to_owned()));
        }
    }

    /// Append entries that may be cut from the end to fit a budget.
    pub(crate) fn push_listing(&mut self, noun: &'static str, entries: Vec<String>) {
        if !entries.is_empty() {
            self.parts.push(Part::Listing { noun, entries });
        }
    }

    /// The text within `max_bytes` as `measure` counts them, or with every
    /// listing cut to its omission line when the fixed text alone exceeds
    /// them.
    pub(crate) fn render(&self, max_bytes: usize, measure: fn(&str) -> usize) -> String {
        let (mut fixed, mut whole) = (0, 0);
        let mut listings = Vec::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => {
                    fixed += measure(text);
                    whole += text.len();
                }
                Part::Listing { noun, entries } => {
                    whole += entries.iter().map(String::len).sum::<usize>();
                    listings.push(Measured::new(noun, entries, measure));
                }
            }
        }
        let level = budget::level(&listings, fixed, max_bytes).unwrap_or(0);

        let mut out = String::with_capacity(whole.min(max_bytes));
        let mut measured = listings.iter();
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Listing { noun, entries } => {
                    let keep = measured
                        .next()
                        .and_then(|listing| listing.keep_at(level))
                        .unwrap_or(entries.len());
                    for entry in entries.iter().take(keep) {
                        out.push_str(entry);
                    }
                    if keep < entries.len() {
                        out.push_str(&omission(entries.len() - keep, entries.len(), noun));
                    }
                }
            }
        }
        out
    }
}

/// The line that ends a cut listing.
fn omission(omitted: usize, total: usize, noun: &str) -> String {
    format!("({omitted} of {total} {noun} omitted to fit the response budget)\n")
}

/// A listing's sizes as the caller measures them.
struct Measured {
    /// `ends[k]`: bytes of the first `k + 1` entries.
    ends: Vec<usize>,
    note: usize,
}

impl Measured {
    fn new(noun: &str, entries: &[String], measure: fn(&str) -> usize) -> Self {
        let ends = entries
            .iter()
            .scan(0usize, |end, entry| {
                *end = end.saturating_add(measure(entry));
                Some(*end)
            })
            .collect();
        let note = measure(&omission(entries.len(), entries.len(), noun));
        Self { ends, note }
    }

    /// Entries to keep at `level`, or `None` for all of them.
    fn keep_at(&self, level: usize) -> Option<usize> {
        budget::limit(self, level).map(|limit| self.ends.partition_point(|&end| end <= limit))
    }
}

impl Shrinkable for Measured {
    fn full(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    fn note(&self) -> usize {
        self.note
    }

    fn kept(&self, limit: usize) -> usize {
        let count = self.ends.partition_point(|&end| end <= limit);
        count
            .checked_sub(1)
            .and_then(|last| self.ends.get(last))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> PromptText {
        let mut prompt = PromptText::default();
        prompt.push_str("HEAD\n");
        prompt.push_listing("rules", vec!["rule 1\n".into(), "rule 2\n".into()]);
        prompt.push_str("MIDDLE\n");
        let decisions = (0..50).map(|i| format!("decision {i:02}\n")).collect();
        prompt.push_listing("decisions", decisions);
        prompt.push_str("PROTOCOL\n");
        prompt
    }

    #[test]
    fn a_prompt_that_fits_renders_whole() {
        let whole = prompt().render(usize::MAX, str::len);
        assert!(whole.starts_with("HEAD\nrule 1\nrule 2\nMIDDLE\ndecision 00\n"));
        assert!(whole.ends_with("decision 49\nPROTOCOL\n"));
        assert_eq!(prompt().render(whole.len(), str::len), whole);
    }

    #[test]
    fn a_cut_keeps_the_fixed_text_and_the_short_listing() {
        let text = prompt().render(200, str::len);

        assert!(text.len() <= 200, "{}", text.len());
        assert!(text.starts_with("HEAD\nrule 1\nrule 2\nMIDDLE\ndecision 00\n"));
        assert!(text.ends_with("omitted to fit the response budget)\nPROTOCOL\n"));
        let kept = text.matches("decision ").count();
        assert!(text.contains(&format!("({} of 50 decisions omitted", 50 - kept)));
    }

    #[test]
    fn listings_are_cut_to_their_omission_lines_when_the_text_alone_is_over() {
        let text = prompt().render(10, str::len);
        assert!(text.contains("(50 of 50 decisions omitted"));
        assert!(text.ends_with("PROTOCOL\n"));
    }

    #[test]
    fn the_measure_decides_what_fits() {
        // Counting every byte twice halves what the same budget holds.
        let single = prompt().render(300, str::len);
        let double = prompt().render(300, |text| text.len() * 2);
        assert!(double.matches("decision ").count() < single.matches("decision ").count());
        assert!(double.len() * 2 <= 300);
    }
}
