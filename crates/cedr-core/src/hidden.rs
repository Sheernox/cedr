//! CG005: characters that render as nothing but are read by a model.

/// Every hidden character in `text` with its byte offset. A U+FEFF at
/// offset 0 is a byte-order mark and is allowed.
pub(crate) fn find_hidden(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    text.char_indices()
        .filter(|&(i, c)| is_hidden(c) && !(c == '\u{feff}' && i == 0))
}

pub(crate) fn is_hidden(c: char) -> bool {
    matches!(c,
        '\u{e0000}'..='\u{e007f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{200b}'..='\u{200d}'
        | '\u{2060}'
        | '\u{feff}')
}

pub(crate) fn describe(c: char) -> &'static str {
    match c {
        '\u{e0000}'..='\u{e007f}' => "tag character",
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => "bidi control",
        '\u{feff}' => "zero-width no-break space",
        _ => "zero-width character",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_each_class_but_not_a_leading_bom() {
        let text = "\u{feff}a\u{200b}b\u{202e}c\u{e0041}d\u{2060}e\u{feff}";
        let found: Vec<char> = find_hidden(text).map(|(_, c)| c).collect();
        assert_eq!(
            found,
            ['\u{200b}', '\u{202e}', '\u{e0041}', '\u{2060}', '\u{feff}']
        );
        assert_eq!(find_hidden("caf\u{e9} text").count(), 0);
    }
}
