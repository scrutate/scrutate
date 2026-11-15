//! Ranking's arithmetic, pinned: on a small fixed tree, with the bundled vector model and ranker,
//! every candidate, feature and score of a few queries is the one recorded in `ranking_pins.json`.
//! A change that moves a feature in its last bit fails here, long before the quality floor of
//! `bench/quality.py` moves. Record again on purpose with `UPDATE_PINS=1 cargo test --test ranking_pins`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
const PINS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ranking_pins.json");

// Short files, so no token is counted more than a few times in a chunk: ln of a small count is
// rounded the same by every libm, and the other arithmetic is IEEE basic operations.
const FILES: &[(&str, &str)] = &[
    (
        "shop/cart.py",
        "class Cart:\n    \"\"\"A basket of items a customer is about to buy.\"\"\"\n\n    def total_price(self, items):\n        \"\"\"Sum the prices of the items in the cart.\"\"\"\n        return sum(i.price for i in items)\n\n    def remove_item(self, item):\n        self.items.remove(item)\n\n\ndef parse_size(text):\n    \"\"\"Read a size such as 10M into bytes.\"\"\"\n    return int(text[:-1]) * 1024 * 1024\n",
    ),
    ("shop/test_cart.py", "def test_total_price():\n    assert Cart().total_price([]) == 0\n"),
    (
        "src/config.rs",
        "/// Settings read from the configuration file.\npub struct Config {\n    pub retries: u32,\n}\n\nimpl Config {\n    /// Load the settings from a TOML file on disk.\n    pub fn load(path: &str) -> Config {\n        let text = std::fs::read_to_string(path).unwrap();\n        parse(&text)\n    }\n}\n\nfn parse(text: &str) -> Config {\n    Config { retries: text.len() as u32 }\n}\n",
    ),
    (
        "net/client.go",
        "package net\n\n// Fetch downloads a page and returns its body.\nfunc Fetch(url string) (string, error) {\n\treturn get(url)\n}\n\n// Retry calls f until it succeeds or the attempts run out.\nfunc Retry(attempts int, f func() error) error {\n\treturn f()\n}\n",
    ),
    (
        "web/router.js",
        "/** Match a request path against the registered routes. */\nexport function matchRoute(routes, path) {\n  return routes.find((r) => r.path === path);\n}\n\nexport class Router {\n  add(route) {\n    this.routes.push(route);\n  }\n}\n",
    ),
];

const QUERIES: &[&str] =
    &["total price of the cart", "parse_size", "load settings from a file", "Retry", "match a route for a request path", "download a web page"];

fn tree() -> PathBuf {
    let root = std::env::temp_dir().join(format!("scrutate_pins_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    // a repository: no index in a directory above takes it over
    fs::create_dir_all(root.join(".git")).unwrap();
    for (rel, text) in FILES {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    root
}

fn scrutate(root: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .arg("--root")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .env_remove("SCRUTATE_SEMANTIC")
        .env_remove("SCRUTATE_RANKER")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// Per query: the hits, and each candidate with its score and features. Numbers are printed in the
/// shortest form that reads back to the same float, so equal JSON is equal bits.
fn measure(root: &Path) -> serde_json::Value {
    scrutate(root, &["index", "--no-text"]);
    // outside the tree, so the queries are not indexed
    let q = root.with_extension("jsonl");
    fs::write(&q, QUERIES.iter().map(|t| format!("{}\n", serde_json::json!({"query": t, "k": 5}))).collect::<String>()).unwrap();
    let recs: Vec<serde_json::Value> =
        scrutate(root, &["batch", "--features", q.to_str().unwrap()]).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let _ = fs::remove_file(&q);
    let names = recs[0]["feature_names"].clone();
    let queries: Vec<serde_json::Value> = recs
        .iter()
        .map(|r| {
            let hits: Vec<_> = r["hits"].as_array().unwrap().iter().map(|h| format!("{}:{}", h["path"].as_str().unwrap(), h["start"])).collect();
            let cands: Vec<_> = r["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| serde_json::json!({"at": format!("{}:{}", c["path"].as_str().unwrap(), c["qualname"].as_str().unwrap()), "score": c["score"], "x": c["x"]}))
                .collect();
            serde_json::json!({"query": r["query"], "hits": hits, "candidates": cands})
        })
        .collect();
    serde_json::json!({"feature_names": names, "queries": queries})
}

#[test]
fn the_features_and_scores_of_a_small_fixture_are_pinned() {
    let root = tree();
    let got = measure(&root);
    let _ = fs::remove_dir_all(&root);
    if std::env::var_os("UPDATE_PINS").is_some() {
        fs::write(PINS, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }
    let want: serde_json::Value = serde_json::from_str(&fs::read_to_string(PINS).unwrap()).unwrap();
    assert_eq!(got["feature_names"], want["feature_names"], "the feature list changed: record the pins again");
    let names: Vec<&str> = want["feature_names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    for (g, w) in got["queries"].as_array().unwrap().iter().zip(want["queries"].as_array().unwrap()) {
        let q = &w["query"];
        let (gc, wc) = (g["candidates"].as_array().unwrap(), w["candidates"].as_array().unwrap());
        let at = |cs: &[serde_json::Value]| cs.iter().map(|c| c["at"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(at(gc), at(wc), "{q}: the candidates or their order changed");
        for (a, b) in gc.iter().zip(wc) {
            for (i, name) in names.iter().enumerate() {
                assert_eq!(a["x"][i], b["x"][i], "{q}: {} feature {name}", b["at"]);
            }
            assert_eq!(a["score"], b["score"], "{q}: {} score", b["at"]);
        }
        assert_eq!(g["hits"], w["hits"], "{q}: the hits changed");
    }
    assert_eq!(got["queries"].as_array().unwrap().len(), want["queries"].as_array().unwrap().len());
}
