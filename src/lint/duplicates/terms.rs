//! The terms of a decision's text: its `choice`, a space and its `reason`,
//! lowercased, each run of the regular expression `[a-z][a-z_]+`, the
//! stop words dropped. A digit, a hyphen or any non-ASCII letter ends a
//! run, so `ed25519` is the term `ed` and `kebab-case` two terms.

/// Whether `word` is too common to tell two decisions apart.
fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "by"
            | "can"
            | "each"
            | "for"
            | "from"
            | "has"
            | "have"
            | "in"
            | "is"
            | "it"
            | "its"
            | "not"
            | "of"
            | "on"
            | "or"
            | "that"
            | "the"
            | "their"
            | "there"
            | "these"
            | "they"
            | "this"
            | "to"
            | "was"
            | "were"
            | "which"
            | "with"
    )
}

/// Call `found` with each term of `choice` and `reason`, in text order,
/// repeats included.
pub(super) fn each_term(choice: &str, reason: &str, mut found: impl FnMut(&str)) {
    let mut run = String::new();
    // Joined by a space, as one text: the space ends the choice's last run.
    for text in [choice, reason] {
        for c in text.chars() {
            if c.is_ascii() {
                take(&mut run, c.to_ascii_lowercase(), &mut found);
            } else {
                // A few lowercase to ASCII: the Kelvin sign to `k`, U+0130
                // to `i` and a combining dot.
                for lower in c.to_lowercase() {
                    take(&mut run, lower, &mut found);
                }
            }
        }
        take(&mut run, ' ', &mut found);
    }
}

/// Add the lowercase character `lower` to the `run` it continues, or end
/// the run, calling `found` with it when it is a term.
fn take(run: &mut String, lower: char, found: &mut impl FnMut(&str)) {
    match lower {
        'a'..='z' => run.push(lower),
        // An underscore continues a run but cannot start one.
        '_' if !run.is_empty() => run.push(lower),
        '_' => {}
        _ => {
            if run.len() >= 2 && !is_stop_word(run) {
                found(run);
            }
            run.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(choice: &str, reason: &str) -> Vec<String> {
        let mut terms = Vec::new();
        each_term(choice, reason, |term| terms.push(term.to_owned()));
        terms
    }

    /// The script's 35 stop words, and none beside them.
    #[test]
    fn the_stop_words_are_the_scripts() {
        let listed = "a an and are as at be been by can each for from has have in is it its \
                      not of on or that the their there these they this to was were which with";
        let dropped = listed.split(' ').filter(|word| is_stop_word(word)).count();
        assert_eq!(dropped, 35);
        for kept in ["any", "than", "then", "those", "will", "would", "ab"] {
            assert!(!is_stop_word(kept), "{kept}");
        }
    }

    /// Each case as Python's `re.findall(r"[a-z][a-z_]+", text.lower())`
    /// splits it, the stop words then dropped.
    #[test]
    fn a_term_is_a_lowercased_run_of_letters_and_underscores() {
        let cases: [(&str, &str, &[&str]); 9] = [
            (
                "Sign tokens",
                "With Ed25519 keys",
                &["sign", "tokens", "ed", "keys"],
            ),
            ("a_b _c __init__ x_", "", &["a_b", "init__", "x_"]),
            ("x y z", "q", &[]),
            ("kebab-case", "snake_case", &["kebab", "case", "snake_case"]),
            ("THE Store", "is not IT", &["store"]),
            ("end", "start", &["end", "start"]),
            ("na\u{ef}ve caf\u{e9}", "", &["na", "ve", "caf"]),
            // U+0130 lowercases to `i` and a combining dot, which ends the run.
            ("\u{130}nput", "", &["nput"]),
            // The Kelvin sign lowercases to an ASCII `k`.
            ("\u{212a}ernel", "", &["kernel"]),
        ];
        for (choice, reason, expected) in cases {
            assert_eq!(terms(choice, reason), expected, "{choice:?} {reason:?}");
        }
    }

    #[test]
    fn the_choice_and_the_reason_never_join_into_one_term() {
        assert_eq!(terms("cache", "line"), ["cache", "line"]);
    }
}
