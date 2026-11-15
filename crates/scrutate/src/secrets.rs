//! Secrets in answer text: each one the detectors find is replaced with `[redacted:KIND]`, its key
//! name, scheme or PEM armour left in place. Most detectors are ports of gitleaks v8.30.1 and
//! secretlint v13.1.0 rules, each naming its source; a few are scrutate's own. A rule's regex runs
//! only on text that holds one of its keywords. Encoded secrets (base64, hex, URL-encoded) are not
//! looked for.

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
/// is on for what an agent reads (MCP, the hooks, the language server) and off for the CLI. Any
/// other value keeps the default and says so on stderr, once.
pub fn on(default: bool) -> bool {
    match std::env::var("SCRUTATE_REDACT").ok().as_deref() {
        None | Some("") => default,
        Some("off") => false,
        Some("on") => true,
        Some(other) => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| eprintln!("[scrutate] SCRUTATE_REDACT={other:?} is neither off nor on: redaction stays {}", if default { "on" } else { "off" }));
            default
        }
    }
}

struct Rule {
    kind: &'static str,
    /// where the rule comes from: the upstream rule, or `scrutate` for one of its own
    source: &'static str,
    re: Regex,
    /// the capture group that is the secret
    group: usize,
    /// dropped when the secret's Shannon entropy is at or below this, as gitleaks drops it
    entropy: f64,
    /// one of these, in any case, must be in the text for the regex to run (none: it always runs)
    keywords: &'static [&'static str],
    /// an extra test of the secret, the whole match and the text before the match: false drops it
    keep: fn(&str, &str, &str) -> bool,
}

fn any(_: &str, _: &str, _: &str) -> bool {
    true
}

const CODE_END: &str = r#"(?:[`'"\s;]|\\[nr]|$)"#;

/// A typed assignment's annotation and `=` (`: &str =`, `?: string | undefined =`, `!: string =`,
/// `: HashMap<String, Vec<u8>> =`): printable ASCII but `=`, `;`, `{`, `}`, `!` and quotes (a
/// lifetime's `'a` aside), and `,`, `(` and `)` only inside angle brackets, so no language's syntax
/// is listed and a run never passes a value or another parameter. `find` drops a match whose
/// annotation (group `ann`) is over `TYPED_MAX`: a bounded repetition here is slow to build.
/// Taken first where an assignment rule takes `=` or `:`.
const TYPED: &str = r#"[?!]?:(?P<ann>(?:[\t\x20\x23-\x26\x2a\x2b\x2d-\x3a\x3f-\x5f\x61-\x7a\x7c\x7e]|'[A-Za-z_]|<(?:[\t\x20\x23-\x26\x28-\x3a\x3f-\x5f\x61-\x7a\x7c\x7e]|<[\t\x20\x23-\x26\x28-\x3a\x3f-\x5f\x61-\x7a\x7c\x7e]*>)*>)+)="#;
const TYPED_MAX: usize = 80;

/// A value wrapped in up to two constructors (`Some("...")`, `Some(String::from("..."))`,
/// `Optional.of("...")`), taken before the value. A lower-case call is left out: its argument names a thing
/// (`rtas_token("ibm,get-system-parameter")`, `getenv("API_TOKEN")`).
const WRAPPED: &str = r#"(?:[ \t]*(?:Some|Ok|[A-Z][A-Za-z0-9_]*(?:::|\.)(?:from|new|of))\(){0,2}"#;

