//! Secrets in answer text: each one the detectors find is replaced with `[redacted:KIND]`, its key
//! name, scheme or PEM armour left in place. The detectors are ports of gitleaks v8.30.1 and
//! secretlint v13.1.0 rules (docs/SECURITY.md lists them); a rule's regex runs only on text that
//! holds one of its keywords. Encoded secrets (base64, hex, URL-encoded) are not looked for.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// One redaction: its kind and the byte span of its marker in the redacted text.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
}

/// Whether answers are redacted: `SCRUTATE_REDACT` (`off` or `on`) decides, else `default`, which
/// is on for what an agent reads (MCP, the hooks, the language server) and off for the CLI.
pub fn on(default: bool) -> bool {
    match std::env::var("SCRUTATE_REDACT").ok().as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("off" | "0" | "false" | "no") => false,
        Some("on" | "1" | "true" | "yes") => true,
        _ => default,
    }
}

struct Rule {
    kind: &'static str,
    re: Regex,
    /// the capture group that is the secret
    group: usize,
    /// dropped when the secret's Shannon entropy is at or below this, as gitleaks drops it
    entropy: f64,
    /// one of these, in any case, must be in the text for the regex to run (none: it always runs)
    keywords: &'static [&'static str],
    /// an extra test of (secret, whole match): false drops it
    keep: fn(&str, &str) -> bool,
}

fn any(_: &str, _: &str) -> bool {
    true
}

const CODE_END: &str = r#"(?:[`'"\s;]|\\[nr]|$)"#;

