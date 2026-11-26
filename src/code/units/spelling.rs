//! How a string's delimiter, an escape of its text and a number are
//! hashed, each by what it means, so the spellings a formatter picks
//! between hash alike and no two of different meaning do.

/// How a string's delimiter `token` is hashed, by what it means: its
/// prefix's letters lowercased, sorted and without `u`, which means
/// nothing, then its quotes, each `'` as `"`. So `u'a'`, `"a"` and `'a'`
/// hash alike, as do `Rb'a'` and `br"a"`.
pub(super) fn delimiter(token: &[u8]) -> Vec<u8> {
    let (letters, quotes): (Vec<u8>, Vec<u8>) =
        token.iter().partition(|byte| byte.is_ascii_alphabetic());
    let mut spelled: Vec<u8> = letters
        .iter()
        .map(u8::to_ascii_lowercase)
        .filter(|letter| *letter != b'u')
        .collect();
    spelled.sort_unstable();
    spelled.extend(quotes.iter().map(|quote| match quote {
        b'\'' => b'"',
        other => *other,
    }));
    spelled
}

/// The bytes of `token`, a raw string's end, that are escapes before its
/// quotes (`\'` of `r'\''`): the string's text, as written.
pub(super) fn escapes_before_quotes(token: &[u8]) -> usize {
    let mut escapes = 0;
    while token.get(escapes) == Some(&b'\\') {
        escapes += 2;
    }
    escapes.min(token.len())
}

/// How `token`, an escape of a string that is not raw, is hashed, by what
/// it means: `\'` as `'` and `\"` as `"`, the digits of a hex escape and
/// the name of a `\N{...}` lowercased, an octal one as three digits, any
/// other as written. Each spelling stands for one escape, whatever text
/// follows it (`\0` then `1` is `\0001`, `\01` is `\001`), so the text of
/// two joined strings is spelled as their value.
pub(super) fn escape(token: &[u8]) -> Vec<u8> {
    match token {
        [b'\\', quote @ (b'\'' | b'"')] => vec![*quote],
        [b'\\', b'x' | b'u' | b'U' | b'N', rest @ ..] => {
            let mut spelled = token.get(..2).unwrap_or_default().to_vec();
            spelled.extend(rest.iter().map(u8::to_ascii_lowercase));
            spelled
        }
        // tree-sitter-python's escape takes up to three digits, Python
        // the octal ones among them; a digit after them is text.
        [b'\\', digits @ ..] if digits.first().is_some_and(is_octal) => {
            let octal = digits.iter().take_while(|digit| is_octal(digit)).count();
            let mut spelled = vec![b'\\'];
            spelled.extend(std::iter::repeat_n(b'0', 3_usize.saturating_sub(octal)));
            spelled.extend_from_slice(digits);
            spelled
        }
        // A one-letter escape, `\\` and a line continuation: one spelling.
        _ => token.to_vec(),
    }
}

const fn is_octal(byte: &u8) -> bool {
    matches!(byte, b'0'..=b'7')
}

/// How a number `token` is hashed, by what it means: its letters
/// lowercased, a `0` where a `.` has no digit before or after it, and no
/// `+` after its exponent's `e`. So `0XAB`, `0xab` and `0xAB` hash alike,
/// as do `1.` and `1.0`, and `1e+5` and `1e5`; `1` and `1.0` stay apart,
/// as do `1e5` and `1e-5`.
pub(super) fn literal(token: &[u8]) -> Vec<u8> {
    let mut spelled = Vec::with_capacity(token.len() + 2);
    for (at, byte) in token.iter().enumerate() {
        let before = at.checked_sub(1).and_then(|at| token.get(at));
        if *byte == b'+' && before.is_some_and(|before| before.eq_ignore_ascii_case(&b'e')) {
            continue;
        }
        let digit = |at: Option<usize>| {
            at.and_then(|at| token.get(at))
                .is_some_and(u8::is_ascii_digit)
        };
        if *byte == b'.' && !digit(at.checked_sub(1)) {
            spelled.push(b'0');
        }
        spelled.push(byte.to_ascii_lowercase());
        if *byte == b'.' && !digit(Some(at + 1)) {
            spelled.push(b'0');
        }
    }
    spelled
}