#[allow(clippy::too_many_arguments)]
fn rule(
    kind: &'static str,
    source: &'static str,
    re: &str,
    group: usize,
    entropy: f64,
    keywords: &'static [&'static str],
    keep: fn(&str, &str, &str) -> bool,
) -> Rule {
    Rule { kind, source, re: Regex::new(re).unwrap_or_else(|e| panic!("{kind}: {e}")), group, entropy, keywords, keep }
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            "aws-access-key-id",
            "gitleaks aws-access-token",
            r"\b((?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b",
            1,
            0.0,
            &["akia", "asia", "abia", "acca", "a3t"],
            any,
        ),
        rule(
            "aws-secret-access-key",
            "secretlint-rule-aws",
            &format!(
                r#"["']?(?:AWS|aws|Aws)?_?(?:SECRET|secret|Secret)_?(?:ACCESS|access|Access)_?(?:KEY|key|Key)["']?\s*(?:{TYPED}|:|=>|=)\s*{WRAPPED}(?:r#{{0,2}}"|["'])?([A-Za-z0-9/+=]{{40}})(?:[^A-Za-z0-9/+=]|$)"#
            ),
            2,
            0.0,
            &["secret_access_key", "secretaccesskey", "secret-access-key"],
            any,
        ),
        rule(
            "github-token",
            "gitleaks github-pat, github-oauth, github-app-token, github-refresh-token",
            r"\b((?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{36})\b",
            1,
            3.0,
            &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
            any,
        ),
        rule("github-fine-grained-token", "gitleaks github-fine-grained-pat", r"\b(github_pat_[0-9A-Za-z_]{82})\b", 1, 3.0, &["github_pat_"], any),
        rule(
            "gitlab-token",
            "gitleaks gitlab-pat, gitlab-pat-routable",
            r"\b(glpat-[0-9A-Za-z_-]{20,300}(?:\.[0-9a-z]{9})?)(?:[^0-9A-Za-z_.-]|$)",
            1,
            3.0,
            &["glpat-"],
            any,
        ),
        rule(
            "slack-token",
            "gitleaks slack-bot-token, slack-user-token, slack-app-token, slack-config-*, slack-legacy-*",
            r"\b(xoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*|xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}|xoxb-[0-9]{8,14}-[a-zA-Z0-9]{18,26}|(?i:xapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+)|(?i:xoxe\.xox[bp]-\d-[A-Z0-9]{163,166})|(?i:xoxe-\d-[A-Z0-9]{146})|xox[os]-\d+-\d+-\d+-[a-fA-F\d]+|xox[ar]-(?:\d-)?[0-9a-zA-Z]{8,48})",
            1,
            2.0,
            &["xoxb-", "xoxp-", "xoxe", "xapp-", "xoxa-", "xoxr-", "xoxo-", "xoxs-"],
            any,
        ),
        rule(
            "slack-webhook",
            "gitleaks slack-webhook-url",
            r"(?:https?://)?hooks\.slack\.com/((?:services|workflows|triggers)/[A-Za-z0-9+/]{43,56})",
            1,
            0.0,
            &["hooks.slack.com"],
            any,
        ),
        rule(
            "stripe-key",
            "gitleaks stripe-access-token (secretlint-rule-stripe's length)",
            r"\b((?:sk|rk)_(?:live|test|prod)_[0-9A-Za-z]{24,99})\b",
            1,
            2.0,
            &["sk_live_", "rk_live_", "sk_test_", "rk_test_", "sk_prod_", "rk_prod_"],
            any,
        ),
        rule("google-api-key", "gitleaks gcp-api-key", &format!(r"\b(AIza[0-9A-Za-z_-]{{35}}){CODE_END}"), 1, 4.0, &["aiza"], any),
        rule(
            "openai-api-key",
            "gitleaks openai-api-key",
            &format!(
                r"\b(sk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{{74}}|[A-Za-z0-9_-]{{58}})T3BlbkFJ(?:[A-Za-z0-9_-]{{74}}|[A-Za-z0-9_-]{{58}})\b|sk-[a-zA-Z0-9]{{20}}T3BlbkFJ[a-zA-Z0-9]{{20}}){CODE_END}"
            ),
            1,
            3.0,
            &["t3blbkfj"],
            any,
        ),
        rule(
            "anthropic-api-key",
            "gitleaks anthropic-api-key, anthropic-admin-api-key",
            r"\b(sk-ant-(?:api|admin)0[0-9]-[A-Za-z0-9_-]{90,128}AA)(?:[^A-Za-z0-9_-]|$)",
            1,
            0.0,
            &["sk-ant-"],
            any,
        ),
        rule("npm-token", "gitleaks npm-access-token", r"\b(npm_[A-Za-z0-9]{36})\b", 1, 0.0, &["npm_"], any),
        rule("pypi-token", "gitleaks pypi-upload-token (any index's prefix)", r"\b(pypi-[A-Za-z0-9_-]{85,1000})", 1, 3.0, &["pypi-"], any),
        rule(
            "jwt",
            "gitleaks jwt",
            &format!(r"\b(ey[a-zA-Z0-9]{{17,}}\.ey[a-zA-Z0-9/\\_-]{{17,}}\.(?:[a-zA-Z0-9/\\_-]{{10,}}={{0,2}})?){CODE_END}"),
            1,
            3.0,
            &["ey"],
            any,
        ),
        rule("twilio-api-key", "gitleaks twilio-api-key", r"\b(SK[0-9a-fA-F]{32})\b", 1, 3.0, &["sk"], any),
        rule(
            "sendgrid-api-key",
            "gitleaks sendgrid-api-token (SendGrid's 22.43 layout)",
            r"\b(SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43})\b",
            1,
            2.0,
            &["sg."],
            any,
        ),
        rule(
            "mailgun-api-key",
            "gitleaks mailgun-private-api-token",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:mailgun)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:{TYPED}|=|>|:{{1,3}}=|\|\||:|=>|\?=|,){WRAPPED}(?:[`'"\s=]|r#{{0,2}}"){{0,5}}(key-[a-f0-9]{{32}}){CODE_END}"#
            ),
            2,
            0.0,
            &["mailgun"],
            any,
        ),
        rule(
            "database-password",
            "secretlint-rule-database-connection-string",
            r"(?i)\b((?:postgres(?:ql)?|mysqlx?|mariadb|mongodb(?:\+srv)?|rediss?|amqps?|mssql|sqlserver)://)([^:/\s@]*):([^@/\s]+)@([^/\s?#]+)",
            3,
            0.0,
            &["postgres", "mysql", "mariadb", "mongodb", "redis", "amqp", "mssql", "sqlserver"],
            real_value,
        ),
        // the password of any other URL that carries one (`https://user:pass@host`)
        rule("url-password", "scrutate", r"(?i)\b[a-z][a-z0-9+.-]{1,20}://[^:/\s@]+:([^@/\s]+)@[^/\s?#]", 1, 0.0, &["://"], env_value),
        // an opaque token after `Bearer` (a header, a curl line); a JWT has its own rule
        rule("bearer-token", "scrutate", r"(?i)\bbearer[ \t]+([A-Za-z0-9._~+/-]{16,}=*)", 1, 3.0, &["bearer"], opaque),
        // HTTP Basic credentials: base64 of `user:password`
        rule("basic-auth", "scrutate", r"(?i)\bbasic[ \t]+([A-Za-z0-9+/]{8,}={0,2})", 1, 0.0, &["basic"], user_and_password),
        // a `.env` line (`NAME=value`, the name upper case): the value may hold any symbol
        rule(
            "env-secret",
            "scrutate",
            r#"\b((?:export[ \t]+)?[A-Z0-9_]*(?:SECRET|PASSW(?:OR)?D|PWD|TOKEN|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY|AUTH_?KEY|CREDENTIALS?)[A-Z0-9_]*)=["']?([^\s"'`#]{8,})"#,
            2,
            3.0,
            &["secret", "passw", "pwd", "token", "key", "credential"],
            env_value,
        ),
        // a quoted value of a password, secret or token: it may hold any symbol (`"Xq7r@T2v!W9#yZ"`)
        rule(
            "password",
            "scrutate",
            &format!(
                r#"(?i)\b(?-u:[\w.-]){{0,50}}?(?:passw(?:or)?d|pwd|secret|token)(?-u:[\w.-]){{0,20}}["']?[ \t]*(?:{TYPED}|=|:|=>|:=)[ \t]*{WRAPPED}(?:r#{{0,2}})?["'`]([^"'`\s]{{8,}})["'`]"#
            ),
            2,
            3.0,
            &["passw", "pwd", "secret", "token"],
            quoted,
        ),
        // a line of a key's base64 body read without its armour (a range that starts inside the
        // key, a search hit): base64 alone on its line, as wide as PEM writes it, near random
        rule("key-material", "scrutate", r#"(?m)(?:^|[\s:"'-])([A-Za-z0-9+/]{40,76}={0,2})["',]?[ \t]*$"#, 1, 4.5, &[], alone_on_its_line),
        rule(
            "generic-secret",
            "gitleaks generic-api-key (without its stopwords)",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:access|auth|(?-i:[Aa]pi|API)|credential|creds|key|passw(?:or)?d|secret|token)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:{TYPED}|=|>|:{{1,3}}=|\|\||:|=>|\?=|,){WRAPPED}(?:[`'"\s=]|r#{{0,2}}"){{0,5}}([\w.=-]{{10,150}}|[a-z0-9][a-z0-9+/]{{11,}}={{0,3}}){CODE_END}"#
            ),
            2,
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
fn real_value(secret: &str, _: &str, _: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    let first = secret.chars().next().unwrap_or(' ');
    !(matches!(lower.as_str(), "true" | "false" | "null" | "none" | "password" | "pass" | "secret")
        || secret.chars().all(|c| c == first)
        || secret.starts_with(['$', '{', '%', '<', '/'])
        || lower.starts_with("os."))
}

/// gitleaks' generic-api-key filters but its stopwords: a value with a digit or a symbol, a name
/// not on the allowlist, no placeholder, and no number written in code (`0xcbf29ce484222325u64`).
fn generic(secret: &str, whole: &str, before: &str) -> bool {
    secret.chars().any(|c| !(c.is_ascii_alphabetic() || matches!(c, '_' | '.' | '-')))
        && !NOT_SECRET_NAMES.is_match(whole)
        && real_value(secret, whole, before)
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
/// A bearer token: a real value with a digit in it, so a word after `Bearer` is none.
fn opaque(secret: &str, whole: &str, before: &str) -> bool {
    real_value(secret, whole, before) && secret.bytes().any(|b| b.is_ascii_digit())
}

/// Base64 that decodes to printable `user:password`.
fn user_and_password(secret: &str, _: &str, _: &str) -> bool {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let (mut bits, mut n, mut out) = (0u32, 0, Vec::new());
    for c in secret.trim_end_matches('=').bytes() {
        let Some(v) = value(c) else { return false };
        bits = (bits << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    let colon = out.iter().position(|&b| b == b':');
    colon.is_some_and(|i| i > 0 && i + 1 < out.len()) && out.iter().all(|b| (0x20..0x7f).contains(b))
}

fn env_value(secret: &str, whole: &str, before: &str) -> bool {
    real_value(secret, whole, before) && !NOT_SECRET_NAMES.is_match(whole) && secret.chars().any(|c| !c.is_ascii_alphabetic())
}

/// A quoted secret: no placeholder, URL, number or one-case name, and a digit or a symbol in it.
fn quoted(secret: &str, whole: &str, before: &str) -> bool {
    secret.len() <= 150 && env_value(secret, whole, before) && !secret.contains("://") && !NUMBER.is_match(secret) && !IDENTIFIER.is_match(secret)
}

/// Base64 that is no checksum a lockfile writes (`h1:` in go.sum, `sha512-` in package-lock),
/// after a space, a colon or a quote, or first in its line's text (grep's `path-7-` ends in `-`).
fn alone_on_its_line(_: &str, _: &str, before: &str) -> bool {
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let text = split_line(line).1;
    !HASH_PREFIX.is_match(before) && (text.is_empty() || text.ends_with([' ', '\t', ':', '"', '\'']))
}

static HASH_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:h1:|sha(?:1|224|256|384|512)[-:=]|md5[-:=])$").unwrap());

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

static PEM_BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----").unwrap());
static PEM_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----").unwrap());
static BASE64_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=]{4,}").unwrap());
/// What an answer writes before a line of a file: read_code's `    7  ` (a range's `1-2  `),
/// grep's `path:7:` and `path-7-`, `7:` and `7-`.
static LINE_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[ \t]*\d+(?:-\d+)?(?:  |[:-] ?)|[^\s:]*?[^\s:-][:-]\d+[:-] ?)").unwrap());

/// A line of an answer as (what the answer put before it, the file's text). Every rule that
/// reads a line as a whole reads its text through this, so no prefix ends a key or hides one.
pub(crate) fn split_line(line: &str) -> (&str, &str) {
    line.split_at(LINE_PREFIX.find(line).map_or(0, |m| m.end()))
}

/// One line inside a private key.
enum Body {
    /// its base64, as a byte range of the line
    Base64(usize, usize),
    /// blank, or a header such as `Proc-Type: 4,ENCRYPTED`
    Skip,
    /// no line of a key: the key has ended
    NotKey,
}

/// A line inside a private key: after an answer's line prefix, base64 alone, perhaps quoted or
/// ending in a written-out `\n` (a key in a string of code).
fn key_body(line: &str) -> Body {
    let (prefix, rest) = split_line(line);
    let content = prefix.len();
    let trimmed = rest.trim();
    if trimmed.is_empty() || trimmed.split_once(": ").is_some_and(|(h, _)| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphabetic() || c == '-')) {
        return Body::Skip;
    }
    let core = key_core(trimmed);
    if core.is_empty() || !core.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')) {
        return Body::NotKey;
    }
    let start = content + rest.find(core).unwrap_or(0);
    Body::Base64(start, start + core.len())
}

/// A key line's base64 without the quotes, separators, `+` and written-out `\n` that a key in a
/// string of code puts around it: what the index hashes and a known line is looked up by.
fn key_core(trimmed: &str) -> &str {
    let core = trimmed.trim_start_matches(['"', '\'', '`']).trim_end_matches(['"', '\'', '`', ',', ';', '+', '\\', ' ']);
    core.strip_suffix("\\n").or_else(|| core.strip_suffix("\\r\\n")).unwrap_or(core).trim_end_matches(['"', '\'', '`'])
}

/// `text` with every `\/` written `/`, and where each of its bytes came from in `text` (one more
/// entry, for the end).
fn unslash(text: &str) -> (String, Vec<usize>) {
    let (mut out, mut map, mut i) = (String::with_capacity(text.len()), Vec::with_capacity(text.len() + 1), 0);
    let b = text.as_bytes();
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'/') {
            out.push('/');
            map.push(i + 1);
            i += 2;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        for k in 0..ch.len_utf8() {
            map.push(i + k);
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    map.push(text.len());
    (out, map)
}

/// Redacts texts in order, a private key's body carried from one text into the next (the lines of
/// a file as an answer's items).
#[derive(Default)]
pub struct Redactor {
    in_key: bool,
    /// the redactions the page `ops::Answer::text_page` last made shows, in either form
    pub shown: usize,
    /// redactions made, by kind
    pub counts: std::collections::BTreeMap<&'static str, usize>,
}

impl Redactor {
    /// `text` with its secrets replaced, and where each marker is.
    pub fn redact<'a>(&mut self, text: &'a str) -> (Cow<'a, str>, Vec<Mark>) {
        let spans = self.find(text);
        if spans.is_empty() {
            return (Cow::Borrowed(text), Vec::new());
        }
        let (mut out, mut marks, mut at) = (String::with_capacity(text.len()), Vec::new(), 0);
        for (a, b, kind) in spans {
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

    /// The secrets in `text`, as byte ranges of it with their kinds, in order, none inside another.
    fn find(&mut self, text: &str) -> Vec<(usize, usize, &'static str)> {
        // JSON may write `/` as `\/` (PHP does): the rules read the text with it as `/`
        let unslashed = text.contains("\\/").then(|| unslash(text));
        let (seen, map) = match &unslashed {
            Some((t, m)) => (t.as_str(), Some(m)),
            None => (text, None),
        };
        let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
        self.private_keys(seen, &mut spans);
        known_key_lines(seen, &mut spans);
        let mut wanted: Vec<bool> = RULES.iter().map(|r| r.keywords.is_empty()).collect();
        let starts: Vec<usize> = RULES.iter().scan(0, |n, r| Some(std::mem::replace(n, *n + r.keywords.len()))).collect();
        for m in PREFILTER.find_overlapping_iter(seen) {
            wanted[starts.partition_point(|&s| s <= m.pattern().as_usize()) - 1] = true;
        }
        for (r, _) in RULES.iter().zip(&wanted).filter(|(_, &w)| w) {
            for c in r.re.captures_iter(seen) {
                let Some(g) = c.get(r.group) else { continue };
                if c.name("ann").is_some_and(|a| a.len() > TYPED_MAX) {
                    continue;
                }
                // a mark already written is no secret: the assembled page is read a second time
                if g.as_str().contains("[redacted:")
                    || (r.entropy > 0.0 && entropy(g.as_str()) <= r.entropy)
                    || !(r.keep)(g.as_str(), &c[0], &seen[..g.start()])
                {
                    continue;
                }
                spans.push((g.start(), g.end(), r.kind));
            }
        }
        if let Some(map) = map {
            spans = spans.into_iter().map(|(a, b, k)| (map[a], map[b - 1] + 1, k)).collect();
        }
        // earlier first, the longer of two at one place; one inside another goes with it
        spans.sort_by_key(|&(a, b, _)| (a, std::cmp::Reverse(b)));
        let mut at = 0;
        spans.retain(|&(a, b, _)| {
            let keep = a >= at;
            if keep {
                at = b;
            }
            keep
        });
        spans
    }

    /// The bodies of private keys: from a BEGIN armour that ends its line (or is followed by a
    /// written-out `\n`, a key in one string) to its END, every line base64 alone; a line that is
    /// something else ends the key, so armour in code (`const HDR = "-----BEGIN ...-----";`)
    /// starts none.
    fn private_keys(&mut self, text: &str, spans: &mut Vec<(usize, usize, &'static str)>) {
        if !self.in_key && !names_a_key(text) {
            return;
        }
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            self.key_line(line, at, spans);
            at += line.len();
        }
    }

    fn key_line(&mut self, line: &str, at: usize, spans: &mut Vec<(usize, usize, &'static str)>) {
        let mut from = 0;
        loop {
            let opened = !self.in_key;
            if opened {
                let Some(m) = PEM_BEGIN.find_at(line, from) else { return };
                let rest = line[m.end()..].trim_end_matches(['\n', '\r']);
                from = m.end();
                if !(rest.trim().is_empty() || rest.starts_with("\\n") || rest.starts_with("\\r\\n")) {
                    continue;
                }
                self.in_key = true;
            }
            let end = PEM_END.find_at(line, from);
            let to = end.map_or(line.len(), |m| m.start());
            if opened || end.is_some() {
                // the rest of the armour's line, or a key's body in one string
                spans.extend(BASE64_RUN.find_iter(&line[from..to]).map(|m| (at + from + m.start(), at + from + m.end(), "private-key")));
            } else {
                match key_body(&line[from..to]) {
                    Body::Base64(a, b) => spans.push((at + from + a, at + from + b, "private-key")),
                    Body::Skip => {}
                    Body::NotKey => {
                        self.in_key = false;
                        return;
                    }
                }
            }
            match end {
                Some(m) => {
                    self.in_key = false;
                    from = m.end();
                }
                None => return,
            }
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
    /// -> how many redactions each text got (0 past the ones redacted).
    pub fn redact_shown(&mut self, texts: &mut [String], first: usize, room: usize) -> Vec<usize> {
        let first = first.min(texts.len());
        texts[..first].iter().for_each(|t| self.track(t));
        let mut marks = vec![0; texts.len()];
        let mut used = 0;
        for (t, n) in texts[first..].iter_mut().zip(&mut marks[first..]) {
            if used > room {
                break;
            }
            let (redacted, m) = self.redact(t);
            *n = m.len();
            if let Cow::Owned(s) = redacted {
                *t = s;
            }
            used += t.len() + 1;
        }
        marks
    }

    /// `text` redacted in place. -> how many redactions.
    pub fn redact_in_place(&mut self, text: &mut String) -> usize {
        let (redacted, marks) = self.redact(text);
        if let Cow::Owned(s) = redacted {
            *text = s;
        }
        marks.len()
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

/// `text` with the closing line for `n` redactions after it, when there were any.
pub fn close(text: &mut String, n: usize) {
    if let Some(note) = note(n) {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&note);
    }
}

/// Every detector: its kind and where its rule comes from (docs/SECURITY.md lists them).
pub fn detectors() -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = RULES.iter().map(|r| (r.kind, r.source)).collect();
    out.push(("private-key", "gitleaks private-key, secretlint-rule-privatekey (line by line between the armour)"));
    out
}

/// The secrets in `text` (one line or more), as byte ranges of it with their kinds, in order.
pub fn spans(text: &str) -> Vec<(usize, usize, &'static str)> {
    Redactor::default().find(text)
}

/// `text` with its secrets replaced, and where each marker is.
pub fn redact(text: &str) -> (Cow<'_, str>, Vec<Mark>) {
    Redactor::default().redact(text)
}

/// `text` redacted, where byte `at` of `text` is in it (the start of a secret that holds it), and
/// how many secrets went.
pub(crate) fn redact_keeping(text: &str, at: usize) -> (String, usize, usize) {
    let found = spans(text);
    let (mut out, mut moved, mut last) = (String::with_capacity(text.len()), at, 0);
    for &(a, b, kind) in &found {
        out.push_str(&text[last..a]);
        let mark = format!("[redacted:{kind}]");
        if a < at {
            moved = if at < b { out.len() } else { moved + mark.len() - (b - a) };
        }
        out.push_str(&mark);
        last = b;
    }
    out.push_str(&text[last..]);
    let moved = moved.min(out.len());
    (out, moved, found.len())
}

/// For each root whose index was read (`know_key_lines_at`), the index's manifest time and its
/// private-key body lines by `key_line_hash`: a line of a key shown anywhere, with or without its
/// file, its line number or its armour, is found by its text.
type KnownRoot = (std::path::PathBuf, Option<std::time::SystemTime>, std::sync::Arc<std::collections::HashSet<i64>>);
static KNOWN_KEY_LINES: std::sync::RwLock<Vec<KnownRoot>> = std::sync::RwLock::new(Vec::new());

/// How many places in a line are tried as the start of a key's line: after an answer's or a
/// program's prefix (`path:3:`, `path-4-`, a tab), not after every dash of a long line.
const KEY_LINE_STARTS: usize = 8;

/// The key body lines the index of `root` (or of the nearest directory above it that has one)
/// recorded, for every redactor of this process to find by their text. Read again only when the
/// index's database or its write-ahead log changed.
pub(crate) fn know_key_lines_at(root: &std::path::Path) {
    let Some(top) = root.ancestors().find(|d| crate::textindex::exists(d)) else { return };
    let stamp = std::fs::metadata(crate::textindex::dir_of(top).join("manifest.json")).and_then(|m| m.modified()).ok();
    let fresh = |known: &[KnownRoot]| known.iter().any(|(r, t, _)| r == top && *t == stamp);
    if fresh(&KNOWN_KEY_LINES.read().unwrap_or_else(|e| e.into_inner())) {
        return;
    }
    let hashes = crate::textindex::TextIndex::open(top).ok().flatten().map(|t| t.key_line_hashes()).unwrap_or_default();
    let mut known = KNOWN_KEY_LINES.write().unwrap_or_else(|e| e.into_inner());
    known.retain(|(r, _, _)| r != top);
    known.push((top.to_path_buf(), stamp, std::sync::Arc::new(hashes)));
}

/// FNV-1a 64 of a key body line's text, without its surrounding blanks: what the index stores,
/// the same in every build.
pub(crate) fn key_line_hash(text: &str) -> i64 {
    text.trim().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3)) as i64
}

/// The text of every line of a private key's body in `text`, as the private-key pass reads them.
pub(crate) fn key_body_lines(text: &str) -> Vec<String> {
    if !names_a_key(text) {
        return Vec::new();
    }
    let mut r = Redactor::default();
    let mut spans = Vec::new();
    r.private_keys(text, &mut spans);
    spans.iter().map(|&(a, b, _)| text[a..b].to_string()).filter(|l| l.len() >= 4).collect()
}

/// Every line of `text` whose text, whole or after one of its first separators, is a known key
/// body line: as a span of that text.
fn known_key_lines(text: &str, spans: &mut Vec<(usize, usize, &'static str)>) {
    // a line any root's index knows is redacted in every root's answers: more marks, never fewer
    let known: Vec<_> = KNOWN_KEY_LINES.read().unwrap_or_else(|e| e.into_inner()).iter().filter(|k| !k.2.is_empty()).map(|k| k.2.clone()).collect();
    if known.is_empty() {
        return;
    }
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let starts = std::iter::once(0).chain(line.match_indices([':', '-', '\t']).map(|(i, _)| i + 1).take(KEY_LINE_STARTS));
        for start in starts {
            let rest = &line[start..];
            let core = key_core(rest.trim());
            if core.len() >= 4 && known.iter().any(|k| k.contains(&key_line_hash(core))) {
                let a = at + start + rest.find(core).unwrap_or(0);
                spans.push((a, a + core.len(), "private-key"));
                break;
            }
        }
        at += line.len();
    }
}

/// Whether `text` says "private key" in any case, as a key's armour does (`PRIVATE KEY`, `Rsa
/// Private Key`): what the private-key pass needs before it reads a text.
pub(crate) fn names_a_key(text: &str) -> bool {
    static NAME: LazyLock<aho_corasick::AhoCorasick> =
        LazyLock::new(|| aho_corasick::AhoCorasick::builder().ascii_case_insensitive(true).build(["private key"]).unwrap());
    NAME.is_match(text)
}

/// The file and line number grep and ripgrep write before a line (`./a.pem:3:`, `a.pem-4-`), from
/// the prefix `split_line` takes off; `None` when the prefix names no file (`3:`).
pub(crate) fn grep_place(prefix: &str) -> Option<(&str, u64)> {
    let rest = prefix.trim_end().strip_suffix([':', '-'])?;
    let (path, n) = rest.rsplit_once([':', '-'])?;
    Some((path, n.parse().ok()?)).filter(|(p, _)| !p.is_empty())
}

/// Whether a line of an answer could be a line of a key's body: its text base64 alone.
pub(crate) fn could_be_key_line(line: &str) -> bool {
    matches!(key_body(line), Body::Base64(..))
}

/// The lines of private keys' bodies in files, redacted as each whole file reads them: a line of
/// a key's body shown without its armour (a search hit, a page from the middle) is still one.
/// A file is read as a stream, only as far as the last line asked for, each line up to
/// `KEY_LINE_MAX` bytes, and only its key lines are kept; one in UTF-16 is decoded as the engines
/// decode it.
#[derive(Default)]
pub(crate) struct KeyFiles(std::collections::HashMap<String, KeyFile>);

/// No line of a key's body comes near this; a longer line is read up to it and the rest skipped.
const KEY_LINE_MAX: u64 = 4096;

struct KeyFile {
    lines: FileLines,
    r: Redactor,
    read: u64,
    keys: std::collections::HashMap<u64, (String, usize)>,
}

enum FileLines {
    Stream(std::io::BufReader<std::fs::File>, Vec<u8>),
    Decoded(std::vec::IntoIter<String>),
    Done,
}

impl FileLines {
    fn open(file: Option<std::fs::File>) -> FileLines {
        use std::io::{BufRead, Read};
        let Some(file) = file else { return FileLines::Done };
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        let utf16 = reader.fill_buf().is_ok_and(|b| b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]));
        if !utf16 {
            return FileLines::Stream(reader, Vec::new());
        }
        let mut raw = Vec::new();
        match reader.read_to_end(&mut raw).ok().and(crate::index::decode_source(&raw)) {
            Some(text) => FileLines::Decoded(text.lines().map(str::to_string).collect::<Vec<_>>().into_iter()),
            None => FileLines::Done,
        }
    }

    /// The next line into `out`, without its line break. -> false at the end.
    fn next(&mut self, out: &mut String) -> bool {
        use std::io::{BufRead, Read};
        out.clear();
        match self {
            FileLines::Stream(reader, buf) => {
                buf.clear();
                let read = reader.by_ref().take(KEY_LINE_MAX).read_until(b'\n', buf).unwrap_or(0);
                if read == 0 {
                    return false;
                }
                if !buf.ends_with(b"\n") && read as u64 == KEY_LINE_MAX && reader.skip_until(b'\n').is_err() {
                    return false;
                }
                out.push_str(String::from_utf8_lossy(buf).trim_end_matches(['\n', '\r']));
                true
            }
            FileLines::Decoded(lines) => lines.next().map(|l| out.push_str(&l)).is_some(),
            FileLines::Done => false,
        }
    }
}

impl KeyFiles {
    /// Line `n` (from 1) of the file at `path` redacted, when it is a line of a private key's
    /// body, with how many redactions; `open` gives the file the first time it is asked for.
    pub(crate) fn line(&mut self, path: &str, n: u64, open: impl FnOnce() -> Option<std::fs::File>) -> Option<(&str, usize)> {
        let f = self.0.entry(path.to_string()).or_insert_with(|| KeyFile {
            lines: FileLines::open(open()),
            r: Redactor::default(),
            read: 0,
            keys: Default::default(),
        });
        let mut line = String::new();
        while f.read < n && f.lines.next(&mut line) {
            f.read += 1;
            if f.r.in_key || names_a_key(&line) {
                let (out, marks) = f.r.redact(&line);
                if marks.iter().any(|m| m.kind == "private-key") {
                    f.keys.insert(f.read, (out.into_owned(), marks.len()));
                }
            }
        }
        f.keys.get(&n).map(|(l, k)| (l.as_str(), *k))
    }
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
    /// `text` in base64, as an HTTP Basic header carries it.
    pub(crate) fn basic(text: &str) -> String {
        const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in text.as_bytes().chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                out.push(if i <= chunk.len() { SET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }

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
            line("url-password", "remote = https://deploy:", format!("{}!{}", fake(62, ALNUM, 8), fake(63, ALNUM, 6)), "@git.example.com/app.git"),
            line("bearer-token", "headers = {\"Authorization\": \"Bearer ", format!("{}7", fake(64, ALNUM, 31)), "\"}"),
            line("basic-auth", "curl -H 'Authorization: Basic ", basic(&format!("deploy:{}", fake(65, ALNUM, 14))), "'"),
            line("env-secret", "DB_PASSWORD=", format!("{}!{}", fake(27, ALNUM, 8), fake(28, ALNUM, 8)), ""),
            line("password", "db_password = \"", format!("{}@{}!{}#", fake(34, ALNUM, 4), fake(35, ALNUM, 4), fake(36, ALNUM, 4)), "\""),
            line("generic-secret", "client_secret: ", fake(29, ALNUM, 18), ""),
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
            "wdrtas_token_get_sp = rtas_token(\"ibm,get-system-parameter\");",
            // the shapes of credentials without one
            "headers[\"Authorization\"] = \"Bearer \" + token",
            "Authorization: Bearer <token>",
            "The Bearer authentication scheme sends a bearer token.",
            "git clone https://user:password@example.com/repo.git",
            "url = \"https://${USER}:${PASS}@example.com\"",
            "WWW-Authenticate: Basic realm=\"staging\"",
            "Use basic authentication over TLS only.",
            // a typed parameter list: the annotation stops at its parameter's comma
            "    def __init__(self, token: str, repo: Optional[str] = \"AdminUser2024\"):",
            "fun connect(token: String, user: String = \"AdminUser2024\") {",
            "  constructor(token: string, user: string = \"AdminUser2024\") {",
            "    init(token: String, user: String = \"AdminUser2024\") {",
        ] {
            let (out, marks) = redact(line);
            assert!(marks.is_empty(), "{line} -> {out}");
        }
    }

    /// Armour in code is no key, a key's short last line goes, JSON's `\/` hides nothing, a
    /// password may hold any symbol, a go.sum hash is no key.
    #[test]
    fn armour_in_code_short_lines_escaped_slashes_symbols_and_hashes() {
        let mut r = Redactor::default();
        let code = [&*format!("const hdr = \"{}\";", armour("BEGIN", "RSA ")), "function parsePemHeaderLine(s) {", "  return s.startsWith(hdr);"];
        for line in code {
            assert_eq!(r.redact(line).0, line);
        }
        let mut r = Redactor::default();
        let body = format!("MHcCAQEEIBq7{}", fake(37, ALNUM, 52));
        let key = [armour("BEGIN", "EC "), body, "AwEHoUQ==".to_string(), armour("END", "EC ")];
        let out: Vec<String> = key.iter().map(|l| r.redact(l).0.into_owned()).collect();
        assert_eq!(out, [key[0].as_str(), "[redacted:private-key]", "[redacted:private-key]", key[3].as_str()]);
        let aws = format!("{}/{}/{}", fake(38, ALNUM, 13), fake(39, ALNUM, 7), fake(40, ALNUM, 18));
        let hook = fake(41, ALNUM, 24);
        let db = fake(42, ALNUM, 10);
        let pw = format!("{}@{}!{}#", fake(43, ALNUM, 4), fake(44, ALNUM, 4), fake(45, ALNUM, 4));
        for (line, secret) in [
            (format!(r#"{{"aws_secret_access_key":"{}"}}"#, aws.replace('/', r"\/")), aws.replace('/', r"\/")),
            (format!(r#"{{"hook":"https:\/\/hooks.slack.com\/services\/T0{}\/B0{}\/{hook}"}}"#, fake(46, ALNUM, 7), fake(47, ALNUM, 7)), hook),
            (format!(r#"{{"db":"postgres:\/\/app:{db}@db.internal\/app"}}"#), db),
            (format!("db_password = \"{pw}\""), pw),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(&secret) && !marks.is_empty(), "{line} -> {out}");
        }
        let gosum = "golang.org/x/net v0.1.0 h1:tL+3tIxTq9jVFpN5MbnSqGTgUx3V/3mIC+r9hXu5rXo=";
        assert_eq!(redact(gosum).0, gosum);
    }

    /// A value longer than any rule's usual width goes whole, not its first 200 characters.
    #[test]
    fn a_long_env_value_is_redacted_whole() {
        let line = format!("DB_PASSWORD={}", fake(48, ALNUM, 250));
        let (out, marks) = redact(&line);
        assert_eq!(marks.len(), 1);
        assert_eq!(out.trim_start_matches("DB_PASSWORD="), "[redacted:env-secret]");
    }

    /// A typed assignment holds its secret as a plain one does, in each language that writes one.
    #[test]
    fn a_typed_assignment_is_redacted() {
        let aws = fake(49, ALNUM, 40);
        let v = format!("{}7", fake(50, ALNUM, 20));
        // a type over 80 characters is no annotation (a limit SECURITY.md states)
        let long = format!("let api_secret: {} = \"{v}\";", "T".repeat(90));
        assert!(!redact(&long.replace(&"T".repeat(90), "T")).1.is_empty(), "{long}");
        assert!(redact(&long).1.is_empty(), "{long}");
        for line in [
            format!("pub const aws_secret_access_key: &str = \"{aws}\";"),
            format!("pub static CLIENT_SECRET: &'static str = \"{v}\";"),
            format!("let clientSecret: String = \"{v}\""),
            format!("const apiToken: string = \"{v}\";"),
            format!("var clientSecret string = \"{v}\""),
            format!("val apiSecret: String = \"{v}\""),
            format!("private static final String CLIENT_SECRET = \"{v}\";"),
            format!("let headers_token: HashMap<String, String> = \"{v}\".into();"),
            format!("var apiToken: String? = \"{v}\""),
            format!("  apiToken?: string = \"{v}\";"),
            format!("let authToken: string | undefined = \"{v}\";"),
            format!("let client_secret: Option<&str> = Some(\"{v}\");"),
            format!("let api_secret: String = String::from(\"{v}\");"),
            format!("let api_secret: String = \"{v}\".to_string();"),
            format!("Optional<String> apiToken = Optional.of(\"{v}\");"),
            format!("  apiToken!: string = \"{v}\";"),
            format!("let client_secret: Option<String> = Some(String::from(\"{v}\"));"),
            format!("let api_secret: HashMap<String, Vec<u8>> = \"{v}\".into();"),
            format!("const API_SECRET: &str = r\"{v}\";"),
            format!("const API_SECRET: &str = r#\"{v}\"#;"),
            format!("let aws_secret_access_key = r\"{aws}\";"),
        ] {
            let (out, marks) = redact(&line);
            assert!(!marks.is_empty() && !out.contains(&aws) && !out.contains(&v), "{line} -> {out}");
        }
    }

    /// A process that serves two repositories finds the key lines of both, whichever it read last.
    #[test]
    fn the_key_lines_of_every_root_read_are_known() {
        let base = std::env::temp_dir().join(format!("scrutate_key_roots_{}", std::process::id()));
        let b64 = format!("{ALNUM}+/");
        let mut first = Vec::new();
        for (i, name) in ["one", "two"].into_iter().enumerate() {
            let root = base.join(name);
            std::fs::create_dir_all(root.join(".git")).unwrap();
            // a line ending in `+`, which a key in code also writes as a concatenation
            let body = format!("M{}{}", fake(90 + i as u64, &b64, 62), ["+", "/"][i]);
            std::fs::write(root.join("k.pem"), format!("{}\n{body}\n{}\n", armour("BEGIN", ""), armour("END", ""))).unwrap();
            crate::textindex::update(&root, true, false).unwrap();
            know_key_lines_at(&root);
            first.push(body);
        }
        for body in &first {
            let mut spans = Vec::new();
            known_key_lines(&format!("k.pem:2:{body}"), &mut spans);
            assert_eq!(spans.len(), 1, "{body}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The page as assembled is read again, so a mark must not be taken for a secret twice.
    #[test]
    fn a_mark_is_not_redacted_again() {
        for line in ["DB_PASSWORD=[redacted:env-secret]", "db_password = \"[redacted:password]\"", "client_secret: [redacted:generic-secret]"] {
            let (out, marks) = redact(line);
            assert!(marks.is_empty(), "{line} -> {out}");
        }
    }

    #[test]
    fn grep_s_prefix_names_the_file_and_line() {
        assert_eq!(grep_place("./keys.pem:3:"), Some(("./keys.pem", 3)));
        assert_eq!(grep_place("a-b.pem-12-"), Some(("a-b.pem", 12)));
        assert_eq!(grep_place("3:"), None);
        assert_eq!(grep_place(""), None);
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

    /// False redactions on a real tree: every line of every file a search of `SCRUTATE_FP_ROOT`
    /// covers (`files::look`) through `redact`, each mark printed with its line, and the rate per
    /// 10,000 lines.
    #[test]
    #[ignore = "needs SCRUTATE_FP_ROOT"]
    fn false_redactions_per_ten_thousand_lines() {
        let Some(root) = std::env::var_os("SCRUTATE_FP_ROOT") else { return };
        let (mut lines, mut marks) = (0u64, 0u64);
        let root = std::path::PathBuf::from(root);
        for f in crate::files::look(&root, true).unwrap().into_iter().filter(|f| f.repo) {
            let path = root.join(&f.path);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let mut r = Redactor::default();
            for (i, line) in text.lines().enumerate() {
                lines += 1;
                let (out, m) = r.redact(line);
                if !m.is_empty() {
                    marks += m.len() as u64;
                    println!("{}:{}: {}", path.display(), i + 1, out.chars().take(200).collect::<String>());
                }
            }
        }
        println!("{marks} marks in {lines} lines: {:.3} per 10,000", marks as f64 * 10_000.0 / lines.max(1) as f64);
    }

    /// What redaction costs on the largest page an answer shows (24,000 bytes), as `text_page`
    /// redacts it: line by line before the budget pages it, then the page once more (release
    /// build: `tools/secret_redactions.py`).
    #[test]
    #[ignore = "a timing"]
    fn the_cost_of_redacting_large_answers() {
        let src: String = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect::<Vec<_>>()
            .concat();
        let page = &src[..src.floor_char_boundary(24_000)];
        let t0 = std::time::Instant::now();
        LazyLock::force(&RULES);
        LazyLock::force(&PREFILTER);
        for re in [&NOT_SECRET_NAMES, &IDENTIFIER, &NUMBER, &HASH_PREFIX, &PEM_BEGIN, &PEM_END, &BASE64_RUN, &LINE_PREFIX] {
            LazyLock::force(re);
        }
        println!("building the detectors, once a process: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t0 = std::time::Instant::now();
                let mut r = Redactor::default();
                let mut lines: Vec<String> = page.lines().map(str::to_string).collect();
                r.redact_shown(&mut lines, 0, usize::MAX);
                let mut whole = lines.join("\n");
                std::hint::black_box(r.redact_in_place(&mut whole));
                t0.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("24,000-byte page: {:.3} ms per answer (median of 101; fastest {:.3})", times[50], times[0]);
    }

    #[test]
    fn the_switch_reads_the_environment_over_the_default() {
        // only this test reads SCRUTATE_REDACT in this binary
        assert!(on(true) || std::env::var("SCRUTATE_REDACT").is_ok());
        assert!(!on(false) || std::env::var("SCRUTATE_REDACT").is_ok());
    }
}
