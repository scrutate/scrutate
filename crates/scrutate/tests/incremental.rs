//! Incremental parsing must give exactly what a fresh parse gives: every language sample is edited
//! many times in small random ways, and after each edit the result of reparsing against the
//! previous tree is compared with a parse from scratch.

use std::fs;
use std::path::Path;

use scrutate::parse::{PrevParse, parse_file_with};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const SNIPPETS: &[&str] = &[
    "x",
    "_",
    "1",
    " ",
    "\n",
    "\n\n",
    "\t",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    ";",
    ",",
    ".",
    ":",
    "=",
    "\"",
    "'",
    "`",
    "#",
    "//",
    "/*",
    "*/",
    "--",
    "\\",
    "<",
    ">",
    "é",
    "日本",
    "fn f() {}\n",
    "def g():\n    pass\n",
    "class K:\n",
    "function h() { return 1; }\n",
    "import os\n",
    "\"\"\"doc\"\"\"",
    "// note\n",
    "# note\n",
    "/* c */",
    "end\n",
    "x = call(y)\n",
    "    ",
];

fn char_floor(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A position in a string or comment half of the time, anywhere otherwise.
fn pick_pos(rng: &mut Rng, s: &str) -> usize {
    if rng.below(2) == 0 {
        let marks: Vec<usize> = s.match_indices(['"', '\'', '#', '/', '-', ';', '%']).map(|(i, _)| i + 1).collect();
        if !marks.is_empty() {
            return marks[rng.below(marks.len())];
        }
    }
    char_floor(s, rng.below(s.len() + 1))
}

fn edit(rng: &mut Rng, s: &str) -> String {
    let a = pick_pos(rng, s);
    let b = char_floor(s, (a + 1 + rng.below(24)).min(s.len()));
    let ins = SNIPPETS[rng.below(SNIPPETS.len())];
    match rng.below(3) {
        0 => format!("{}{}{}", &s[..a], ins, &s[a..]),
        1 => format!("{}{}", &s[..a], &s[b..]),
        _ => format!("{}{}{}", &s[..a], ins, &s[b..]),
    }
}

fn dump(path: &str, src: &str, prev: Option<PrevParse>) -> (String, Option<PrevParse>) {
    let (p, tree) = parse_file_with(path, src, prev);
    (format!("{:?}\n{:?}", p.chunks, p.imports), tree)
}

#[test]
fn incremental_parse_equals_fresh_parse() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let steps: usize = std::env::var("SCRUTATE_INC_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    let seed: u64 = std::env::var("SCRUTATE_INC_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let (mut checked, mut reused) = (0, 0);
    for (n, p) in files.iter().enumerate() {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name == "expected.json" {
            continue;
        }
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (n as u64 + 1) ^ seed.wrapping_mul(0x0100_0000_01B3));
        let orig = fs::read_to_string(p).unwrap();
        let mut src = orig.clone();
        let (_, mut prev) = dump(&name, &src, None);
        for step in 0..steps {
            let before = src.clone();
            // going back to the original now and then keeps many states free of syntax errors,
            // which is when a previous tree is kept
            src = if rng.below(3) == 0 { orig.clone() } else { edit(&mut rng, &src) };
            reused += prev.is_some() as usize;
            let (inc, next) = dump(&name, &src, prev.take());
            let (fresh, _) = dump(&name, &src, None);
            assert!(inc == fresh, "{name}: step {step} differs\n--- before\n{before}\n--- after\n{src}");
            prev = next;
            checked += 1;
        }
    }
    assert!(checked > 5000 && reused > 500, "checked {checked}, reused a tree {reused} times");
}