fn rule(kind: &'static str, re: &str, group: usize, entropy: f64, keywords: &'static [&'static str], keep: fn(&str, &str) -> bool) -> Rule {
    Rule { kind, re: Regex::new(re).unwrap_or_else(|e| panic!("{kind}: {e}")), group, entropy, keywords, keep }
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule("aws-access-key-id", r"\b((?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b", 1, 0.0, &["akia", "asia", "abia", "acca", "a3t"], any),
        rule(
            "aws-secret-access-key",
            r#"["']?(?:AWS|aws|Aws)?_?(?:SECRET|secret|Secret)_?(?:ACCESS|access|Access)_?(?:KEY|key|Key)["']?\s*(?::|=>|=)\s*["']?([A-Za-z0-9/+=]{40})(?:[^A-Za-z0-9/+=]|$)"#,
            1,
            0.0,
            &["secret_access_key", "secretaccesskey", "secret-access-key"],
            any,
        ),
        rule("github-token", r"\b((?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{36})\b", 1, 3.0, &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"], any),
        rule("github-fine-grained-token", r"\b(github_pat_[0-9A-Za-z_]{82})\b", 1, 3.0, &["github_pat_"], any),
        rule("gitlab-token", r"\b(glpat-[0-9A-Za-z_-]{20,300}(?:\.[0-9a-z]{9})?)(?:[^0-9A-Za-z_.-]|$)", 1, 3.0, &["glpat-"], any),
        rule(
            "slack-token",
            r"\b(xoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*|xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}|xoxb-[0-9]{8,14}-[a-zA-Z0-9]{18,26}|(?i:xapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+)|(?i:xoxe\.xox[bp]-\d-[A-Z0-9]{163,166})|(?i:xoxe-\d-[A-Z0-9]{146})|xox[os]-\d+-\d+-\d+-[a-fA-F\d]+|xox[ar]-(?:\d-)?[0-9a-zA-Z]{8,48})",
            1,
            2.0,
            &["xoxb-", "xoxp-", "xoxe", "xapp-", "xoxa-", "xoxr-", "xoxo-", "xoxs-"],
            any,
        ),
        rule("slack-webhook", r"(?:https?://)?hooks\.slack\.com/((?:services|workflows|triggers)/[A-Za-z0-9+/]{43,56})", 1, 0.0, &["hooks.slack.com"], any),
        rule(
            "stripe-key",
            r"\b((?:sk|rk)_(?:live|test|prod)_[0-9A-Za-z]{24,99})\b",
            1,
            2.0,
            &["sk_live_", "rk_live_", "sk_test_", "rk_test_", "sk_prod_", "rk_prod_"],
            any,
        ),
        rule("google-api-key", &format!(r"\b(AIza[0-9A-Za-z_-]{{35}}){CODE_END}"), 1, 4.0, &["aiza"], any),
        rule(
            "openai-api-key",
            &format!(
                r"\b(sk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{{74}}|[A-Za-z0-9_-]{{58}})T3BlbkFJ(?:[A-Za-z0-9_-]{{74}}|[A-Za-z0-9_-]{{58}})\b|sk-[a-zA-Z0-9]{{20}}T3BlbkFJ[a-zA-Z0-9]{{20}}){CODE_END}"
            ),
            1,
            3.0,
            &["t3blbkfj"],
            any,
        ),
        rule("anthropic-api-key", r"\b(sk-ant-(?:api|admin)0[0-9]-[A-Za-z0-9_-]{90,128}AA)(?:[^A-Za-z0-9_-]|$)", 1, 0.0, &["sk-ant-"], any),
        rule("npm-token", r"\b(npm_[A-Za-z0-9]{36})\b", 1, 0.0, &["npm_"], any),
        rule("pypi-token", r"\b(pypi-[A-Za-z0-9_-]{85,1000})", 1, 3.0, &["pypi-"], any),
        rule("jwt", &format!(r"\b(ey[a-zA-Z0-9]{{17,}}\.ey[a-zA-Z0-9/\\_-]{{17,}}\.(?:[a-zA-Z0-9/\\_-]{{10,}}={{0,2}})?){CODE_END}"), 1, 3.0, &["ey"], any),
        rule("twilio-api-key", r"\b(SK[0-9a-fA-F]{32})\b", 1, 3.0, &["sk"], any),
        rule("sendgrid-api-key", r"\b(SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43})\b", 1, 2.0, &["sg."], any),
        rule(
            "mailgun-api-key",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:mailgun)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:=|>|:{{1,3}}=|\|\||:|=>|\?=|,)[`'"\s=]{{0,5}}(key-[a-f0-9]{{32}}){CODE_END}"#
            ),
            1,
            0.0,
            &["mailgun"],
            any,
        ),
        rule(
            "database-password",
            r"(?i)\b((?:postgres(?:ql)?|mysqlx?|mariadb|mongodb(?:\+srv)?|rediss?|amqps?|mssql|sqlserver)://)([^:/\s@]{0,100}):([^@/\s]{1,200})@([^/\s?#]{1,100})",
            3,
            0.0,
            &["postgres", "mysql", "mariadb", "mongodb", "redis", "amqp", "mssql", "sqlserver"],
            real_value,
        ),
        // a `.env` line (`NAME=value`, the name upper case): the value may hold any symbol
        rule(
            "env-secret",
            r#"\b((?:export[ \t]+)?[A-Z0-9_]*(?:SECRET|PASSW(?:OR)?D|PWD|TOKEN|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY|AUTH_?KEY|CREDENTIALS?)[A-Z0-9_]*)=["']?([^\s"'`#]{8,200})"#,
            2,
            3.0,
            &["secret", "passw", "pwd", "token", "key", "credential"],
            env_value,
        ),
        // a line of a key's base64 body read without its armour (a range that starts inside the
        // key, a search hit): base64 alone on its line, as wide as PEM writes it, near random
        rule("key-material", r#"(?m)(?:^|[\s:"'])([A-Za-z0-9+/]{40,76}={0,2})["',]?[ \t]*$"#, 1, 4.5, &[], any),
        rule(
            "generic-secret",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:access|auth|(?-i:[Aa]pi|API)|credential|creds|key|passw(?:or)?d|secret|token)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:=|>|:{{1,3}}=|\|\||:|=>|\?=|,)[`'"\s=]{{0,5}}([\w.=-]{{10,150}}|[a-z0-9][a-z0-9+/]{{11,}}={{0,3}}){CODE_END}"#
            ),
            1,
            3.5,
            &["access", "api", "auth", "key", "credential", "creds", "passwd", "password", "secret", "token"],
            generic,
        ),
    ]
});

/// Names whose keyword is part of a longer word or names something that is no secret (gitleaks'
/// generic-api-key allowlist, tested on the whole match).
static NOT_SECRET_NAMES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)accessibility|accessor|access_id|random_access|api_id|api_name|api_version|rapid|capital|author|credentials_id|withcredentials|bucket_key|foreign_key|primary_key|public_key|schema_key|sequence_key|turkey|monkey|key_alias|key_id|key_length|key_name|key_pair|key_size|keypress|keystore_file|keytab_path|keying|secret_length|secret_name|secret_size|usersecretsid|csrf_token|public_token|token_file|key_file|api_endpoint|api_url|token_uri|io\.jsonwebtoken|authentication-results|x-ms-exchange-organization-auth").unwrap()
});

/// A value that is no placeholder: not `true`, `false` or `null`, not one character repeated, not
/// a variable (`$VAR`, `${VAR}`, `{{ x }}`, `%VAR%`, `<value>`), not an absolute path.
fn real_value(secret: &str, _: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    let first = secret.chars().next().unwrap_or(' ');
    !(matches!(lower.as_str(), "true" | "false" | "null" | "none" | "password" | "pass" | "secret")
        || secret.chars().all(|c| c == first)
        || secret.starts_with(['$', '{', '%', '<', '/'])
        || lower.starts_with("os."))
}

/// gitleaks' generic-api-key filters but its stopwords: a value with a digit or a symbol, a name
/// not on the allowlist, no placeholder, and no number written in code (`0xcbf29ce484222325u64`).
fn generic(secret: &str, whole: &str) -> bool {
    secret.chars().any(|c| !(c.is_ascii_alphabetic() || matches!(c, '_' | '.' | '-')))
        && !NOT_SECRET_NAMES.is_match(whole)
        && real_value(secret, whole)
        && !NUMBER.is_match(secret)
        && !IDENTIFIER.is_match(secret)
}

/// A name in one case joined by `_`, `-` or `.` (`NL80211_AUTHTYPE_SAE`, `keypad_row0`,
/// `x2apic_state`): code and device trees name things so; a random secret mixes cases.
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[A-Z][A-Z0-9]*(?:[_.-][A-Z0-9]+)+|[a-z][a-z0-9]*(?:[_.-][a-z0-9]+)+)$").unwrap());

/// A number as code writes it: hex, decimal or float, with a type suffix.
static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:0x[0-9a-f_]+|[0-9][0-9_]*(?:\.[0-9_]+)?(?:e[+-]?[0-9]+)?)(?:[ui](?:8|16|32|64|128|size)|f32|f64|[lu]{1,3})?$").unwrap()
});

/// A `.env` value that is no placeholder and is not a name or a word alone.
fn env_value(secret: &str, whole: &str) -> bool {
    real_value(secret, whole) && !NOT_SECRET_NAMES.is_match(whole) && secret.chars().any(|c| !c.is_ascii_alphabetic())
}

/// Shannon entropy in bits per character, over every character, as gitleaks computes it.
fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&k| -(k as f64 / n) * (k as f64 / n).log2()).sum()
}

static PREFILTER: LazyLock<aho_corasick::AhoCorasick> = LazyLock::new(|| {
    let words = RULES.iter().flat_map(|r| r.keywords.iter().copied());
    aho_corasick::AhoCorasick::builder().ascii_case_insensitive(true).build(words).unwrap()
});

static PEM_BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----").unwrap());
static PEM_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----").unwrap());
static PEM_BODY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=]{16,}").unwrap());

/// Redacts texts in order, a private key's body carried from one text into the next (the lines of
/// a file as an answer's items).
#[derive(Default)]
pub struct Redactor {
    in_key: bool,
    /// redactions made, by kind
    pub counts: std::collections::BTreeMap<&'static str, usize>,
}

impl Redactor {
    /// `text` with its secrets replaced, and where each marker is.
    pub fn redact<'a>(&mut self, text: &'a str) -> (Cow<'a, str>, Vec<Mark>) {
        let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
        self.private_keys(text, &mut spans);
        let mut wanted: Vec<bool> = RULES.iter().map(|r| r.keywords.is_empty()).collect();
        let starts: Vec<usize> = RULES.iter().scan(0, |n, r| Some(std::mem::replace(n, *n + r.keywords.len()))).collect();
        for m in PREFILTER.find_overlapping_iter(text) {
            wanted[starts.partition_point(|&s| s <= m.pattern().as_usize()) - 1] = true;
        }
        for (r, _) in RULES.iter().zip(&wanted).filter(|(_, &w)| w) {
            for c in r.re.captures_iter(text) {
                let Some(g) = c.get(r.group) else { continue };
                if (r.entropy > 0.0 && entropy(g.as_str()) <= r.entropy) || !(r.keep)(g.as_str(), &c[0]) {
                    continue;
                }
                spans.push((g.start(), g.end(), r.kind));
            }
        }
        if spans.is_empty() {
            return (Cow::Borrowed(text), Vec::new());
        }
        // earlier first, the longer of two at one place; one inside another goes with it
        spans.sort_by_key(|&(a, b, _)| (a, std::cmp::Reverse(b)));
        let (mut out, mut marks, mut at) = (String::with_capacity(text.len()), Vec::new(), 0);
        for (a, b, kind) in spans {
            if a < at {
                continue;
            }
            out.push_str(&text[at..a]);
            let start = out.len();
            out.push_str("[redacted:");
            out.push_str(kind);
            out.push(']');
            marks.push(Mark { kind, start, end: out.len() });
            *self.counts.entry(kind).or_default() += 1;
            at = b;
        }
        out.push_str(&text[at..]);
        (Cow::Owned(out), marks)
    }

    /// The base64 runs of private key bodies, line by line: from a BEGIN armour line to its END
    /// line, which stay.
    fn private_keys(&mut self, text: &str, spans: &mut Vec<(usize, usize, &'static str)>) {
        if !self.in_key && !text.contains("PRIVATE KEY") && !text.contains("private key") {
            return;
        }
        let mut line_at = 0;
        for line in text.split_inclusive('\n') {
            let (mut from, mut to) = (0, line.len());
            if let Some(m) = PEM_BEGIN.find(line).filter(|_| !self.in_key) {
                self.in_key = true;
                from = m.end();
            }
            if self.in_key {
                if let Some(m) = PEM_END.find_at(line, from) {
                    to = m.start();
                    self.in_key = false;
                }
                spans.extend(PEM_BODY.find_iter(&line[from..to]).map(|m| (line_at + from + m.start(), line_at + from + m.end(), "private-key")));
                // a key in one JSON string (`\n` written out) may open again after its END
                if !self.in_key && PEM_BEGIN.is_match(&line[to..]) {
                    let rest = to;
                    let mut sub = Vec::new();
                    self.private_keys(&line[rest..], &mut sub);
                    spans.extend(sub.into_iter().map(|(a, b, k)| (line_at + rest + a, line_at + rest + b, k)));
                }
            }
            line_at += line.len();
        }
    }

    /// Reads `text` for where a private key's body starts or ends and redacts nothing: a text
    /// before what an answer shows, which a key in it may run on from.
    pub fn track(&mut self, text: &str) {
        self.private_keys(text, &mut Vec::new());
    }

    /// The texts an answer can show redacted in place: from `first`, until `room` bytes of them
    /// (after redaction) are done; the ones before `first` only tracked. Texts past that are
    /// left as they are, as no page of `room` bytes reaches them.
    pub fn redact_shown(&mut self, texts: &mut [String], first: usize, room: usize) {
        let first = first.min(texts.len());
        texts[..first].iter().for_each(|t| self.track(t));
        let mut used = 0;
        for t in &mut texts[first..] {
            if used > room {
                break;
            }
            let redacted = match self.redact(t).0 {
                Cow::Owned(s) => s,
                Cow::Borrowed(_) => {
                    used += t.len() + 1;
                    continue;
                }
            };
            *t = redacted;
            used += t.len() + 1;
        }
    }

    /// Every string in `v` redacted in place.
    pub fn redact_json(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if let (Cow::Owned(r), _) = self.redact(s) {
                    *s = r;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.redact_json(x)),
            Value::Object(o) => o.values_mut().for_each(|x| self.redact_json(x)),
            _ => {}
        }
    }

    /// How many redactions were made.
    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }

    /// The closing line of an answer that had redactions, if any.
    pub fn note(&self) -> Option<String> {
        note(self.total())
    }
}

/// The closing line of an answer that shows `n` redactions, if any.
pub fn note(n: usize) -> Option<String> {
    (n > 0).then(|| format!("({n} secret{} redacted: SCRUTATE_REDACT=off shows them)", if n == 1 { "" } else { "s" }))
}

/// `text` with its secrets replaced, and where each marker is.
pub fn redact(text: &str) -> (Cow<'_, str>, Vec<Mark>) {
    Redactor::default().redact(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` characters of `set`, the same for a seed every run: samples are built here, so no file
    /// of the repository holds a secret-shaped literal for a scanner to stop.
    pub(crate) fn fake(seed: u64, set: &str, n: usize) -> String {
        let set: Vec<char> = set.chars().collect();
        let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                set[(x >> 33) as usize % set.len()]
            })
            .collect()
    }

    pub(crate) const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const HEX: &str = "0123456789abcdef";

    /// A secret of each kind, as it might sit in code or a `.env` file: (kind, line, secret).
    pub(crate) fn samples() -> Vec<(&'static str, String, String)> {
        let line = |kind: &'static str, before: &str, secret: String, after: &str| (kind, format!("{before}{secret}{after}"), secret);
        vec![
            line("aws-access-key-id", "aws_key = \"", format!("{}{}", "AKIA", fake(1, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16)), "\""),
            line("aws-secret-access-key", "aws_secret_access_key = ", fake(2, &format!("{ALNUM}/+"), 40), ""),
            line("github-token", "token: ", format!("{}{}", "ghp_", fake(3, ALNUM, 36)), ""),
            line("github-fine-grained-token", "GH=", format!("{}{}", "github_pat_", fake(4, ALNUM, 82)), ""),
            line("gitlab-token", "CI_TOKEN ", format!("{}{}", "glpat-", fake(5, ALNUM, 20)), " "),
            line("slack-token", "slack = '", format!("{}{}-{}-{}", "xoxb-", fake(6, "0123456789", 13), fake(7, "0123456789", 13), fake(8, ALNUM, 24)), "'"),
            line(
                "slack-webhook",
                "url = https://hooks.slack.com/services/",
                format!("T0{}/B0{}/{}", fake(9, ALNUM, 7), fake(10, ALNUM, 7), fake(11, ALNUM, 24)),
                "",
            ),
            line("stripe-key", "Stripe.api_key = \"", format!("{}{}", "sk_live_", fake(12, ALNUM, 26)), "\""),
            line("google-api-key", "maps = \"", format!("{}{}", "AIza", fake(13, ALNUM, 35)), "\""),
            line("openai-api-key", "OPENAI = \"", format!("{}{}{}{}", "sk-", fake(14, ALNUM, 20), "T3BlbkFJ", fake(15, ALNUM, 20)), "\""),
            line("anthropic-api-key", "key = ", format!("{}{}{}{}", "sk-ant-", "api03-", fake(16, ALNUM, 93), "AA"), ""),
            line("npm-token", "//registry.npmjs.org/:_authToken=", format!("{}{}", "npm_", fake(17, ALNUM, 36)), ""),
            line("pypi-token", "password = ", format!("{}{}", "pypi-", fake(18, ALNUM, 100)), ""),
            line("jwt", "Authorization: Bearer ", format!("{}{}.{}{}.{}", "eyJ", fake(19, ALNUM, 20), "eyJ", fake(20, ALNUM, 20), fake(21, ALNUM, 24)), " "),
            line("twilio-api-key", "TWILIO_SID ", format!("{}{}", "SK", fake(22, HEX, 32)), ""),
            line("sendgrid-api-key", "sg = ", format!("{}{}.{}", "SG.", fake(23, ALNUM, 22), fake(24, ALNUM, 43)), ""),
            line("mailgun-api-key", &format!("{}_API_KEY=", "MAILGUN"), format!("{}{}", "key-", fake(25, HEX, 32)), ""),
            line("database-password", "DATABASE_URL=postgres://app:", fake(26, ALNUM, 12), "@db.internal:5432/app"),
            line("env-secret", "DB_PASSWORD=", format!("{}!{}", fake(27, ALNUM, 8), fake(28, ALNUM, 8)), ""),
            line("generic-secret", "client_secret = \"", fake(29, ALNUM, 18), "\""),
        ]
    }

    #[test]
    fn each_kind_is_redacted_and_its_name_stays() {
        for (kind, line, secret) in samples() {
            let (out, marks) = redact(&line);
            assert!(!out.contains(&secret), "{kind}: {out}");
            assert!(marks.iter().any(|m| m.kind == kind), "{kind}: {marks:?} {out}");
            assert!(out.contains(&format!("[redacted:{kind}]")), "{kind}: {out}");
            let name_len = line.find(&secret).unwrap().min(6);
            assert!(out.starts_with(&line[..name_len]), "{kind}: the name went: {out}");
            for m in &marks {
                assert_eq!(&out[m.start..m.end], format!("[redacted:{}]", m.kind));
            }
        }
    }

    /// A PEM armour line, written out here so no file holds one.
    pub(crate) fn armour(edge: &str, what: &str) -> String {
        format!("-----{edge} {what}PRIVATE KEY-----")
    }

    #[test]
    fn a_private_key_body_goes_line_by_line_across_texts_and_its_armour_stays() {
        let mut r = Redactor::default();
        let b64 = format!("{ALNUM}+/");
        let lines = [
            format!("  1: {}", armour("BEGIN", "RSA ")),
            format!("  2: MIIE{}", fake(30, &b64, 60)),
            format!("  3: {}==", fake(31, &b64, 60)),
            format!("  4: {}", armour("END", "RSA ")),
            "  5: x = 1".to_string(),
        ];
        let out: Vec<String> = lines.iter().map(|l| r.redact(l).0.into_owned()).collect();
        assert_eq!(out[0], lines[0]);
        assert_eq!(out[1], "  2: [redacted:private-key]");
        assert_eq!(out[2], "  3: [redacted:private-key]");
        assert_eq!(out[3], lines[3]);
        assert_eq!(out[4], lines[4]);
        // in one JSON string, `\n` written out
        let body = format!("MIIEv{}", fake(32, ALNUM, 28));
        let json = format!(r#""key": "{}\n{body}\n{}\n""#, armour("BEGIN", ""), armour("END", ""));
        let (out, _) = redact(&json);
        assert!(!out.contains(&body) && out.contains(&armour("END", "")), "{out}");
    }

    /// Code that names keys, tokens and secrets without holding one is left alone.
    #[test]
    fn code_about_secrets_is_not_redacted() {
        for line in [
            "let token = get_token(user_id);",
            "api_key: Option<String>,",
            "password = os.environ[\"DB_PASSWORD\"]",
            "secret_name = \"prod-db-password\"",
            "public_key = load_public_key(path)",
            "DATABASE_URL=postgres://user:${DB_PASSWORD}@localhost/app",
            "auth_token = \"${{ secrets.TOKEN }}\"",
            "key = \"aaaaaaaaaaaaaaaaaaaa\"",
            "static int key_len = 32;",
            "if (key == KEY_ENTER) return;",
            "token_type: \"Bearer\",",
            "struct crypto_skcipher *tfm = crypto_alloc_skcipher(\"cbc(aes)\", 0, 0);",
            "access_mode = O_RDONLY | O_CLOEXEC;",
            "let mut key = 0xcbf29ce484222325u64;",
            "params->auth_type = NL80211_AUTHTYPE_SAE;",
            "keypad_row0: keypad_row0_0 {",
            "x2apic_state = X2APIC_ON_LOCKED;",
        ] {
            let (out, marks) = redact(line);
            assert!(marks.is_empty(), "{line} -> {out}");
        }
    }

    #[test]
    fn strings_in_json_are_redacted_in_place_and_counted() {
        let mut v = serde_json::json!({"result": [{"text": format!("x = '{}{}'", "ghp_", fake(33, ALNUM, 36)), "line": 3}]});
        let mut r = Redactor::default();
        r.redact_json(&mut v);
        assert_eq!(v["result"][0]["text"], "x = '[redacted:github-token]'");
        assert_eq!(r.total(), 1);
        assert!(r.note().unwrap().starts_with("(1 secret redacted"));
    }

    /// False redactions on a real tree: every line of every text file under `SCRUTATE_FP_ROOT`
    /// through `redact`, each mark printed with its line, and the rate per 10,000 lines.
    #[test]
    #[ignore = "needs SCRUTATE_FP_ROOT"]
    fn false_redactions_per_ten_thousand_lines() {
        let Some(root) = std::env::var_os("SCRUTATE_FP_ROOT") else { return };
        let (mut lines, mut marks) = (0u64, 0u64);
        let walk = ignore::WalkBuilder::new(&root).hidden(false).filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".scrutate").build();
        for e in walk.flatten().filter(|e| e.file_type().is_some_and(|t| t.is_file())) {
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let mut r = Redactor::default();
            for (i, line) in text.lines().enumerate() {
                lines += 1;
                let (out, m) = r.redact(line);
                if !m.is_empty() {
                    marks += m.len() as u64;
                    println!("{}:{}: {}", e.path().display(), i + 1, out.chars().take(200).collect::<String>());
                }
            }
        }
        println!("{marks} marks in {lines} lines: {:.3} per 10,000", marks as f64 * 10_000.0 / lines.max(1) as f64);
    }

    /// What redaction costs on the largest answers: a 24,000-byte page and a 1 MB file, line by
    /// line, as `text_page` redacts them (release build: `cargo test --release`).
    #[test]
    #[ignore = "a timing"]
    fn the_cost_of_redacting_large_answers() {
        let src: String = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect::<Vec<_>>()
            .concat();
        for (what, text) in [("24,000-byte page", &src[..src.floor_char_boundary(24_000)]), ("1 MB file", &src[..src.floor_char_boundary(1 << 20)])] {
            let t0 = std::time::Instant::now();
            let runs = 20;
            for _ in 0..runs {
                let mut r = Redactor::default();
                for line in text.lines() {
                    std::hint::black_box(r.redact(line));
                }
            }
            println!("{what}: {:.3} ms per answer", t0.elapsed().as_secs_f64() * 1000.0 / runs as f64);
        }
    }

    #[test]
    fn the_switch_reads_the_environment_over_the_default() {
        // only this test reads SCRUTATE_REDACT in this binary
        assert!(on(true) || std::env::var("SCRUTATE_REDACT").is_ok());
        assert!(!on(false) || std::env::var("SCRUTATE_REDACT").is_ok());
    }
}
