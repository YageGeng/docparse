//! Pure character and list-marker rules shared by extraction and presentation.
// SPDX-License-Identifier: Apache-2.0
// Adapted from LiteParse 4d4a51c246ff56d382166930942898f3ff563eba.

/// Maps known PDF control-code and Unicode presentation ligatures to their ordinary character sequences.
pub(crate) fn ligature_expansion(c: char) -> Option<&'static str> {
    match c {
        '\u{001B}' => Some("ft"),
        '\u{001D}' => Some("Th"),
        '\u{001A}' | '\u{FB00}' => Some("ff"),
        '\u{001C}' | '\u{FB01}' => Some("fi"),
        '\u{001F}' | '\u{FB02}' => Some("fl"),
        '\u{001E}' | '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// Folds the same typographic quotes, primes and dash family as LiteParse extraction.
pub(crate) fn normalize_punctuation(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        _ => c,
    }
}

/// Identifies invalid mappings and private-use codepoints while allowing ordinary layout whitespace.
pub(crate) fn suspicious_codepoint(value: u32) -> bool {
    char::from_u32(value).is_none()
        || matches!(value, 0 | 0xFFFD..=0xFFFF)
        || (value < 0x20 && !matches!(value, 0x09 | 0x0A | 0x0D))
        || (0x7F..=0x9F).contains(&value)
        || (0xE000..=0xF8FF).contains(&value)
        || (0xF0000..=0xFFFFD).contains(&value)
        || (0x100000..=0x10FFFD).contains(&value)
}

/// Recognizes source list markers only at line start and only when followed by whitespace.
pub(crate) fn is_list_marker(text: impl Iterator<Item = char>) -> bool {
    let mut chars = text.skip_while(|c| c.is_whitespace());
    let Some(first) = chars.next() else {
        return false;
    };
    if matches!(
        first,
        '-' | '*'
            | '–'
            | '•'
            | '·'
            | '◦'
            | '▪'
            | '▸'
            | '▶'
            | '●'
            | '○'
            | '■'
            | '□'
            | '\u{F0B7}'
    ) {
        return chars.next().is_some_and(char::is_whitespace);
    }
    if !first.is_ascii_digit() {
        return false;
    }
    let mut digits = 1;
    loop {
        match chars.next() {
            Some(c) if c.is_ascii_digit() && digits < 3 => digits += 1,
            Some('.' | ')') => {
                return chars.next().is_some_and(char::is_whitespace);
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    /// Recovered circle/square bullets and Unicode whitespace must establish list boundaries.
    #[test]
    fn recognizes_extended_list_markers() {
        for text in [
            "● item",
            "○\u{3000}item",
            "□ item",
            "\u{F0B7} item",
            "12)\titem",
        ] {
            assert!(super::is_list_marker(text.chars()), "{text:?}");
        }
        for text in ["●item", "5-10", "1234. item", "A. Background"] {
            assert!(!super::is_list_marker(text.chars()), "{text:?}");
        }
        assert!(super::is_list_marker(
            ["12", ")", "\titem"].into_iter().flat_map(str::chars)
        ));
    }
}
