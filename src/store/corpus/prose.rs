//! The corpus's text, of the graph and of the source tree's doc comments:
//! sentences of a vocabulary of [`WORDS`] and words coined from
//! [`SYLLABLES`], each drawn by its rank's band ([`VOCABULARY_BANDS`]).

use std::ops::RangeInclusive;

use super::seeded::SplitMix64;
use super::words::WORDS;

/// Longer than any word drawn, so a sentence fits its room.
const SHORTEST_SENTENCE: usize = 16;

/// The vocabulary's ranks come in this many bands, the band `b` holding
/// `2^b` ranks: [`WORDS`] first, then words coined from [`SYLLABLES`],
/// 8,191 in all. A draw picks a band, then a rank in it, so rank `r` is
/// drawn about `1 / (13 (r + 1))` of the time. That gives a few words in
/// most texts and a long tail in few, as in prose; a uniform draw from a
/// short list makes every two texts alike.
const VOCABULARY_BANDS: usize = 13;

/// What a coined word is spelled with: none of them a stop word of
/// `lint::duplicates`, so a coined word is always a term.
const SYLLABLES: [&str; 16] = [
    "ba", "de", "fi", "go", "ku", "la", "me", "no", "pu", "ri", "sa", "te", "vo", "wu", "xe", "zo",
];

const SENTENCE_BYTES: RangeInclusive<usize> = 40..=120;

/// Set `word` to the word of the vocabulary at `rank`: an entry of
/// [`WORDS`], then a word coined from [`SYLLABLES`]. A coined word spells
/// the digits of a number from 256 on in base 16, so it has three
/// syllables or more and no two are spelled alike.
fn spell(rank: usize, word: &mut String) {
    word.clear();
    if let Some(known) = WORDS.get(rank) {
        word.push_str(known);
        return;
    }
    let base = SYLLABLES.len();
    let mut coined = rank - WORDS.len() + base * base;
    while coined > 0 {
        let syllable = SYLLABLES.get(coined % base);
        word.push_str(syllable.copied().unwrap_or_default());
        coined /= base;
    }
}

/// The corpus's text, drawn from the vocabulary of [`VOCABULARY_BANDS`].
impl SplitMix64 {
    pub(super) fn prose_within(&mut self, bytes: &RangeInclusive<usize>) -> String {
        let length = self.within(bytes);
        self.prose(length)
    }

    pub(super) fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[self.below(words.len())]
    }

    /// Sentences of the vocabulary, at most `bytes` long, on one line.
    fn prose(&mut self, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes);
        while bytes.saturating_sub(text.len()) > SHORTEST_SENTENCE {
            if !text.is_empty() {
                text.push(' ');
            }
            let room = bytes - text.len() - 1;
            let length = self.within(&SENTENCE_BYTES).min(room);
            text.push_str(&self.sentence(length));
        }
        text
    }

    /// A count of sentences of the vocabulary in `count`, on one line.
    pub(super) fn sentences(&mut self, count: &RangeInclusive<usize>) -> String {
        let count = self.within(count);
        let sentences: Vec<String> = (0..count)
            .map(|_| {
                let length = self.within(&SENTENCE_BYTES);
                self.sentence(length)
            })
            .collect();
        sentences.join(" ")
    }

    /// A phrase of at most `bytes`, capitalized, and a full stop.
    fn sentence(&mut self, bytes: usize) -> String {
        let phrase = self.phrase(bytes);
        let mut letters = phrase.chars();
        let mut sentence: String = letters
            .next()
            .map(|c| c.to_ascii_uppercase())
            .into_iter()
            .collect();
        sentence.push_str(letters.as_str());
        sentence.push('.');
        sentence
    }

    /// Lowercase words of the vocabulary, at most `bytes` long unless the
    /// first word alone is longer.
    pub(super) fn phrase(&mut self, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes);
        let mut word = String::new();
        loop {
            self.word(&mut word);
            if text.is_empty() {
                text.push_str(&word);
            } else if text.len() + 1 + word.len() <= bytes {
                text.push(' ');
                text.push_str(&word);
            } else {
                return text;
            }
        }
    }

    /// Set `word` to a word of the vocabulary, drawn by its rank's band.
    fn word(&mut self, word: &mut String) {
        let band = 1 << self.below(VOCABULARY_BANDS);
        spell(band - 1 + self.below(band), word);
    }

    /// A reason of about `bytes` that restates `earlier`: its first half,
    /// cut at a space, then sentences of its own.
    pub(super) fn restate(&mut self, earlier: &str, bytes: usize) -> String {
        let half = earlier.get(..earlier.len() / 2).unwrap_or(earlier);
        let kept = half.rfind(' ').and_then(|space| half.get(..space));
        let mut text = kept.unwrap_or(half).to_owned();
        let rest = self.prose(bytes.saturating_sub(text.len() + 1));
        if !rest.is_empty() {
            text.push(' ');
            text.push_str(&rest);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Every rank the bands reach is a word of its own: 8,191 words, the
    /// coined ones of three syllables or more, longer than no sentence's
    /// least room.
    #[test]
    fn the_vocabulary_holds_one_word_per_rank() {
        let ranks = (1 << VOCABULARY_BANDS) - 1;
        let mut word = String::new();
        let words: BTreeSet<String> = (0..ranks)
            .map(|rank| {
                spell(rank, &mut word);
                assert!(word.len() < SHORTEST_SENTENCE, "{word}");
                word.clone()
            })
            .collect();
        assert_eq!(words.len(), 8191);
        spell(WORDS.len(), &mut word);
        assert_eq!(word.len(), 6);
    }
}
