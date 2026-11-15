//! Text that is not Latin script: search by words of every script, names compared in NFC, and
//! definitions with very long names.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

use scrutate::text::{name_key, query_terms, tokenize2};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: &[(&str, String)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_unicode_{}_{}", name, std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (rel, text) in files {
            fs::write(root.join(rel), text).unwrap();
        }
        let r = Repo { root };
        let (code, _, err) = r.run(&["index", "--no-text"]);
        assert_eq!(code, 0, "{err}");
        r
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN).args(args).current_dir(&self.root).stdin(Stdio::null()).output().expect("run scrutate");
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
    fn top(&self, cmd: &str, q: &str) -> String {
        let (code, out, err) = self.run(&[cmd, q]);
        // 1 is "no results"
        assert!(code == 0 || code == 1, "{cmd} {q}: {err}");
        out
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const SCRIPTS: &str = r#"def calc_total(items):
    """计算订单总价，包括税费和运费。"""
    return 1


def 関数():
    """注文の合計金額を計算する"""
    return 2


def выручка():
    """Вычисляет общую выручку за период"""
    return 3


def korean_total():
    """주문 총액을 계산합니다"""
    return 4


def arabic_total():
    """يحسب إجمالي الطلب"""
    return 5


def parse_config(path):
    """Read the configuration file."""
    return path
"#;

#[test]
fn queries_in_every_script_find_the_function() {
    let r = Repo::new("scripts", &[("m.py", SCRIPTS.to_string())]);
    let cases = [
        ("计算订单总价", "calc_total"),
        ("订单", "calc_total"),
        ("関数", "関数"),
        ("注文の合計金額", "関数"),
        ("合計", "関数"),
        ("выручка", "выручка"),
        ("общую выручку", "выручка"),
        ("Вычисляет", "выручка"),
        ("주문 총액", "korean_total"),
        ("계산합니다", "korean_total"),
        ("إجمالي الطلب", "arabic_total"),
    ];
    let mut missed = Vec::new();
    for (q, want) in cases {
        let out = r.top("search", q);
        let first = out.lines().next().unwrap_or("");
        if !first.ends_with(&format!("] {want}")) {
            missed.push(format!("{q} -> {first:?}"));
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");
    assert!(r.top("search", "read configuration file").lines().next().unwrap().ends_with("] parse_config"));
}

#[test]
fn ascii_tokens_are_unchanged() {
    let (s, w) = tokenize2("def parseHTTPResponse_v2(self): 2fa refreshing tokens", None);
    assert_eq!(s, vec!["pars", "http", "respons", "v", "fa", "refresh", "token"].into_iter().filter(|t| *t != "v").collect::<Vec<_>>());
    assert_eq!(w, vec!["parsehttpresponsev"]);
    assert_eq!(query_terms("where is the oauth token refreshed?"), vec!["oauth", "token", "refresh"]);
}

#[test]
fn unspaced_scripts_are_bigrams_and_spaced_scripts_are_words() {
    assert_eq!(tokenize2("订单总价", None).0, vec!["订单", "单总", "总价"]);
    assert_eq!(tokenize2("税", None).0, vec!["税"]);
    assert_eq!(tokenize2("Вычисляет ОБЩУЮ", None).0, vec!["вычисляет", "общую"]);
    // an ASCII part of a mixed word is stemmed like any ASCII word; the rest is not
    assert_eq!(tokenize2("parsing_выручки", None).0, vec!["pars", "выручки"]);
    // the Japanese 関 folds with its traditional form 關 to the simplified 关
    assert_eq!(tokenize2("x関数y", None).0, vec!["关数"]);
    assert_eq!(query_terms("合計 合計"), vec!["合计"]);
    // Thai, Lao, Myanmar, Khmer, Javanese: no spaces between words either
    for run in ["ภาษาไทย", "ພາສາລາວ", "မြန်မာစာ", "ភាសាខ្មែរ", "ꦧꦱꦗꦮ"] {
        let chars: Vec<char> = run.chars().collect();
        let pairs: Vec<String> = chars.windows(2).map(|p| p.iter().collect()).collect();
        assert_eq!(tokenize2(run, None).0, pairs, "{run}");
    }
    // Tibetan marks its syllables: the tsheg between them ends a run
    assert_eq!(tokenize2("བོད་སྐད", None).0, vec!["བོ", "ོད", "སྐ", "ྐད"]);
}

const HAN_FOLD: &str = include_str!("../data/han_fold.txt");

#[test]
fn traditional_and_simplified_han_give_the_same_tokens() {
    // a regenerated table must update these numbers
    assert_eq!(HAN_FOLD.len(), 47496);
    assert!(HAN_FOLD.starts_with("# Unihan 18.0.0 kSimplifiedVariant"));
    assert!(HAN_FOLD.lines().next().unwrap().contains("OpenCC ver.1.4.2 JPShinjitaiCharacters.txt"));
    assert_eq!(HAN_FOLD.lines().nth(1).unwrap().chars().count(), 2 * 6993);
    assert_eq!(tokenize2("資料庫", None), tokenize2("资料库", None));
    assert_eq!(tokenize2("資料庫", None).0, vec!["资料", "料库"]);
    assert_eq!(tokenize2("庫", None).0, vec!["库"]);
    assert_eq!(tokenize2("資料库", None).0, vec!["资料", "料库"]);
    // several simplified variants: the first that is not the character itself
    assert_eq!(tokenize2("乾", None).0, vec!["干"]);
    assert_eq!(tokenize2("噁", None).0, vec!["恶"]);
    assert_eq!(tokenize2("データ庫の", None).0, vec!["デー", "ータ", "タ库", "库の"]);
    assert_eq!(tokenize2("ひらがな", None).0, vec!["ひら", "らが", "がな"]);
    assert_eq!(query_terms("讀取 资料库"), query_terms("读取 資料庫"));
    assert_eq!(name_key("資料庫"), "資料庫");
}

const HAN_FORMS: &str = r#"def load_settings(path):
    """連接資料庫並讀取使用者設定"""
    return path


def notify_admin(msg):
    """发送电子邮件通知给管理员"""
    return msg


def parse_config(path):
    """Read the configuration file."""
    return path
"#;

#[test]
fn a_query_in_either_han_form_finds_text_in_the_other() {
    let r = Repo::new("hanforms", &[("m.py", HAN_FORMS.to_string())]);
    let cases = [("资料库", "load_settings"), ("读取使用者设定", "load_settings"), ("電子郵件", "notify_admin"), ("發送通知給管理員", "notify_admin")];
    let mut missed = Vec::new();
    for (q, want) in cases {
        let out = r.top("search", q);
        let first = out.lines().next().unwrap_or("");
        if !first.ends_with(&format!("] {want}")) {
            missed.push(format!("{q} -> {first:?}"));
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");
}

#[test]
fn japanese_shinjitai_give_the_tokens_of_traditional_and_simplified_han() {
    // shinjitai, traditional, simplified
    for forms in [["関", "關", "关"], ["図", "圖", "图"], ["経", "經", "经"], ["読", "讀", "读"], ["総", "總", "总"], ["発", "發", "发"]] {
        let tokens: Vec<_> = forms.iter().map(|f| tokenize2(f, None).0).collect();
        assert!(tokens.iter().all(|t| *t == tokens[0]), "{forms:?} -> {tokens:?}");
    }
    // a traditional form with no simplified one, and an old form that meets the shinjitai's Chinese key
    assert_eq!(tokenize2("乗", None), tokenize2("乘", None));
    assert_eq!(tokenize2("緖", None).0, vec!["绪"]);
    // Unihan's own simplified form of 関 still meets it
    assert_eq!(tokenize2("\u{2CBA6}", None), tokenize2("関", None));
    assert_eq!(tokenize2("開発", None), tokenize2("开发", None));
    // other characters, and shinjitai that are other words in Chinese, keep their own tokens
    for (a, b) in [("関", "閑"), ("関", "開"), ("芸", "藝"), ("沪", "濾"), ("予", "豫"), ("欠", "缺")] {
        assert_ne!(tokenize2(a, None), tokenize2(b, None), "{a} {b}");
    }
}

const SHINJITAI: &str = r#"def relations_cn(user):
    """更新用户关系表"""
    return user


def relations_tw(user):
    """更新使用者關係表"""
    return user


def relations_jp(user):
    """ユーザーの関係を更新する"""
    return user


def parse_config(path):
    """Read the configuration file."""
    return path
"#;

#[test]
fn a_query_in_shinjitai_traditional_or_simplified_finds_the_other_two() {
    let r = Repo::new("shinjitai", &[("m.py", SHINJITAI.to_string())]);
    let mut missed = Vec::new();
    for q in ["関係", "關係", "关系"] {
        let out = r.top("search", q);
        for want in ["relations_cn", "relations_tw", "relations_jp"] {
            if !out.lines().any(|l| l.ends_with(&format!("] {want}"))) {
                missed.push(format!("{q} misses {want}"));
            }
        }
        assert!(!out.contains("parse_config"), "{q}: {out}");
    }
    assert!(missed.is_empty(), "{missed:#?}");
}

#[test]
fn names_compare_in_nfc() {
    let nfc = "caf\u{e9}";
    let nfd = "cafe\u{301}";
    assert_eq!(name_key(nfd), name_key(nfc));
    assert_eq!(name_key("ΛΌΓΟΣ"), name_key("λόγος"));
    assert_eq!(name_key("Send_Request"), "sendrequest");
    let r = Repo::new("nfc", &[("a.py", format!("def {nfc}():\n    return 1\n")), ("b.py", format!("def {nfd}_x():\n    return 2\n"))]);
    for q in [nfc, nfd] {
        assert!(r.top("symbol", q).contains("a.py:1"), "symbol {q:?}");
        assert!(r.top("symbol", &format!("{q}_x")).contains("b.py:1"), "symbol {q:?}_x");
    }
    assert!(r.top("search", nfd).lines().next().unwrap().ends_with(&format!("] {nfc}")));
}

#[test]
fn very_long_names_are_found_by_their_exact_name() {
    let names: Vec<String> = [200usize, 255, 256, 300, 600].iter().map(|n| format!("f{}", "a".repeat(n - 1))).collect();
    let src: String = names.iter().map(|n| format!("def {n}():\n    return 1\n\n\n")).collect();
    let r = Repo::new("long", &[("m.py", src)]);
    for n in &names {
        assert!(r.top("symbol", n).contains(&format!("] {n}")), "symbol of {} chars", n.len());
        assert!(r.top("search", n).lines().next().unwrap_or("").ends_with(&format!("] {n}")), "search of {} chars", n.len());
    }
}
