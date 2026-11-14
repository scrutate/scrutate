//! What a character of a name is, per UAX #31 (XID_Start, XID_Continue), so every reader of
//! names (dead, refs, the call graph, the language server) splits the same text the same way.

/// A character a name can start with: XID_Start or `_`.
pub fn is_start(c: char) -> bool {
    c == '_' || unicode_ident::is_xid_start(c)
}

/// A character a name can go on with: XID_Continue (letters, marks such as a virama, digits, `_`).
pub fn is_continue(c: char) -> bool {
    unicode_ident::is_xid_continue(c)
}

/// The characters a language allows in a name past UAX #31, by language id.
pub fn extra(lang: &str) -> &'static str {
    match lang {
        "javascript" | "typescript" | "tsx" | "java" | "scala" | "groovy" => "$",
        // `\` joins the parts of a qualified name (`App\Models\User`)
        "php" => "$\\",
        // a prime is a name character (`x'`, `fold_left'`)
        "haskell" | "ocaml" | "ocaml_interface" | "fsharp" | "nix" => "'",
        _ => "",
    }
}

/// `is_continue`, or one of the language's `extra` characters.
pub fn is_continue_in(lang: &str, c: char) -> bool {
    is_continue(c) || extra(lang).contains(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(s: &str) -> bool {
        s.chars().next().is_some_and(is_start) && s.chars().all(is_continue)
    }

    #[test]
    fn a_virama_stays_in_the_word() {
        assert!(word("हिन्दी"), "Devanagari virama U+094D");
        assert!(word("हिंदी"));
    }

    #[test]
    fn bengali_and_thai_marks_stay_in_the_word() {
        assert!(word("সংখ্যা"), "Bengali vowel sign and virama");
        assert!(word("ชื่อ"), "Thai vowel and tone marks");
        assert!(word("größe") && word("_private") && word("x1"));
    }

    #[test]
    fn a_digit_or_a_mark_does_not_start_a_name() {
        assert!(!is_start('1') && !is_start('\u{094D}') && !is_start('-'));
        assert!(is_continue('1') && !is_continue('-') && !is_continue('.') && !is_continue(' '));
    }

    #[test]
    fn no_hand_written_name_rule_in_the_source() {
        // the one rule that splits module paths, where `-` stays inside a package name, and the
        // regexes of POSIX shell names, which are ASCII by spec
        const ALLOWED: &[(&str, &str)] = &[("names.rs", "c == '-'"), ("parse.rs", "BASH_NAME"), ("parse.rs", "SHELL_FN")];
        // a name class in a regex that starts ASCII-only (split so this line does not match itself)
        let ascii_class = ["[A-Z", "a-z_"].concat();
        let rule = regex::Regex::new(r"is_alphanumeric\(\)\s*\|\|\s*\w+\s*==\s*'_'|is_alphabetic\(\)\s*\|\|\s*\w+\s*==\s*'_'").unwrap();
        let mut found = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().map(Result::unwrap) {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let file = p.file_name().unwrap().to_string_lossy().to_string();
                    for (i, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                        let ascii_regex =
                            line.contains(&ascii_class) && (line.contains("r\"") || line.contains("r#\"")) && !line.trim_start().starts_with("//");
                        if (rule.is_match(line) || ascii_regex) && !ALLOWED.iter().any(|(f, s)| *f == file && line.contains(s)) {
                            found.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "use crate::ident in place of a hand-written name rule: {found:?}");
    }

    #[test]
    fn dollar_prime_and_backslash_are_per_language() {
        assert!(is_continue_in("javascript", '$') && is_continue_in("typescript", '$') && is_continue_in("php", '$'));
        assert!(is_continue_in("php", '\\') && is_continue_in("ocaml", '\'') && is_continue_in("fsharp", '\''));
        assert!(!is_continue_in("python", '$') && !is_continue_in("python", '\'') && !is_continue('$'));
    }
}
