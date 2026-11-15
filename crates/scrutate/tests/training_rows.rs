//! What the training scripts read from the binary: `batch --features` (the ranker's rows) and the
//! hidden `views` command (the vector model's chunks, view tokens and query terms).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("scrutate_training_{}_{}", name, std::process::id()));
    tag::claim(&root);
    let _ = fs::remove_dir_all(&root);
    // a repository: no index in a directory above takes it over
    fs::create_dir_all(root.join(".git")).unwrap();
    for (rel, text) in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    root
}

fn run(root: &PathBuf, args: &[&str]) -> Vec<serde_json::Value> {
    let out = Command::new(BIN).arg("--root").arg(root).args(args).stdin(Stdio::null()).env("SCRUTATE_SEMANTIC", "none").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

const SHOP: &str = "class Order:\n    def total(self):\n        \"\"\"Sum of the lines.\"\"\"\n        return 1\n\n\ndef total_price():\n    return 2\n";

#[test]
fn batch_features_name_every_feature_once_per_query() {
    let root = tree("features", &[("shop.py", SHOP)]);
    let q = root.join("q.jsonl");
    fs::write(&q, "{\"query\": \"total\", \"k\": 3}\n").unwrap();
    let rec = &run(&root, &["batch", "--features", q.to_str().unwrap()])[0];
    let names: Vec<&str> = rec["feature_names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(names, scrutate::search::FEATURES);
    let cands = rec["candidates"].as_array().unwrap();
    let total = cands.iter().find(|c| c["qualname"] == "Order.total").expect("Order.total is a candidate");
    assert_eq!((total["name"].as_str(), total["kind"].as_str()), (Some("total"), Some("method")));
    let x = total["x"].as_array().unwrap();
    assert_eq!(x.len(), names.len());
    let at = |n: &str| x[names.iter().position(|f| *f == n).unwrap()].as_f64().unwrap();
    assert_eq!((at("exact_name"), at("k_method"), at("has_doc")), (1.0, 1.0, 1.0));
    for key in ["exact_ids", "lex_ids", "sem_ids"] {
        assert!(rec[key].is_array(), "{key}: {rec}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn views_print_the_chunks_of_a_file_and_the_terms_of_a_query() {
    let root = tree("views", &[("src/shop.py", SHOP)]);
    let q = root.join("q.jsonl");
    let file = root.join("src/shop.py");
    let req = serde_json::json!({"file": file, "path": "shop.py"});
    fs::write(&q, format!("{req}\n{{\"query\": \"sum the order lines\"}}\n{{\"file\": \"missing.py\", \"path\": \"missing.py\"}}\n")).unwrap();
    let recs = run(&root, &["views", q.to_str().unwrap()]);
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0]["path"], "shop.py");
    let total = recs[0]["chunks"].as_array().unwrap().iter().find(|c| c["qualname"] == "Order.total").expect("Order.total");
    assert_eq!(total["kind"], "method");
    assert_eq!(total["doc"], "Sum of the lines.");
    assert_eq!(total["views"]["name"], serde_json::json!(scrutate::text::tokenize2("total", None).0));
    assert_eq!(total["views"]["qual"], serde_json::json!(scrutate::text::tokenize2("Order", None).0));
    assert_eq!(recs[1]["terms"], serde_json::json!(scrutate::text::query_terms("sum the order lines")));
    assert!(recs[2]["error"].is_string() && recs[2].get("chunks").is_none(), "{}", recs[2]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn views_read_given_text_and_print_a_files_language_test_flag_subwords_and_stop_list() {
    let root = tree("views_text", &[("keep.txt", "")]);
    let q = root.join("q.jsonl");
    let reqs = [
        serde_json::json!({"text": "/// Adds two numbers.\npub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n", "path": "src/math_test.rs"}),
        serde_json::json!({"text": "export function total() { return 1; }\n", "path": "lib/shop.ts"}),
        serde_json::json!({"split": "parseHTTPResponse_v2"}),
        serde_json::json!({"stopwords": true}),
    ];
    fs::write(&q, reqs.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    let recs = run(&root, &["views", q.to_str().unwrap()]);
    assert_eq!((recs[0]["lang"].as_str(), recs[0]["test"].as_bool()), (Some("rust"), Some(true)));
    let add = recs[0]["chunks"].as_array().unwrap().iter().find(|c| c["name"] == "add").expect("add");
    assert_eq!((add["kind"].as_str(), add["start"].as_u64(), add["doc"].as_str()), (Some("function"), Some(2), Some("Adds two numbers.")));
    assert_eq!((recs[1]["lang"].as_str(), recs[1]["test"].as_bool()), (Some("typescript"), Some(false)));
    assert_eq!(recs[2]["subwords"], serde_json::json!(scrutate::text::raw_subwords("parseHTTPResponse_v2")));
    let stop: Vec<&str> = recs[3]["stopwords"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(stop.windows(2).all(|w| w[0] < w[1]), "sorted, each once");
    assert_eq!(stop.len(), scrutate::text::QUERY_STOP.len());
    assert!(stop.iter().all(|w| scrutate::text::QUERY_STOP.contains(w)));
    let _ = fs::remove_dir_all(&root);
}
