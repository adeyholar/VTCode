pub(crate) fn display_error(message: &str) {
    tracing::error!("{message}");
}

/// Trim surrounding punctuation, retaining parenthesized source locations.
pub(crate) fn trim_transcript_token_bounds(token: &str) -> (usize, usize) {
    let mut start = 0usize;
    let mut end = token.len();

    while start < end {
        let Some(ch) = token[start..end].chars().next() else {
            break;
        };
        if matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '`') {
            start += ch.len_utf8();
        } else {
            break;
        }
    }

    while start < end {
        let Some(ch) = token[start..end].chars().next_back() else {
            break;
        };
        if matches!(ch, ')' | ']' | '}' | '>' | '"' | '\'' | '`' | ',' | ';' | '.' | '!' | '?') {
            // Preserve trailing ')' when it looks like a location suffix e.g. file.rs(10,5)
            if ch == ')' && location_paren_suffix_start(&token[start..end]).is_some() {
                break;
            }
            end -= ch.len_utf8();
        } else {
            break;
        }
    }

    (start, end)
}

fn location_paren_suffix_start(token: &str) -> Option<usize> {
    let paren_start = token.rfind('(')?;
    let inner = token[paren_start + 1..].strip_suffix(')')?;
    // Accept `(digits)` or `(digits,digits)` — reject empty or malformed like `(,)` `(10,,5)`
    let valid = !inner.is_empty()
        && !inner.starts_with(',')
        && !inner.ends_with(',')
        && !inner.contains(",,")
        && inner.chars().all(|c| c.is_ascii_digit() || c == ',');
    valid.then_some(paren_start)
}

#[cfg(test)]
mod tests {
    use super::trim_transcript_token_bounds;

    #[test]
    fn token_bounds_preserve_unicode_and_valid_location_suffixes() {
        for (token, expected) in [
            ("[`src/日本.rs(10,5)`].", "src/日本.rs(10,5)"),
            ("(src/é.rs)", "src/é.rs"),
            ("\"src/é.rs(10)\",", "src/é.rs(10)"),
            ("src/é.rs(10,,5).", "src/é.rs(10,,5"),
            ("src/é.rs(,).", "src/é.rs("),
            ("src/é.rs().", "src/é.rs("),
            ("src/é.rs", "src/é.rs"),
            ("[``]", ""),
            ("", ""),
        ] {
            let (start, end) = trim_transcript_token_bounds(token);
            assert_eq!(&token[start..end], expected, "token: {token}");
        }
    }
}
