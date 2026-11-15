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

/// The marks a name of the language may end with: Ruby's `valid?`, Julia's `push!`.
pub fn trailing(lang: &str) -> &'static str {
    match lang {
        "ruby" | "elixir" => "?!",
        "julia" => "!",
        _ => "",
    }
}

/// Whether `c` right before a name makes a longer name of it: a name character, or the `$` a
/// JavaScript, TypeScript or Java name can start with (`$init`). Scala's and Groovy's `$` opens
/// an interpolation (`s"v$version"`), PHP's is a variable's sigil (`$current` is `current`), and a
/// `'` before a Haskell name quotes it (`'helper`): none of them is part of the name.
pub fn joins_before(lang: &str, c: char) -> bool {
    is_continue(c) || c == '$' && matches!(lang, "javascript" | "typescript" | "tsx" | "java")
}

/// Whether `c` right after a name makes a longer name of it: `joins_before`'s, or a prime
/// (`foo'` in Haskell, OCaml, F# and Nix). PHP's `\` joins the parts of a qualified name, each a
/// name of its own.
pub fn joins_after(lang: &str, c: char) -> bool {
    joins_before(lang, c) || c == '\'' && extra(lang).contains('\'')
}

/// Whether `text[a..b]` is part of a longer name of the language at either end (`joins_before`,
/// `joins_after`): `foo` in `foo'`, `init` in `$init`, `valid` in `valid?`. A mark before `=` or
/// `~` is an operator (`a != b`).
pub fn inside_name(lang: &str, text: &str, a: usize, b: usize) -> bool {
    let (Some(before), Some(rest)) = (text.get(..a), text.get(b..)) else { return false };
    let mut after = rest.chars();
    before.chars().next_back().is_some_and(|c| joins_before(lang, c))
        || match after.next() {
            Some(c) if joins_after(lang, c) => true,
            Some(c) if trailing(lang).contains(c) => !after.next().is_some_and(|n| n == '=' || n == '~'),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `line` holds a regex (a raw string, not in a comment) with a character class that
    /// spells out ASCII letters, in any order (`[A-Za-z_]`, `[_a-zA-Z]`): a name rule by hand.
    fn ascii_letter_class(line: &str) -> bool {
        static CLASS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            let (lo, up) = (["a", "-z"].concat(), ["A", "-Z"].concat());
            regex::Regex::new(&format!(r"\[[^\]]*(?:{lo}[^\]]*{up}|{up}[^\]]*{lo})[^\]]*\]")).unwrap()
        });
        (line.contains("r\"") || line.contains("r#\"")) && !line.trim_start().starts_with("//") && CLASS.is_match(line)
    }

    #[test]
    fn an_ascii_letter_class_is_caught_in_every_spelling() {
        for hand in [
            r#"Regex::new(r"^[A-Za-z_]\w*")"#,
            r#"Regex::new(r"[a-zA-Z_][\w$]*")"#,
            r#"Regex::new(r"[_a-zA-Z]")"#,
            r#"Regex::new(r"[A-Z_a-z]")"#,
            r#"Regex::new(r"[a-zA-Z$_]")"#,
        ] {
            assert!(ascii_letter_class(hand), "{hand}");
        }
        for fine in [
            r#"Regex::new(r"[\p{XID_Start}_]")"#,
            r#"Regex::new(r"[a-z]+")"#,
            r#"Regex::new(r"[0-9a-fA-F]")"#,
            "// [A-Za-z_] in a comment",
            r#""[A-Za-z_]" not raw"#,
        ] {
            assert!(!ascii_letter_class(fine), "{fine}");
        }
    }

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
        // the one rule that splits module paths, where `-` stays inside a package name, the
        // regexes of POSIX shell names, which are ASCII by spec, and the secret detectors, whose
        // token formats are ASCII as their issuers define them
        const ALLOWED: &[(&str, &str)] = &[("names.rs", "c == '-'"), ("parse.rs", "BASH_NAME"), ("parse.rs", "SHELL_FN"), ("secrets.rs", "")];
        let rule = regex::Regex::new(r"is_alphanumeric\(\)\s*\|\|\s*\w+\s*==\s*'_'|is_alphabetic\(\)\s*\|\|\s*\w+\s*==\s*'_'").unwrap();
        let mut found = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().map(Result::unwrap) {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") && p.file_name().is_some_and(|f| f != "ident.rs") {
                    let file = p.file_name().unwrap().to_string_lossy().to_string();
                    for (i, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                        if (rule.is_match(line) || ascii_letter_class(line)) && !ALLOWED.iter().any(|(f, s)| *f == file && line.contains(s)) {
                            found.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "use crate::ident in place of a hand-written name rule: {found:?}");
    }

    #[test]
    fn a_name_inside_a_longer_one_is_told_apart_per_language() {
        assert!(inside_name("haskell", "foo' 1", 0, 3) && !inside_name("python", "foo' 1", 0, 3));
        assert!(inside_name("javascript", "$init()", 1, 5) && inside_name("julia", "push!(x)", 0, 4));
        assert!(!inside_name("ruby", "a != b", 0, 1) && !inside_name("ruby", "a!=b", 0, 1) && inside_name("ruby", "a? b", 0, 1));
        assert!(!inside_name("php", r"\App\User", 5, 9), "a part of a qualified name is a name");
        assert!(!inside_name("php", "User::$current", 7, 14), "a sigil is not part of the name");
        assert!(!inside_name("scala", "s\"v$version\"", 4, 11) && !inside_name("groovy", "\"v$release\"", 3, 10), "an interpolation");
        assert!(!inside_name("haskell", "varE 'helper", 6, 12) && inside_name("haskell", "helper' x", 0, 6), "a quote before, a prime after");
        for lang in ["ocaml", "fsharp", "nix"] {
            assert!(inside_name(lang, "foo' 1", 0, 3) && !inside_name(lang, "'foo", 1, 4), "{lang}: a prime joins after a name only");
        }
    }

    #[test]
    fn the_trailing_marks_are_the_ones_the_name_rule_keeps() {
        for lang in ["ruby", "elixir", "julia", "python", "haskell"] {
            for mark in ['?', '!'] {
                let kept = crate::parse::name_rule(lang).find(&format!("f{mark}")).is_some_and(|m| m.as_str().len() == 2);
                assert_eq!(trailing(lang).contains(mark), kept, "{lang} {mark}");
            }
        }
    }

    #[test]
    fn dollar_prime_and_backslash_are_per_language() {
        assert!(is_continue_in("javascript", '$') && is_continue_in("typescript", '$') && is_continue_in("php", '$'));
        assert!(is_continue_in("php", '\\') && is_continue_in("ocaml", '\'') && is_continue_in("fsharp", '\''));
        assert!(!is_continue_in("python", '$') && !is_continue_in("python", '\'') && !is_continue('$'));
    }
}
