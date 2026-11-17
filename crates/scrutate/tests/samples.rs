//! Every language sample must yield exactly the symbols recorded in tests/samples/expected.json.
//! Run with UPDATE_EXPECTED=1 to re-record after an intentional rule change (review the diff!).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scrutate::parse::parse_source;

#[test]
fn samples_yield_the_recorded_symbols() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let expected_path = dir.join("expected.json");
    let mut got: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name == "expected.json" {
            continue;
        }
        let src = fs::read_to_string(&p).unwrap();
        let chunks = parse_source(&name, &src);
        got.insert(name, chunks.iter().map(|c| (c.kind.to_string(), c.qualname.clone())).collect());
    }
    if std::env::var("UPDATE_EXPECTED").is_ok() {
        fs::write(&expected_path, serde_json::to_string_pretty(&got).unwrap()).unwrap();
        return;
    }
    let expected: BTreeMap<String, Vec<(String, String)>> =
        serde_json::from_str(&fs::read_to_string(&expected_path).expect("expected.json (run with UPDATE_EXPECTED=1 once)")).unwrap();
    let mut failures = Vec::new();
    for (name, chunks) in &got {
        match expected.get(name) {
            Some(e) if e == chunks => {}
            Some(e) => failures.push(format!("{name}: expected {e:?}\n   got {chunks:?}")),
            None => failures.push(format!("{name}: no expectation recorded")),
        }
    }
    for name in expected.keys() {
        if !got.contains_key(name) {
            failures.push(format!("{name}: sample missing"));
        }
    }
    assert!(failures.is_empty(), "symbol regressions:\n{}", failures.join("\n"));
    assert!(got.len() >= 60, "fewer samples than expected: {}", got.len());
}

/// `languages --json` must name every kind the index gives a language's sample.
#[test]
fn the_registry_names_every_kind_a_sample_yields() {
    let mut missing = Vec::new();
    for sub in ["tests/samples", "tests/samples_extra"] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(sub);
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(src) = fs::read_to_string(&p) else { continue };
            let Some(spec) = scrutate::parse::spec_for(&name, &src) else { continue };
            let known = scrutate::langs::definition_kinds(spec);
            for c in parse_source(&name, &src) {
                if !matches!(c.kind, "module" | "window") && !known.contains(&c.kind) {
                    missing.push(format!("{}: {} ({name})", spec.id, c.kind));
                }
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "kinds the registry does not name:\n{}", missing.join("\n"));
}

/// A language whose registry entry declares call edges or imports shows them in its samples, so
/// docs/LANGUAGES.md's Calls and Imports columns are checked here as its kinds are above.
#[test]
fn every_sample_shows_the_call_edges_and_imports_its_language_declares() {
    let mut dirs = vec!["tests/samples"];
    if cfg!(feature = "extra-langs") {
        dirs.push("tests/samples_extra");
    }
    let mut missing = Vec::new();
    for sub in dirs {
        for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(sub)).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(src) = fs::read_to_string(&p) else { continue };
            let Some(spec) = scrutate::parse::spec_for(&name, &src) else { continue };
            let parsed = scrutate::parse::parse_file(&name, &src);
            if !spec.calls.is_empty() && parsed.chunks.iter().all(|c| c.calls.is_empty()) {
                missing.push(format!("{}: no call edge ({sub}/{name})", spec.id));
            }
            if !spec.imports.is_empty() && parsed.imports.is_empty() {
                missing.push(format!("{}: no import ({sub}/{name})", spec.id));
            }
        }
    }
    missing.sort();
    assert!(missing.is_empty(), "declared but not shown by the sample:\n{}", missing.join("\n"));
}

#[test]
fn a_racket_define_syntax_rule_is_a_macro() {
    let chunks = parse_source("m.rkt", "#lang racket\n(define-syntax-rule (twice e) (begin e e))\n(define (f x) (twice x))\n");
    let found: Vec<(&str, &str)> = chunks.iter().map(|c| (c.kind, c.name.as_str())).collect();
    assert!(found.contains(&("macro", "twice")) && found.contains(&("function", "f")), "{found:?}");
}

#[test]
fn a_gdscript_comment_below_the_class_header_documents_the_class_unless_a_member_follows_it() {
    let doc = |src: &str, name: &str| parse_source("a.gd", src).into_iter().find(|c| c.name == name).map(|c| (c.doc, c.start, c.end));
    let below = "class_name Hero\nextends Node\n## Walks the map.\n\nfunc go():\n\tpass\n";
    assert_eq!(doc(below, "Hero"), Some(("Walks the map.".into(), 1, 3)));
    let above = "## Walks the map.\nclass_name Hero extends Node\n## Starts walking.\nfunc go():\n\tpass\n";
    assert_eq!(doc(above, "Hero"), Some(("Walks the map.".into(), 2, 2)));
    let member = "class_name Hero\nextends Node\n## Starts walking.\nfunc go():\n\tpass\n";
    assert_eq!(doc(member, "Hero"), Some((String::new(), 1, 2)));
    assert_eq!(doc(member, "go").map(|d| d.0), Some("Starts walking.".into()));
}

#[test]
fn every_text_file_is_searchable_even_without_symbols() {
    let chunks = parse_source("notes.whatever", "line one\nline two\n");
    assert!(!chunks.is_empty());
    let windows = parse_source("data.unknown", &"x\n".repeat(200));
    assert!(windows.iter().all(|c| c.kind == "window" || c.kind == "module"));
}

/// Every line of a sample that holds more than brackets or a closing word is in the kept text of
/// a chunk `search` reads (constants have no postings), or in the documentation right above one.
#[test]
fn every_line_of_a_sample_is_in_a_searchable_chunk() {
    let closer = |t: &str| matches!(t.trim_end_matches([';', ',']), "end" | "fi" | "done" | "esac") || t.chars().all(|c| "{}[]();,".contains(c));
    let comment = ["#", "//", "--", ";", "%", "*", "/*", "\"\"\"", "'''", "<!--", "{-", "\"", "{"];
    let mut dirs = vec!["tests/samples"];
    if cfg!(feature = "extra-langs") {
        dirs.push("tests/samples_extra");
    }
    let mut left = Vec::new();
    for sub in dirs {
        for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(sub)).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(src) = fs::read_to_string(&p) else { continue };
            if name == "expected.json" {
                continue;
            }
            let lines = scrutate::text::split_lines(&src);
            let mut covered = vec![false; lines.len() + 2];
            for c in parse_source(&name, &src).iter().filter(|c| c.kind != "constant") {
                let kept = c.text.lines().count() - c.text.ends_with("\n...") as usize;
                for l in c.start..=c.end.min(c.start + kept as i64 - 1) {
                    covered[l as usize] = true;
                }
                let mut l = c.start as usize - 1;
                while !c.doc.is_empty() && l >= 1 && comment.iter().any(|m| lines[l - 1].trim().starts_with(m)) {
                    covered[l] = true;
                    l -= 1;
                }
            }
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim();
                if !t.is_empty() && !closer(t) && !covered[i + 1] {
                    left.push(format!("{sub}/{name}:{}: {t}", i + 1));
                }
            }
        }
    }
    assert!(left.is_empty(), "lines in no searchable chunk:\n{}", left.join("\n"));
}
