/// Normalise a company/director name to a canonical slug form for matching.
///
/// Two-pass normalisation:
///  1. Remove intra-word elision chars (apostrophe, curly apostrophe, hyphen)
///     so "O'Brien" → "obrien".
///  2. Replace remaining punctuation with spaces so "acme.ltd" → "acme ltd".
///
/// Then strips common legal-entity suffixes (limited/ltd/plc/llp/llc) and
/// collapses whitespace.
///
/// Used for cross-LPA Phoenix detection (`director:{slug}` keying) and
/// CH backfill name matching.
pub fn normalise_name(name: &str) -> String {
    fn strip_suffix(s: &str, suffix: &str) -> String {
        if s == suffix {
            return String::new();
        }
        let with_space = format!(" {suffix}");
        if s.ends_with(&with_space) {
            s[..s.len() - with_space.len()].to_string()
        } else {
            s.to_string()
        }
    }
    let lower = name.to_lowercase();
    let elided: String = lower
        .chars()
        .filter(|&c| c != '\'' && c != '\u{2019}' && c != '-')
        .collect();
    let spaced: String = elided
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    let s = spaced.trim().to_string();
    let s = strip_suffix(&s, "limited");
    let s = strip_suffix(&s, "ltd");
    let s = strip_suffix(&s, "plc");
    let s = strip_suffix(&s, "llp");
    let s = strip_suffix(&s, "llc");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalise_strips_ltd() {
        assert_eq!(normalise_name("ACME Ltd"), "acme");
    }

    #[test]
    fn test_normalise_strips_limited() {
        assert_eq!(normalise_name("Apex Build Limited"), "apex build");
    }

    #[test]
    fn test_normalise_strips_plc() {
        assert_eq!(normalise_name("Big Corp Plc"), "big corp");
    }

    #[test]
    fn test_normalise_strips_punctuation() {
        assert_eq!(
            normalise_name("O'Brien Construction Ltd."),
            "obrien construction"
        );
    }

    #[test]
    fn test_normalise_collapses_whitespace() {
        assert_eq!(normalise_name("  A  B  Ltd  "), "a b");
    }

    #[test]
    fn test_normalise_strips_llp() {
        // '&' becomes a space, then whitespace-collapse joins "smith" + "jones"
        assert_eq!(normalise_name("Smith & Jones LLP"), "smith jones");
    }

    #[test]
    fn test_normalise_strips_llc() {
        assert_eq!(normalise_name("Acme Holdings LLC"), "acme holdings");
    }

    #[test]
    fn test_normalise_curly_apostrophe() {
        assert_eq!(normalise_name("O\u{2019}Brien Ltd"), "obrien");
    }

    #[test]
    fn test_normalise_hyphen_removal() {
        assert_eq!(normalise_name("Well-Being Ltd"), "wellbeing");
    }

    #[test]
    fn test_normalise_empty() {
        assert_eq!(normalise_name(""), "");
    }

    #[test]
    fn test_normalise_only_suffix() {
        assert_eq!(normalise_name("Ltd"), "");
    }
}
