//! Secrets in answer text: each one the detectors find is replaced with `[redacted:KIND]`, its key
//! name, scheme or PEM armour left in place. Most detectors are ports of gitleaks v8.30.1 and
//! secretlint v13.1.0 rules, each naming its source; a few are scrutate's own. A rule's regex runs
//! only on text that holds one of its keywords. Encoded secrets (base64, hex, URL-encoded) are not
//! looked for.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// One redaction: its kind, the byte span of its marker in the redacted text and of its secret in
/// the text as it was.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
    pub secret: (usize, usize),
}

/// Where byte `at` of a text is once `marks` redacted it: moved by each marker before it, or the
/// start of the marker of a secret that holds it.
pub fn redacted_at(at: usize, marks: &[Mark]) -> usize {
    let mut moved = at as isize;
    for m in marks {
        if at < m.secret.0 {
            break;
        }
        if at < m.secret.1 {
            return m.start;
        }
        moved += (m.end - m.start) as isize - (m.secret.1 - m.secret.0) as isize;
    }
    moved as usize
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
    /// the pattern, compiled the first time a text holds one of the keywords: a hook call pays only
    /// for the rules its text can match
    pattern: String,
    re: std::sync::OnceLock<Regex>,
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

/// What may follow a secret: a quote, space, `;`, an escaped line end, the line's end, or `&`
/// before a URL's next query parameter.
const CODE_END: &str = r#"(?:[`'"\s;&]|\\[nr]|$)"#;

/// A `:` or `,` with more of a value after it: `value_end` takes the rest. One that ends a value
/// (`password: x!, user: y`, `f(x!):`) is not told from code that lists or calls, so it is not one.
const VALUE_END: &str = r#"(?u:[:,][^\s"'`;&:,])"#;

/// What ends an unquoted value after a `&` or `;`: another parameter, its name `=` (a connection
/// string's `Initial Catalog=` holds spaces), behind HTML's `&amp;` or `&#38;` too; another `&`
/// (`&&`); a cookie's attribute; or a word and a space, another command (`;echo done`).
static PARAMETER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:amp;|#38;)?[A-Za-z_][\w.-]*(?: [A-Za-z_][\w.-]*)*=|&|(?:HttpOnly|Secure|Partitioned)(?:[;\s]|$)|[A-Za-z_][\w.-]*\s+\S)").unwrap()
});

/// A key after a `,`: its name and `=` or `:` (`,user=deploy`, `,status: :ok`).
static KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w.-]*\s*[=:]").unwrap());

/// Where an unquoted value that a rule ended at `end` ends: a closing bracket ends it; a `&`, `;`,
/// `:`, `,` or opening bracket ends it only before a space or the line's end, after `&` or `;`
/// what `PARAMETER` reads as the next item, after `,` a `KEY`; else the value goes on past it, so
/// none of it is left shown.
fn value_end(text: &str, mut end: usize) -> usize {
    const ON: &[char] = &['&', ';', ':', ',', '(', '[', '{', '<'];
    while let Some(sep) = text[end..].chars().next().filter(|c| ON.contains(c)) {
        let after = &text[end + 1..];
        let next_item = match sep {
            '&' | ';' => PARAMETER.is_match(after),
            ',' => KEY.is_match(after),
            _ => false,
        };
        if after.is_empty() || after.starts_with(char::is_whitespace) || next_item {
            break;
        }
        end += 1 + after.find(|c: char| c.is_whitespace() || "`'\")]}>".contains(c) || ON.contains(&c)).unwrap_or(after.len());
    }
    end
}

/// A typed assignment's annotation and `=` (`: &str =`, `?: string | undefined =`, `!: string =`,
/// `: HashMap<String, Vec<u8>> =`): printable ASCII but `=`, `;`, `{`, `}`, `!` and quotes (a
/// lifetime's `'a` aside), and `,`, `(` and `)` only inside angle brackets, so no language's syntax
/// is listed and a run never passes a value or another parameter. A colon only as a path's `::`:
/// a run stops at the next `key:`, so a long line of `key: value` pairs is read once, not once
/// per pair. `find` drops a match whose annotation (group `ann`) is over `TYPED_MAX`: a bounded
/// repetition here is slow to build.
/// Taken first where an assignment rule takes `=` or `:`.
const TYPED: &str = r#"[?!]?:(?P<ann>(?:[\t\x20\x23-\x26\x2a\x2b\x2d-\x39\x3f-\x5f\x61-\x7a\x7c\x7e]|::|'[A-Za-z_]|<(?:[\t\x20\x23-\x26\x28-\x39\x3f-\x5f\x61-\x7a\x7c\x7e]|::|<(?:[\t\x20\x23-\x26\x28-\x39\x3f-\x5f\x61-\x7a\x7c\x7e]|::)*>)*>)+)="#;
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
    Rule { kind, source, pattern: re.to_string(), re: std::sync::OnceLock::new(), group, entropy, keywords, keep }
}

impl Rule {
    fn regex(&self) -> &Regex {
        self.re.get_or_init(|| Regex::new(&self.pattern).unwrap_or_else(|e| panic!("{}: {e}", self.kind)))
    }
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
            r"((?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{36})",
            1,
            3.0,
            &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
            any,
        ),
        rule("github-fine-grained-token", "gitleaks github-fine-grained-pat", r"(github_pat_[0-9A-Za-z_]{82})", 1, 3.0, &["github_pat_"], any),
        rule("gitlab-token", "gitleaks gitlab-pat, gitlab-pat-routable", r"(glpat-[0-9A-Za-z_-]{20,300}(?:\.[0-9a-z]{9})?)", 1, 3.0, &["glpat-"], any),
        rule(
            "slack-token",
            "gitleaks slack-bot-token, slack-user-token, slack-app-token, slack-config-*, slack-legacy-*",
            r"(xoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*|xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}|xoxb-[0-9]{8,14}-[a-zA-Z0-9]{18,26}|(?i:xapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+)|(?i:xoxe\.xox[bp]-\d-[A-Z0-9]{163,166})|(?i:xoxe-\d-[A-Z0-9]{146})|xox[os]-\d+-\d+-\d+-[a-fA-F\d]+|xox[ar]-(?:\d-)?[0-9a-zA-Z]{8,48})",
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
        rule("npm-token", "gitleaks npm-access-token", r"\b((?i:npm_)[A-Za-z0-9]{36})\b", 1, 0.0, &["npm_"], any),
        rule("pypi-token", "gitleaks pypi-upload-token (any index's prefix)", r"(pypi-[A-Za-z0-9_-]{85,1000})", 1, 3.0, &["pypi-"], any),
        rule(
            "jwt",
            "gitleaks jwt",
            &format!(r"\b(ey[a-zA-Z0-9]{{17,}}\.ey[a-zA-Z0-9/\\_-]{{17,}}\.(?:[a-zA-Z0-9/\\_-]{{10,}}={{0,2}})?){CODE_END}"),
            1,
            3.0,
            &["ey"],
            any,
        ),
        rule("twilio-api-key", "gitleaks twilio-api-key", r"(SK[0-9a-fA-F]{32})", 1, 3.0, &["sk"], any),
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
            r"(?i)\b((?:postgres(?:ql)?|mysqlx?|mariadb|mongodb(?:\+srv)?|rediss?|amqps?|mssql|sqlserver)://)([^:/\s@]*):([^/\s]+)@([^/\s?#@]+)",
            3,
            0.0,
            &["postgres", "mysql", "mariadb", "mongodb", "redis", "amqp", "mssql", "sqlserver"],
            real_value,
        ),
        rule(
            "huggingface-token",
            "gitleaks huggingface-access-token, huggingface-organization-api-token",
            &format!(r"\b((?:hf_|api_org_)(?i:[a-z]{{34}})){CODE_END}"),
            1,
            2.0,
            &["hf_", "api_org_"],
            any,
        ),
        rule(
            "shopify-token",
            "gitleaks shopify-access-token, shopify-custom-access-token, shopify-private-app-access-token, shopify-shared-secret",
            r"(shp(?:at|ca|pa|ss)_[a-fA-F0-9]{32})",
            1,
            2.0,
            &["shpat_", "shpca_", "shppa_", "shpss_"],
            any,
        ),
        rule(
            "digitalocean-token",
            "gitleaks digitalocean-pat, digitalocean-access-token, digitalocean-refresh-token",
            &format!(r"(?i)\b(do[opr]_v1_[a-f0-9]{{64}}){CODE_END}"),
            1,
            3.0,
            &["dop_v1_", "doo_v1_", "dor_v1_"],
            any,
        ),
        rule(
            "vault-token",
            "gitleaks vault-service-token (the `hvs.` form), vault-batch-token",
            &format!(r"\b(hvs\.[A-Za-z0-9_-]{{90,120}}|hvb\.[A-Za-z0-9_-]{{138,300}}){CODE_END}"),
            1,
            3.5,
            &["hvs.", "hvb."],
            any,
        ),
        rule(
            "telegram-bot-token",
            "gitleaks telegram-bot-api-token",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:telegr)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:=|>|:{{1,3}}=|\|\||:|=>|\?=|,)[`'"\s=]{{0,5}}([0-9]{{5,16}}:(?-i:A)[a-z0-9_-]{{34}}){CODE_END}"#
            ),
            1,
            0.0,
            &["telegr"],
            any,
        ),
        // a token as a URL's user, alone or before a placeholder password, as a clone URL carries one
        rule("url-token", "scrutate", r"(?i)\b[a-z][a-z0-9+.-]{1,20}://([A-Za-z0-9_-]{20,})(?::[^@/\s]*)?@[^/\s?#]", 1, 3.0, &["://"], opaque),
        // the password of any other URL that carries one (`https://user:pass@host`)
        rule("url-password", "scrutate", r"(?i)\b[a-z][a-z0-9+.-]{1,20}://[^:/\s@]*:([^/\s]+)@[^/\s?#@]", 1, 0.0, &["://"], url_password),
        // an opaque token after `Bearer` or an `Authorization: Token` scheme (a header, a curl
        // line); a JWT has its own rule
        rule(
            "bearer-token",
            "scrutate",
            r#"(?i)(?:\bbearer|\bauthorization["']?\s*[:=]\s*["']?token)[ \t]+([A-Za-z0-9._~+/-]{16,}=*)"#,
            1,
            3.0,
            &["bearer", "authorization"],
            opaque,
        ),
        // the password after the user that curl's `-u`, `--user` (a value joined to `-u` too) or a
        // proxy's `-U`, `--proxy-user` gives
        rule(
            "curl-user",
            "scrutate",
            r#"(?i)\bcurl\b[^\n|;&]*?\s(?:-u[ \t]*|--(?:proxy-)?user(?:[ \t]+|=))["']?[^\s:"']+:([^\s"']+)"#,
            1,
            0.0,
            &["curl"],
            url_password,
        ),
        // HTTP Basic credentials: base64 of `user:password`
        rule("basic-auth", "scrutate", r"(?i)\bbasic[ \t]+([A-Za-z0-9+/]{8,}={0,2})", 1, 0.0, &["basic"], user_and_password),
        // a `.env` line (`NAME=value`, the name upper case): the value may hold any symbol; `PASS`
        // only as the whole name or after `_` (`DB_PASS`, not `BYPASS`)
        rule(
            "env-secret",
            "scrutate",
            r#"\b((?:export[ \t]+)?(?:[A-Z0-9_]*(?:SECRET|PASSW(?:OR)?D|PWD|TOKEN|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY|AUTH_?KEY|CREDENTIALS?)[A-Z0-9_]*|(?:[A-Z0-9_]*_)?PASS))=["']?([^\s"'`#]{8,})"#,
            2,
            3.0,
            &["secret", "pass", "pwd", "token", "key", "credential"],
            env_secret_value,
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
        // XML: an element whose name ends in a password, secret or token (`<password>`,
        // `<ns:accessToken>`), or a .NET `<add key="..." value="...">` whose key does
        rule(
            "xml-secret",
            "scrutate",
            r#"(?i)(?:<(?:[\w.-]+:)?[\w.-]*(?:passw(?:or)?d|pwd|secret(?:[_-]?key)?|token|api[_-]?key)>|\bkey=["'][^"'<>]*(?:passw(?:or)?d|pwd|secret(?:[_-]?key)?|token|api[_-]?key)["'][ \t]+value=["'])([^<"'\s]{8,})(?:</|["'])"#,
            1,
            3.0,
            &["passw", "pwd", "secret", "token", "apikey", "api_key", "api-key"],
            xml_value,
        ),
        // and such an element's whole text to its closing tag, a quote or a space in it
        rule(
            "xml-secret",
            "scrutate",
            r#"(?i)<(?:[\w.-]+:)?[\w.-]*(?:passw(?:or)?d|pwd|secret(?:[_-]?key)?|token|api[_-]?key)>([^<\r\n]{8,})</"#,
            1,
            3.0,
            &["passw", "pwd", "secret", "token", "apikey", "api_key", "api-key"],
            element_text,
        ),
        // a line of a key's base64 body read without its armour (a range that starts inside the
        // key, a search hit): base64 alone on its line, as wide as PEM writes it, near random
        rule("key-material", "scrutate", r#"(?m)(?:^|[\s:"'-])([A-Za-z0-9+/]{40,76}={0,2})["',]?[ \t]*$"#, 1, 4.5, &[], alone_on_its_line),
        rule(
            "generic-secret",
            "gitleaks generic-api-key (without its stopwords)",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:access|auth|(?-i:[Aa]pi|API)|credential|creds|key|passw(?:or)?d|secret|token)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:{TYPED}|=|>|:{{1,3}}=|\|\||:|=>|\?=|,){WRAPPED}(?:[`'"\s=]|r#{{0,2}}"){{0,5}}([\w.=-]{{10,150}}|[a-z0-9][a-z0-9+/]{{11,}}={{0,3}}|(?u:[^\s"'`;&()\[\]{{}}<>,:]{{10,150}})){CODE_END}"#
            ),
            2,
            3.5,
            &["access", "api", "auth", "key", "credential", "creds", "passwd", "password", "secret", "token"],
            generic,
        ),
        // the same, for a value with a symbol that holds a `:` or `,`
        rule(
            "generic-secret",
            "gitleaks generic-api-key (without its stopwords)",
            &format!(
                r#"(?i-u)[\w.-]{{0,50}}?(?:access|auth|(?-i:[Aa]pi|API)|credential|creds|key|passw(?:or)?d|secret|token)(?:[ \t\w.-]{{0,20}})[\s'"]{{0,3}}(?:{TYPED}|=|>|:{{1,3}}=|\|\||:|=>|\?=|,){WRAPPED}(?:[`'"\s=]|r#{{0,2}}"){{0,5}}((?u:[^\s"'`;&()\[\]{{}}<>,:\\]{{10,150}})){VALUE_END}"#
            ),
            2,
            3.5,
            &["access", "api", "auth", "key", "credential", "creds", "passwd", "password", "secret", "token"],
            generic_with_symbol,
        ),
    ]
});

/// Names whose keyword is part of a longer word or names something that is no secret (gitleaks'
/// generic-api-key allowlist, tested on the whole match).
static NOT_SECRET_NAMES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)accessibility|accessor|access_id|random_access|api_id|api_name|api_version|rapid|capital|author|credentials_id|withcredentials|bucket_key|foreign_key|primary_key|public_key|schema_key|sequence_key|turkey|monkey|key_alias|key_id|key_length|key_name|key_pair|key_size|keypress|keystore_file|keytab_path|keying|secret_length|secret_name|secret_size|usersecretsid|csrf_token|public_token|token_file|key_file|api_endpoint|api_url|token_uri|io\.jsonwebtoken|authentication-results|x-ms-exchange-organization-auth").unwrap()
});

/// A value that is no placeholder: not `true`, `false` or `null`, not one character repeated, not
/// a variable (`$VAR`, `${VAR}`, `{{ x }}`, `%VAR%`, `<value>`), not an absolute path. Base64 that
/// starts with `/` and ends in `=` padding, as no path does, is no path.
fn real_value(secret: &str, _: &str, _: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    let first = secret.chars().next().unwrap_or(' ');
    let base64 = secret.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) && secret.ends_with('=');
    !(matches!(lower.as_str(), "true" | "false" | "null" | "none")
        || PLACEHOLDER_PASSWORDS.contains(&lower.as_str())
        || secret.chars().all(|c| c == first)
        || secret.starts_with(['$', '{', '%', '<'])
        || secret.starts_with('/') && !base64
        || lower.starts_with("os."))
}

/// gitleaks' generic-api-key filters but its stopwords: a value with a digit or a symbol, a name
/// not on the allowlist, no placeholder, and no number written in code (`0xcbf29ce484222325u64`).
/// A value with a symbol outside gitleaks' set only after a password, secret or token, and no URL.
fn generic(secret: &str, whole: &str, before: &str) -> bool {
    let only = |set: &[u8]| secret.bytes().all(|b| b.is_ascii_alphanumeric() || set.contains(&b));
    let symbolic = !only(b"_.=-") && !only(b"+/=");
    let name = &whole[..whole.rfind(secret).unwrap_or(0)];
    // a Rust macro call (`quote_spanned!`) and digits with format flags (`1234567890.-+`) are code,
    // as are a chain of names through `?.` or `!.`, Razor's `@Model.Password` and a `~/` path; a
    // plain dotted chain is not, since a passphrase has that shape
    let marked = secret.starts_with(['@', '~']) || secret.contains("?.") || secret.contains("!.");
    let code = secret.strip_suffix('!').is_some_and(|s| s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        || !secret.bytes().any(|b| b.is_ascii_alphabetic())
        || marked && CODE_OR_HOME.is_match(secret);
    (!symbolic || (names_a_secret(name) && !secret.contains("://") && !code))
        && secret.chars().any(|c| !(c.is_ascii_alphabetic() || matches!(c, '_' | '.' | '-')))
        && !NOT_SECRET_NAMES.is_match(whole)
        && real_value(secret, whole, before)
        && !NUMBER.is_match(secret)
        && !IDENTIFIER.is_match(secret)
        && !names_a_thing(secret)
}

/// The keywords after which generic-secret takes a value with a symbol; `passw` alone stands for
/// any other word it starts.
static SECRET_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)passw(?:or)?[dt]|passw|secret|token").unwrap());

/// Parts after a keyword that say the name is about the secret, not the secret itself.
const NOT_SECRET_PARTS: &str = "label header hint type name field file path dir cache url uri endpoint prompt policy length len count format pattern regex rule ttl expiry expires timeout mode kind id";

/// Letters that, run on from a keyword, make a different word: `tokenizer`, `secretary`, `passwordless`.
const WORD_ENDINGS: &str = "izer izers ize ized izes izing ization ary aries ariat ion ions ive less";

/// `name` holds a password, secret or token keyword that names a secret: any does, except one
/// that runs on into a different word (`tokenizer`, `passwordless`) or is followed by a part such
/// as `label` or `cache` (`password_label`, `token.cache.dir`, `passwordHint`, `tokentype`).
/// Fails closed: an unknown part is a secret (`secretkey`, `tokenvalue`).
fn names_a_secret(name: &str) -> bool {
    let b = name.as_bytes();
    SECRET_WORD.find_iter(name).any(|m| {
        let mut e = m.end();
        // a plural: `tokens`, `PASSWORDS`
        if b.get(e).is_some_and(|c| c.eq_ignore_ascii_case(&b's')) && !b.get(e + 1).is_some_and(u8::is_ascii_alphabetic) {
            e += 1;
        }
        let part = |from: usize| {
            let run = b[from..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
            // a camel-case part ends where the next capital starts
            let run = b[from + 1..from + run].iter().position(u8::is_ascii_uppercase).map_or(run, |i| i + 1);
            name[from..from + run].to_ascii_lowercase()
        };
        let not_secret = |p: String| NOT_SECRET_PARTS.split(' ').any(|w| w == p);
        match b.get(e) {
            Some(b'_' | b'.' | b'-') => !not_secret(part(e + 1)),
            // camel case: a capital after a lower-case keyword starts the next part
            Some(c) if c.is_ascii_uppercase() && b[e - 1].is_ascii_lowercase() => !not_secret(part(e)),
            // letters run on: a secret unless they make another word (`tokenizer`, `secretary`)
            // or are a part such as `type` (`tokentype`); `secretkey` and `tokenvalue` are secrets
            Some(c) if c.is_ascii_alphabetic() => {
                let p = part(e);
                !(WORD_ENDINGS.split(' ').any(|w| w == p) || not_secret(p))
            }
            _ => true,
        }
    })
}

/// Names of letters joined by `.`, `?.` or `!.`, with Razor's `@` before them: no digit or other
/// symbol, so a password shaped so is still taken for one. Or `~/` and a path of path characters
/// that a dotted first segment or a second segment shows is one (`~/.ssh/id_rsa`,
/// `~/keys2/prod9.pem`); `~/` and one segment (`~/Prod2024key`) may be a password.
static CODE_OR_HOME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:@?[A-Za-z_]+(?:[?!]?\.[A-Za-z_]+)+|~/(?:\.[\w.-]+(?:/[\w./-]*)?|[\w.-]+/[\w./-]+))$").unwrap());

/// Code a `:` or `,` follows: a Ruby instance variable's field (`@user.auth_token2`), a method
/// chain that ends in `!` or `?` (`Rails.application.credentials.dig!`), an image's digest
/// (`repo@sha256`).
static CODE_BEFORE_A_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:@[A-Za-z_]\w*\.\w|[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+[!?]$)|@sha(?:256|384|512)$").unwrap());

/// `generic` of a value with a symbol gitleaks' set leaves out, that is no code: no ternary (a
/// `?` before the `:` that follows it, `isAuth?user.token:null`) and no `CODE_BEFORE_A_SEPARATOR`.
fn generic_with_symbol(secret: &str, whole: &str, before: &str) -> bool {
    let at = whole.rfind(secret).map_or(whole.len(), |i| i + secret.len());
    let ternary = secret.contains('?') && whole[at..].starts_with(':');
    secret.bytes().any(|b| !(b.is_ascii_alphanumeric() || b"_.=+/-".contains(&b)))
        && !ternary
        && !CODE_BEFORE_A_SEPARATOR.is_match(secret)
        && generic(secret, whole, before)
}

/// A quoted value that names a thing: a file path with an extension (`config/secret_settings.toml`)
/// or words run together in camel case, a year or a count after them (`TokenRefreshed2024`).
static NOT_A_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[\w.-]*[/\\][\w./\\-]*\.([A-Za-z][A-Za-z0-9]{0,4})|(?:[A-Z][a-z]+){2,}[0-9]{0,4})$").unwrap());

/// The extensions a quoted path is taken to end in, compared without case.
const PATH_EXTENSIONS: &str = "bash bat bin c cc cer cfg cjs cmd conf cpp crt cs css csv dat db der dtb dts dtsi env fw go gz h hpp htm html img ini jar java jks js json jsx key kt lock log lua md mjs p12 pem pfx php pl png ps1 pub py rb rs scss sh so sql svg swift tar tgz toml ts tsx txt xml yaml yml zip";

/// Whether `secret` reads as the name of a thing (`NOT_A_PASSWORD`): a path only with a known
/// extension and no part that mixes both cases with digits, as a generated password does.
fn names_a_thing(secret: &str) -> bool {
    let mixed =
        |seg: &str| seg.chars().any(|c| c.is_ascii_uppercase()) && seg.chars().any(|c| c.is_ascii_lowercase()) && seg.chars().any(|c| c.is_ascii_digit());
    let Some(c) = NOT_A_PASSWORD.captures(secret) else { return false };
    match c.get(1) {
        Some(ext) => PATH_EXTENSIONS.split(' ').any(|e| ext.as_str().eq_ignore_ascii_case(e)) && !secret.split(['/', '\\', '.']).any(mixed),
        None => true,
    }
}

/// A name in one case joined by `_`, `-` or `.` (`NL80211_AUTHTYPE_SAE`, `keypad_row0`,
/// `x2apic_state`): code and device trees name things so; a random secret mixes cases.
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[A-Z][A-Z0-9]*(?:[_.-][A-Z0-9]+)+|[a-z][a-z0-9]*(?:[_.-][a-z0-9]+)+)$").unwrap());

/// A number as code writes it: hex, decimal or float, with a type suffix.
static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:0x[0-9a-f_]+|[0-9][0-9_]*(?:\.[0-9_]+)?(?:e[+-]?[0-9]+)?)(?:[ui](?:8|16|32|64|128|size)|f32|f64|[lu]{1,3})?$").unwrap()
});

/// A token: a real value shaped like one. It mixes upper case, lower case and digits, or runs
/// letters and digits through each other (a change between them three times or more: one case
/// with digits, as issuers write tokens), or is a UUID, or 32 or more hex digits; a word with a
/// number after it (`accesstoken12345678`, `authenticationv2scheme`) is none.
fn opaque(secret: &str, whole: &str, before: &str) -> bool {
    let has = |f: fn(&u8) -> bool| secret.as_bytes().iter().any(f);
    let mixed = has(u8::is_ascii_uppercase) && has(u8::is_ascii_lowercase) && has(u8::is_ascii_digit);
    let hex = secret.len() >= 32 && secret.bytes().all(|b| b.is_ascii_hexdigit()) && has(u8::is_ascii_digit);
    let changes = secret.as_bytes().windows(2).filter(|w| w[0].is_ascii_digit() != w[1].is_ascii_digit() && w.iter().all(u8::is_ascii_alphanumeric)).count();
    real_value(secret, whole, before) && (mixed || hex || UUID.is_match(secret) || (changes >= 3 && has(u8::is_ascii_alphabetic)))
}

static UUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$").unwrap());

/// Base64 that decodes to printable `user:password`, the password no doc's example word.
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
    let Some(i) = out.iter().position(|&b| b == b':').filter(|&i| i > 0 && i + 1 < out.len()) else { return false };
    let password = String::from_utf8_lossy(&out[i + 1..]).to_ascii_lowercase();
    out.iter().all(|b| (0x20..0x7f).contains(b)) && !PLACEHOLDER_PASSWORDS.contains(&password.as_str())
}

/// A URL's password: no variable or placeholder word, and a digit or a symbol in it unless it is
/// eight letters or more (`admin:admin@localhost` is a doc's example, a generated one is long).
fn url_password(secret: &str, whole: &str, before: &str) -> bool {
    real_value(secret, whole, before)
        && !NOT_SECRET_NAMES.is_match(whole)
        && (secret.len() >= 8 || secret.chars().any(|c| !c.is_ascii_alphabetic()))
        && !placeholder_credentials(whole, secret)
}

/// The words a doc writes for a password, which every rule leaves as written.
const PLACEHOLDER_PASSWORDS: [&str; 6] = ["password", "passwd", "pass", "pwd", "secret", "changeme"];

/// A doc's example `user:password`: the password the user name again (`anonymous:anonymous`), or
/// with the word password in it (`username:mypassword`).
fn placeholder_credentials(whole: &str, secret: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    let user = whole.rfind(&format!(":{secret}")).map(|i| &whole[..i]).map(|u| &u[u.rfind(['/', ' ', '\t', '=', '"', '\'']).map_or(0, |j| j + 1)..]);
    // the word password in letters alone (`mypassword`, `your_password_here`); with a digit or a
    // symbol beside it (`MyPassword2024`) it is a real, weak one
    let word = (lower.contains("password") || lower.contains("passwd")) && lower.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_');
    word || user.is_some_and(|u| u.eq_ignore_ascii_case(secret))
}

/// A `.env` value that is no placeholder and is not a name or a word alone.
fn env_value(secret: &str, whole: &str, before: &str) -> bool {
    real_value(secret, whole, before) && !NOT_SECRET_NAMES.is_match(whole) && secret.chars().any(|c| !c.is_ascii_alphabetic())
}

/// A `.env` secret: `env_value`'s test, and no render or build stage's setting.
fn env_secret_value(secret: &str, whole: &str, before: &str) -> bool {
    env_value(secret, whole, before) && !stage_setting(secret, whole)
}

/// A render or build stage's setting (`RENDER_PASS=shadowmap2`, `LINT_PASS=strict_mode`): a name
/// ending in `_PASS` with a stage's word among its parts, and a value made of that stage's setting
/// words alone. Both names can also be a login's (`RENDER` is a hosting platform, `CI_BUILD_PASS`
/// a CI credential), so a value with any other part keeps its redaction, as does any other name
/// ending in `PASS` (`DB_PASS`, `TEST_PASS`).
fn stage_setting(secret: &str, whole: &str) -> bool {
    const RENDER: &[&str] = &["RENDER", "SHADOW", "SHADER", "LIGHT", "LIGHTING", "DEPTH", "BLUR", "BLOOM", "GBUFFER", "OPAQUE", "TRANSPARENT"];
    const BUILD: &[&str] = &["LINT", "BUILD", "COMPILE", "CODEGEN", "OPT"];
    const SETTING_WORDS: &[&str] =
        &["strict", "mode", "enabled", "disabled", "on", "off", "true", "false", "fast", "full", "debug", "release", "none", "all", "default", "warn", "error"];
    const RENDER_WORDS: &[&str] = &[
        "shadow",
        "shadowmap",
        "deferred",
        "forward",
        "prepass",
        "low",
        "medium",
        "high",
        "ultra",
        "hdr",
        "ldr",
        "msaa",
        "fxaa",
        "taa",
        "ssao",
        "bloom",
        "depth",
        "opaque",
        "transparent",
        "gbuffer",
        "lighting",
        "cascaded",
        "linear",
        "gamma",
    ];
    let name = whole.split('=').next().unwrap_or("").trim_start_matches("export").trim();
    let Some(stem) = name.strip_suffix("_PASS") else { return false };
    let has = |words: &[&str]| stem.split('_').any(|part| words.contains(&part));
    let version = |w: &str| {
        let n = w.strip_prefix('v').unwrap_or(w);
        (1..=2).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
    };
    if has(BUILD) {
        return secret.split(['_', '-', '.']).all(|w| SETTING_WORDS.contains(&w) || version(w));
    }
    // a render word may carry its version glued on (`shadowmap2`)
    let render_word = |w: &str| {
        let word = w.trim_end_matches(|c: char| c.is_ascii_digit());
        (w.len() - word.len() <= 2) && (RENDER_WORDS.contains(&word) || SETTING_WORDS.contains(&word))
    };
    has(RENDER) && secret.split(['_', '-', '.']).all(|w| render_word(w) || version(w))
}

/// An XML credential: `quoted`'s test, and no variable a deploy fills in.
fn xml_value(secret: &str, whole: &str, before: &str) -> bool {
    quoted(secret, whole, before) && !DEPLOY_PLACEHOLDER.is_match(secret)
}

/// A variable a deploy fills in, closed and of names alone: Octopus `#{db.password}`, token
/// replacement `@@DB_PASSWORD@@`, `[ClientSecret]`, Razor's `@Model.Password`.
static DEPLOY_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:#\{[\w.:-]+\}|@@[\w.-]+@@|\[[A-Za-z_][A-Za-z_.:-]*\]|@(?:Model|ViewBag|ViewData|Html|Configuration)(?:\.[A-Za-z_]+)+)$").unwrap()
});

/// A quoted secret: no placeholder, URL, number or one-case name, and a digit or a symbol in it.
fn quoted(secret: &str, whole: &str, before: &str) -> bool {
    secret.len() <= 150
        && !names_a_thing(secret)
        && env_value(secret, whole, before)
        && !secret.contains("://")
        && !NUMBER.is_match(secret)
        && !IDENTIFIER.is_match(secret)
}

/// `xml_value` of an element's whole text, but not a sentence (`Forgot your password?`, `Copy to
/// clipboard`: a space, no digit and no symbol but punctuation) or an escaped `&lt;...&gt;`.
fn element_text(secret: &str, whole: &str, before: &str) -> bool {
    let prose = secret.contains(' ') && secret.chars().all(|c| c.is_alphabetic() || c.is_whitespace() || ".,!?;:'\"()-".contains(c));
    !prose && !secret.trim_start().starts_with("&lt;") && xml_value(secret, whole, before)
}

/// Base64 that is no checksum a lockfile writes (`h1:` in go.sum, `sha512-` in package-lock),
/// after a space, a colon or a quote, or first in its line's text (grep's `path-7-` ends in `-`).
fn alone_on_its_line(_: &str, _: &str, before: &str) -> bool {
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let text = split_line(line).1;
    !HASH_PREFIX.is_match(before) && !SSH_PUBLIC.is_match(before) && (text.is_empty() || text.ends_with([' ', '\t', ':', '"', '\'']))
}

/// An SSH public key's type, as `known_hosts` and `authorized_keys` write it before the key.
static SSH_PUBLIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:ssh-(?:rsa|dss|ed25519|ed448)|ecdsa-sha2-[A-Za-z0-9-]+|sk-(?:ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)[ \t]+$").unwrap()
});

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
/// The armour of material that is public: a public key, a certificate, a certificate request.
static PUBLIC_ARMOUR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)-----(BEGIN|END)([ A-Z0-9_-]{0,100}(?:PUBLIC KEY|CERTIFICATE(?: REQUEST)?|X509 CRL))-----").unwrap());
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

/// The most lines and bytes of a public key's or a certificate's body held for its END: past
/// either, it is read as any base64 (a 4096-bit certificate's body is about 30 lines).
const PUBLIC_LINES: usize = 64;
const B64_DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PUBLIC_BYTES: usize = 8 << 10;

/// What `Redactor::hold` says to do with a line of a stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Hold {
    /// keep it back with the lines held before it
    Line,
    /// the lines held go out first, as a public body's or as any lines, then this one as usual
    Release { public: bool },
}

/// The body of a public key or a certificate being read: only armour that ends its line (or is
/// followed by a written-out `\n`, a key in one string) and is not closed on it opens one; base64
/// alone, blank and header lines are its body. An END of the same label shows it was one, when the
/// body is then exactly the one DER element its first bytes say; anything else ends it, and a body
/// that is not such an element is marked, whatever its label (an OpenPGP or an RFC 4716 armour is
/// no public armour here, so their bodies are marked as any key material).
#[derive(Default)]
struct PublicBody {
    open: bool,
    /// the BEGIN's label (`PUBLIC KEY`, `CERTIFICATE`)
    label: String,
    /// its base64 lines so far: (text, start, end)
    held: Vec<(usize, usize, usize)>,
    lines: usize,
    bytes: usize,
    /// the body's base64 characters so far, padding left out, and the first eight of them
    chars: usize,
    head: String,
    /// all of it, padding kept: what a public key label's body is decoded from at its END
    b64: String,
    /// the lines of a body that ended without being let out (`Seen::Key`), for the caller
    key: Vec<(usize, usize, usize)>,
    /// such a body ended inside its base64: the base64 lines after it are its own (`Seen::KeyLine`)
    marking: bool,
}

/// What a public body's first bytes say of it.
enum Der {
    /// too few have come to tell
    TooFew,
    /// they begin no DER element of a length this reads
    Not,
    /// the length of the element they begin, its header included
    Len(usize),
}

enum Seen {
    Held,
    /// the END: the lines held are a public body
    Released,
    /// the body ended without being let out (the END of its label after an element of no public
    /// shape, another END, a cut, more than its element, over the hold): the lines it held,
    /// in `PublicBody::key`, are key material, a private key's body under a public label
    Key,
    /// a base64 line that runs on after such a body: key material too
    KeyLine,
    Other,
}

impl PublicBody {
    fn line(&mut self, ti: usize, at: usize, line: &str) -> Seen {
        if self.marking {
            if !PUBLIC_ARMOUR.is_match(line) && matches!(key_body(line), Body::Base64(..)) {
                return Seen::KeyLine;
            }
            self.marking = false;
        }
        if self.open {
            if let Some(c) = PUBLIC_ARMOUR.captures(line).filter(|c| &c[1] == "END") {
                self.open = false;
                if c[2].trim().eq_ignore_ascii_case(&self.label) && matches!(self.der(), Der::Len(n) if n == self.decoded()) && self.public_shape() {
                    return Seen::Released;
                }
                self.key = std::mem::take(&mut self.held);
                return Seen::Key;
            }
            let body = key_body(line);
            if !matches!(body, Body::NotKey) && self.lines < PUBLIC_LINES && self.bytes + line.len() <= PUBLIC_BYTES {
                self.lines += 1;
                self.bytes += line.len();
                if let Body::Base64(a, b) = body {
                    let b64 = &line[a..base64_run(line, b)];
                    let take = (8 - self.head.len()).min(b64.len());
                    self.head.push_str(&b64[..take]);
                    self.b64.push_str(b64);
                    self.chars += b64.bytes().filter(|&c| c != b'=').count();
                    self.held.push((ti, at, at + line.len()));
                }
                // not a DER element, or more than the one it began: another body runs on after it
                match self.der() {
                    Der::TooFew => return Seen::Held,
                    Der::Len(n) if self.decoded() <= n => return Seen::Held,
                    _ => {}
                }
            } else if matches!(body, Body::Base64(..)) {
                // over the hold: this line is the body's too
                self.held.push((ti, at, at + line.len()));
            }
            self.open = false;
            let key = std::mem::take(&mut self.held);
            self.opens(line);
            self.key = key;
            self.marking = !self.open && matches!(body, Body::Base64(..));
            return Seen::Key;
        }
        self.opens(line);
        Seen::Other
    }

    fn opens(&mut self, line: &str) {
        for c in PUBLIC_ARMOUR.captures_iter(line).filter(|c| &c[1] == "BEGIN") {
            let m = c.get(0).unwrap();
            let rest = line[m.end()..].trim_end_matches(['\n', '\r']);
            let closed = PUBLIC_ARMOUR.captures_iter(&line[m.end()..]).any(|e| &e[1] == "END");
            if (rest.trim().is_empty() || rest.starts_with("\\n")) && !closed {
                *self = PublicBody { open: true, label: c[2].trim().to_string(), ..PublicBody::default() };
            }
        }
    }

    /// The bytes the base64 held so far decodes to.
    fn decoded(&self) -> usize {
        self.chars * 3 / 4
    }

    /// What the body's first bytes say of the DER element it starts with (a SEQUENCE, as every
    /// public key, certificate, request and list of revoked ones is).
    fn der(&self) -> Der {
        let value = |c: u8| B64_DIGITS.iter().position(|&d| d == c);
        let mut bytes = Vec::new();
        for quad in self.head.as_bytes().chunks(4).filter(|q| q.len() == 4) {
            let Some(v) = quad.iter().try_fold(0u32, |acc, &c| value(c).map(|d| acc << 6 | d as u32)) else { return Der::Not };
            bytes.extend([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        match bytes[..] {
            [0x30, n, ..] if n < 0x80 => Der::Len(2 + n as usize),
            [0x30, 0x81, a, ..] => Der::Len(3 + a as usize),
            [0x30, 0x82, a, b, ..] => Der::Len(4 + ((a as usize) << 8 | b as usize)),
            [0x30, 0x83, a, b, c, ..] => Der::Len(5 + ((a as usize) << 16 | (b as usize) << 8 | c as usize)),
            // what has come so far could still begin one
            [] | [0x30, 0x82 | 0x83, _] if self.head.len() < 8 => Der::TooFew,
            _ => Der::Not,
        }
    }

    /// Whether the body is what its label says, and nothing else in it. Under a public key's label a
    /// SubjectPublicKeyInfo (a SEQUENCE and a BIT STRING) or a PKCS#1 RSAPublicKey (two INTEGERs);
    /// under a certificate's, a request's or a list of revoked ones', a SEQUENCE, a SEQUENCE and a
    /// BIT STRING. A private key starts with its version, an INTEGER, then more, so it is none.
    fn public_shape(&self) -> bool {
        let Some(der) = base64_decode(&self.b64) else { return false };
        let Some((0x30, content)) = der_element(&der).filter(|(_, c)| c.len() + der_header(&der) == der.len()) else { return false };
        let mut tags = Vec::new();
        let mut rest = content;
        while !rest.is_empty() {
            let Some((tag, inner)) = der_element(rest) else { return false };
            tags.push(tag);
            rest = &rest[der_header(rest) + inner.len()..];
        }
        if self.label.to_ascii_uppercase().ends_with("PUBLIC KEY") { matches!(tags[..], [0x30, 0x03] | [0x02, 0x02]) } else { tags[..] == [0x30, 0x30, 0x03] }
    }
}

/// Where the base64 that `key_body` found ending at `end` really ends: a PEM line's own `+`, which
/// `key_core` takes off as the joint of a key in code, is part of it.
fn base64_run(line: &str, end: usize) -> usize {
    end + line[end..].bytes().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=')).count()
}

/// The bytes of standard base64, `=` padding at its end; None for any other character.
fn base64_decode(b64: &str) -> Option<Vec<u8>> {
    let value = |c: u8| B64_DIGITS.iter().position(|&d| d == c).map(|d| d as u32);
    let body = b64.trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.as_bytes().chunks(4) {
        let v = chunk.iter().try_fold(0u32, |acc, &c| value(c).map(|d| acc << 6 | d))? << (6 * (4 - chunk.len()));
        out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8][..chunk.len().saturating_sub(1)]);
    }
    Some(out)
}

/// The length of the tag and length bytes of the DER element `der` starts with (0 if unread).
fn der_header(der: &[u8]) -> usize {
    match der.get(1) {
        Some(&n) if n < 0x80 => 2,
        Some(&n @ 0x81..=0x83) => 2 + (n - 0x80) as usize,
        _ => 0,
    }
}

/// The tag and content of the DER element `der` starts with, when its length fits in `der`.
fn der_element(der: &[u8]) -> Option<(u8, &[u8])> {
    let head = der_header(der);
    let len = match head {
        0 => return None,
        2 => der[1] as usize,
        _ => der.get(2..head)?.iter().fold(0usize, |a, &b| a << 8 | b as usize),
    };
    Some((der[0], der.get(head..head + len)?))
}

/// `text` as the rules read it: JSON may write `/` as `\/` (PHP does).
fn seen(text: &str) -> Cow<'_, str> {
    if text.contains("\\/") { Cow::Owned(unslash(text).0) } else { Cow::Borrowed(text) }
}

/// For each of a call's texts, byte ranges of it.
type LineRanges = Vec<Vec<(usize, usize)>>;

/// For each of `texts`, the byte ranges (as the rules read it) of the lines of a public key's or a
/// certificate's body whose END comes later in `texts`: a body cut off before it is no public one.
/// And of the lines of a body under a public key's label that is no public key (`Seen::Key`).
fn public_in(texts: &[&str]) -> (LineRanges, LineRanges) {
    let mut body = PublicBody::default();
    let (mut public, mut key) = (vec![Vec::new(); texts.len()], vec![Vec::new(); texts.len()]);
    for (ti, text) in texts.iter().enumerate() {
        // a text with no armour can still hold a body's lines, or the key lines that run on after one
        if !body.open && !body.marking && !text.contains("-----") {
            continue;
        }
        let mut at = 0;
        for line in seen(text).split_inclusive('\n') {
            match body.line(ti, at, line) {
                Seen::Released => body.held.drain(..).for_each(|(t, a, b)| public[t].push((a, b))),
                Seen::Key => body.key.drain(..).for_each(|(t, a, b)| key[t].push((a, b))),
                Seen::KeyLine => key[ti].push((at, at + line.len())),
                _ => {}
            }
            at += line.len();
        }
    }
    // a body its END never came for
    if body.open {
        body.held.drain(..).for_each(|(t, a, b)| key[t].push((a, b)));
    }
    (public, key)
}

/// Redacts texts in order, a private key's body carried from one text into the next (the lines of
/// a file as an answer's items).
#[derive(Default)]
pub struct Redactor {
    in_key: bool,
    /// the public body a stream of lines (`hold`) is inside
    stream: PublicBody,
    /// the lines (as the rules read the text being redacted) of a body under a public key's label
    /// that is no public key, marked whole as key material
    key_lines: Vec<(usize, usize)>,
    /// how many such lines a stream released and has still to mark
    stream_key_lines: usize,
    /// the redactions the page `ops::Answer::text_page` last made shows, in either form
    pub shown: usize,
    /// redactions made, by kind
    pub counts: std::collections::BTreeMap<&'static str, usize>,
}

impl Redactor {
    /// `text` with its secrets replaced, and where each marker is.
    pub fn redact<'a>(&mut self, text: &'a str) -> (Cow<'a, str>, Vec<Mark>) {
        let (mut public, mut key) = public_in(&[text]);
        self.key_lines = key.remove(0);
        self.redact_with(text, &public.remove(0))
    }

    /// `text` redacted as a line of a public key's or a certificate's body, which `hold` released.
    pub fn redact_public<'a>(&mut self, text: &'a str) -> (Cow<'a, str>, Vec<Mark>) {
        self.redact_with(text, &[(0, usize::MAX)])
    }

    /// For a stream of lines, read one at a time (`hook run`): whether `line` is to be held back
    /// with the lines held before it, as the body of a public key or a certificate whose END has
    /// not come yet; else whether those go out first as that (`redact_public`) or as any line.
    pub fn hold(&mut self, line: &str) -> Hold {
        match self.stream.line(0, 0, &seen(line)) {
            Seen::Held => Hold::Line,
            Seen::Released => Hold::Release { public: true },
            Seen::Key => {
                self.stream_key_lines += std::mem::take(&mut self.stream.key).len();
                Hold::Release { public: false }
            }
            Seen::KeyLine => {
                self.stream_key_lines += 1;
                Hold::Release { public: false }
            }
            Seen::Other => Hold::Release { public: false },
        }
    }

    /// The stream `hold` reads has ended, the lines it holds about to go out: a body whose END never
    /// came is key material.
    pub fn end_stream(&mut self) {
        if self.stream.open {
            self.stream_key_lines += self.stream.held.len();
        }
        self.stream = PublicBody::default();
    }

    /// `redact`, with the byte ranges of `text` (as the rules read it) that are a public body.
    fn redact_with<'a>(&mut self, text: &'a str, public: &[(usize, usize)]) -> (Cow<'a, str>, Vec<Mark>) {
        let spans = self.find(text, public);
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
            marks.push(Mark { kind, start, end: out.len(), secret: (a, b) });
            *self.counts.entry(kind).or_default() += 1;
            at = b;
        }
        out.push_str(&text[at..]);
        (Cow::Owned(out), marks)
    }

    /// The secrets in `text`, as byte ranges of it with their kinds, in order, none inside another.
    fn find(&mut self, text: &str, public: &[(usize, usize)]) -> Vec<(usize, usize, &'static str)> {
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
        for (r, _) in RULES.iter().zip(&wanted).filter(|&(_, &w)| w) {
            for c in r.regex().captures_iter(seen) {
                let Some(g) = c.get(r.group) else { continue };
                if c.name("ann").is_some_and(|a| a.len() > TYPED_MAX) {
                    continue;
                }
                let end = if r.pattern.ends_with(CODE_END) || r.pattern.ends_with(VALUE_END) { value_end(seen, g.end()) } else { g.end() };
                // a mark already written is no secret: the assembled page is read a second time. The
                // value a `:` or `,` cuts (`Xq7r@T2vW9:yZk3`) may pass the entropy test whole, when
                // its part mixes both cases and digits as a password does and an address does not
                let random = |v: &str| r.entropy <= 0.0 || entropy(v) > r.entropy;
                let has = |f: fn(&u8) -> bool| g.as_str().bytes().any(|b| f(&b));
                let cut = r.pattern.ends_with(VALUE_END) && has(u8::is_ascii_uppercase) && has(u8::is_ascii_lowercase) && has(u8::is_ascii_digit);
                if g.as_str().contains("[redacted:")
                    || !(random(g.as_str()) || cut && random(&seen[g.start()..end]))
                    || !(r.keep)(g.as_str(), &c[0], &seen[..g.start()])
                {
                    continue;
                }
                spans.push((g.start(), end, r.kind));
            }
        }
        // base64 alone on a line is a key's body only outside a public key or a certificate
        spans.retain(|&(a, _, k)| k != "key-material" || !public.iter().any(|&(s, e)| s <= a && a < e));
        // a body under a public key's label that is no public key: every line of it, a short last
        // one too, from the text's ranges or, in a stream, the lines released one by one
        let mut key_lines = std::mem::take(&mut self.key_lines);
        if self.stream_key_lines > 0 && matches!(key_body(seen), Body::Base64(..)) {
            self.stream_key_lines -= 1;
            key_lines.push((0, seen.len()));
        }
        for (a, b) in key_lines {
            let line = &seen[a.min(seen.len())..b.min(seen.len())];
            if let Body::Base64(x, y) = key_body(line) {
                spans.push((a + x, a + base64_run(line, y), "key-material"));
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
        let (public, key) = public_in(&texts[first..].iter().map(String::as_str).collect::<Vec<_>>());
        let mut marks = vec![0; texts.len()];
        let mut used = 0;
        for (((t, n), public), key) in texts[first..].iter_mut().zip(&mut marks[first..]).zip(&public).zip(key) {
            if used > room {
                break;
            }
            self.key_lines = key;
            let (redacted, m) = self.redact_with(t, public);
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
    // a detector of two rules (xml-secret) is one
    out.dedup();
    out.push(("private-key", "gitleaks private-key, secretlint-rule-privatekey (line by line between the armour)"));
    out
}

/// The secrets in `text` (one line or more), as byte ranges of it with their kinds, in order.
pub fn spans(text: &str) -> Vec<(usize, usize, &'static str)> {
    let (mut public, mut key) = public_in(&[text]);
    let mut r = Redactor { key_lines: key.remove(0), ..Redactor::default() };
    r.find(text, &public.remove(0))
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

/// The file at `path` when it is a regular file: a device or a FIFO a printed line names is never
/// opened (`/dev/zero` reads forever, a FIFO blocks in open).
pub(crate) fn open_regular(path: &str) -> Option<std::fs::File> {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    opened_regular(path)
}

/// `path` opened without waiting, kept only when the handle is a regular file: a FIFO swapped in
/// after the path was checked does not block the open. No `O_NOFOLLOW`: a key file reached
/// through a link must still be read, or its body lines would go out unredacted.
fn opened_regular(path: &str) -> Option<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    o.open(path).ok().filter(|f| f.metadata().is_ok_and(|m| m.is_file()))
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

    #[cfg(unix)]
    #[test]
    fn a_fifo_swapped_in_after_the_path_was_checked_does_not_block_the_open() {
        let dir = std::env::temp_dir().join(format!("scrutate_fifo_open_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("key.pem");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let path = fifo.to_str().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || tx.send(opened_regular(&p).is_some()).unwrap());
        let got = rx.recv_timeout(std::time::Duration::from_secs(5));
        if got.is_err() {
            // a writer end lets a blocked open return, so no thread is left behind
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new().write(true).custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32).open(&fifo);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Ok(false), "the open waited for a writer");
    }

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
            line("huggingface-token", "HF_TOKEN = \"", format!("{}{}", "hf_", fake(80, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ", 34)), "\""),
            line("shopify-token", "SHOPIFY = \"", format!("{}{}", "shpat_", fake(81, "0123456789abcdef", 32)), "\""),
            line("digitalocean-token", "DO = \"", format!("{}{}", "dop_v1_", fake(82, "0123456789abcdef", 64)), "\""),
            line("vault-token", "VAULT_TOKEN = \"", format!("{}{}", "hvs.", fake(83, &format!("{ALNUM}_-"), 100)), "\""),
            line("telegram-bot-token", "telegram_bot = \"", format!("{}:A{}", fake(84, "0123456789", 10), fake(85, &format!("{ALNUM}_-"), 34)), "\""),
            line("url-token", "url = \"https://", fake(66, HEX, 40), "@github.com/org/app.git\""),
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

    #[test]
    fn a_token_glued_to_word_characters_is_redacted_where_gitleaks_finds_it() {
        // gitleaks' rules for these kinds have no word boundary before the token (8.30.1)
        let glued = ["github-token", "github-fine-grained-token", "gitlab-token", "slack-token", "pypi-token", "twilio-api-key"];
        let mut seen = 0;
        for (kind, _, secret) in samples().into_iter().filter(|(k, ..)| glued.contains(&&k[..])) {
            seen += 1;
            for line in [format!("x = q{secret} ;"), format!("x = {secret}q ;"), format!("x = q{secret}q ;")] {
                let (out, _) = redact(&line);
                assert!(!out.contains(&secret), "{kind}: {out}");
            }
        }
        assert_eq!(seen, glued.len());
    }

    /// The base64 lines, 64 to a line, of a SubjectPublicKeyInfo of `n` bytes in all (at least 28),
    /// its key bits from `seed`: a body read as one public key.
    fn der_lines(seed: u64, n: usize) -> Vec<String> {
        pem_lines(&spki(seed, n))
    }

    /// `n` bytes over the whole range, the same for a seed every run.
    fn random_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect()
    }

    /// A SubjectPublicKeyInfo of `n` bytes in all (at least 28): an RSA algorithm and a BIT STRING
    /// of `seed`'s bytes, each length in the long form, as `der_lines` has always measured it.
    fn spki(seed: u64, n: usize) -> Vec<u8> {
        let alg = tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]), vec![0x05, 0x00]].concat());
        let bits = n - 4 - alg.len() - 4;
        let mut bit_string = vec![0x03, 0x82, (bits >> 8) as u8, bits as u8, 0x00];
        bit_string.extend(random_bytes(seed, bits - 1));
        let content = [alg, bit_string].concat();
        [vec![0x30, 0x82, (content.len() >> 8) as u8, content.len() as u8], content].concat()
    }

    /// `der` in base64, 64 characters to a line, as PEM writes a body.
    fn pem_lines(der: &[u8]) -> Vec<String> {
        const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for c in der.chunks(3) {
            let v = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
            (0..4).for_each(|i| b64.push(if i <= c.len() { SET[(v >> (18 - 6 * i) & 63) as usize] as char } else { '=' }));
        }
        b64.as_bytes().chunks(64).map(|l| String::from_utf8(l.to_vec()).unwrap()).collect()
    }

    /// One DER element: its tag, its length in the short or long form, its content.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let n = content.len();
        let mut v = vec![tag];
        match n {
            0..0x80 => v.push(n as u8),
            0x80..0x100 => v.extend([0x81, n as u8]),
            _ => v.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        v.extend(content);
        v
    }

    /// The DER of an RSA key's shapes, built here from seeded bytes so no file holds a key:
    /// SubjectPublicKeyInfo, PKCS#1 RSAPublicKey, PKCS#1 RSAPrivateKey and PKCS#8 PrivateKeyInfo.
    fn rsa_shapes(seed: u64) -> [(&'static str, Vec<u8>); 4] {
        let big = |s: u64, n: usize| [&[0u8][..], &random_bytes(s, n)].concat();
        let int = |s: u64, n: usize| tlv(0x02, &big(s, n));
        let rsa_alg = tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]), vec![0x05, 0x00]].concat());
        let rsa_public = tlv(0x30, &[int(seed, 256), tlv(0x02, &[0x01, 0x00, 0x01])].concat());
        let spki = tlv(0x30, &[rsa_alg.clone(), tlv(0x03, &[&[0u8][..], &rsa_public].concat())].concat());
        let version = tlv(0x02, &[0x00]);
        let mut private = version.clone();
        for (i, n) in [256, 3, 256, 128, 128, 128, 128, 128].into_iter().enumerate() {
            private.extend(int(seed + 1 + i as u64, n));
        }
        let pkcs1 = tlv(0x30, &private);
        let pkcs8 = tlv(0x30, &[version, rsa_alg, tlv(0x04, &pkcs1)].concat());
        [("SubjectPublicKeyInfo", spki), ("RSAPublicKey", rsa_public), ("RSAPrivateKey", pkcs1), ("PrivateKeyInfo", pkcs8)]
    }

    /// Under a public key's armour, with its END, a public key's body is left as written and a
    /// private key's body is marked: a SEQUENCE of the right length is not proof enough.
    #[test]
    fn a_private_key_body_under_a_public_key_label_is_redacted() {
        let armoured =
            |label: &str, body: &[String]| [vec![format!("-----BEGIN {label}-----")], body.to_vec(), vec![format!("-----END {label}-----")]].concat();
        for (what, der) in rsa_shapes(151) {
            let public = !what.contains("Private");
            for label in ["PUBLIC KEY", "RSA PUBLIC KEY", "CERTIFICATE"] {
                let body = pem_lines(&der);
                // a public key is shown under a public key's label; under a certificate's it is none
                let shown = if public && label.ends_with("PUBLIC KEY") { body.len() } else { 0 };
                assert_eq!(body_lines_shown(&armoured(label, &body)), [shown; 3], "{what} under {label}");
            }
        }
        // a certificate, a request and a list of revoked ones are shown under their labels
        for label in ["CERTIFICATE", "CERTIFICATE REQUEST", "X509 CRL"] {
            let body = pem_lines(&certificate(161, 700));
            assert_eq!(body_lines_shown(&armoured(label, &body)), [body.len(); 3], "a certificate's shape under {label}");
        }
        // a private key whose body has a line that ends in `+`, and a short last line
        let plus = (200..400)
            .map(|seed| pem_lines(&rsa_shapes(seed)[3].1))
            .find(|b| b[..b.len() - 1].iter().any(|l| l.ends_with('+')) && b.last().is_some_and(|l| l.len() < 40))
            .unwrap();
        assert_eq!(body_lines_shown(&armoured("PUBLIC KEY", &plus)), [0; 3], "a body line that ends in `+`");
        // a body under a public label that ends without being let out: every line of it marked
        let pkcs8 = pem_lines(&rsa_shapes(171)[3].1);
        let no_end = [vec!["-----BEGIN PUBLIC KEY-----".to_string()], pkcs8.clone()].concat();
        let padded = armoured("PUBLIC KEY", &[pkcs8.clone(), vec!["QUJDRA==".to_string()]].concat());
        let cut = armoured("PUBLIC KEY", &[&pkcs8[..3], &pkcs8[4..]].concat());
        let other_end = [vec!["-----BEGIN PUBLIC KEY-----".to_string()], pkcs8.clone(), vec!["-----END CERTIFICATE-----".to_string()]].concat();
        let crl_plus = armoured("X509 CRL", &plus);
        let over_hold = armoured("PUBLIC KEY", &der_lines(181, 85 * 48));
        for (what, lines) in
            [("no END", no_end), ("padded", padded), ("cut", cut), ("another label's END", other_end), ("a CRL", crl_plus), ("over the hold", over_hold)]
        {
            assert_eq!(body_lines_shown(&lines), [0; 3], "{what}");
        }
        // over the hold with a short last line, also as `read_code` gives a file: a line to an item,
        // its number before it
        let over_short = armoured("PUBLIC KEY", &der_lines(182, 85 * 48 + 10));
        assert!(over_short[over_short.len() - 2].len() < 40);
        assert_eq!(body_lines_shown(&over_short), [0; 3], "over the hold, a short last line");
        let mut numbered: Vec<String> = over_short.iter().enumerate().map(|(i, l)| format!("{:>5}  {l}", i + 1)).collect();
        Redactor::default().redact_shown(&mut numbered, 0, usize::MAX);
        let shown: Vec<&String> = numbered.iter().filter(|l| !l.contains("-----") && !l.contains("[redacted:")).collect();
        assert!(shown.is_empty(), "read_code's lines of a body over the hold shown: {shown:?}");
    }

    /// A certificate's shape of `n` bytes or so: a SEQUENCE of a SEQUENCE, a SEQUENCE and a BIT
    /// STRING, as a certificate, a request and a list of revoked ones are.
    fn certificate(seed: u64, n: usize) -> Vec<u8> {
        let alg = tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]), vec![0x05, 0x00]].concat());
        let signed = tlv(0x30, &random_bytes(seed, n));
        let signature = tlv(0x03, &[&[0u8][..], &random_bytes(seed + 1, 256)].concat());
        tlv(0x30, &[signed, alg, signature].concat())
    }

    /// How many lines of `lines` that are no armour are shown, read as one text, as an answer's
    /// lines (`redact_shown`) and as a stream that `hook run` reads with `hold`, the lines held at
    /// the end let out as it does.
    fn body_lines_shown(lines: &[String]) -> [usize; 3] {
        let shown = |out: &[String]| out.iter().zip(lines).filter(|(o, l)| !l.starts_with("-----") && !o.contains("[redacted:")).count();
        let one_text: Vec<String> = redact(&lines.join("\n")).0.split('\n').map(str::to_string).collect();
        let mut answer = lines.to_vec();
        Redactor::default().redact_shown(&mut answer, 0, usize::MAX);
        let (mut r, mut held, mut streamed) = (Redactor::default(), Vec::new(), Vec::new());
        for l in lines {
            match r.hold(l) {
                Hold::Line => held.push(l.clone()),
                Hold::Release { public } => {
                    for h in held.drain(..) {
                        streamed.push(if public { r.redact_public(&h).0.into_owned() } else { r.redact(&h).0.into_owned() });
                    }
                    streamed.push(r.redact(l).0.into_owned());
                }
            }
        }
        r.end_stream();
        streamed.extend(held.iter().map(|h| r.redact(h).0.into_owned()));
        [shown(&one_text), shown(&answer), shown(&streamed)]
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
            "let mut key = 0xcbf29ce484222325u64;", // gitleaks:allow (FNV-1a's offset basis)
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
            // a word with a digit, a doc's example credentials, placeholders
            "Authorization: Bearer accesstoken12345678",
            "-H 'Authorization: Bearer authenticationv2scheme'",
            "Authorization: Basic dXNlcm5hbWU6cGFzc3dvcmQ=",
            "Authorization: Basic dXNlcjpwYXNzd29yZA==",
            "Authorization: Basic <base64 of username:password>",
            // quoted values that name a thing
            "secret_file = PathBuf::from(\"config/secret_settings.toml\")",
            "token_msg = Ok(\"TokenRefreshed2024\")",
            "secret_manager_client_name = \"SecretsManagerClient2024\"",
            "TOKEN_SCOPES = Set.of(\"repo:read\", \"repo:write\")",
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

    /// Where a rule's threshold differs from gitleaks' it marks more, never less: an AWS key id of
    /// low entropy (gitleaks wants 3) and an npm token whose prefix is upper case (gitleaks takes
    /// it in any case) are both redacted.
    #[test]
    fn a_rule_that_differs_from_gitleaks_marks_more() {
        let aws = format!("AKIA{}", "A".repeat(16));
        assert_eq!(redact(&format!("key = {aws}")).1.iter().map(|m| m.kind).collect::<Vec<_>>(), ["aws-access-key-id"], "{aws}");
        for prefix in ["npm_", "NPM_", "Npm_"] {
            let token = format!("{prefix}{}", fake(51, ALNUM, 36));
            let line = format!("//registry.npmjs.org/:_authToken={token}");
            let (out, marks) = redact(&line);
            assert!(marks.iter().any(|m| m.kind == "npm-token") && !out.contains(&token), "{line}: {out}");
        }
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
            // a path type: the annotation takes `::`
            format!("let api_secret: std::string::String = String::from(\"{v}\");"),
            format!("static SECRET_TOKEN: std::sync::LazyLock<std::string::String> = \"{v}\";"),
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

    /// A public key or a certificate is no secret: its body stays as written, while a private
    /// key after it is still redacted.
    #[test]
    fn public_keys_and_certificates_are_not_redacted() {
        let b64 = format!("{ALNUM}+/");
        let host = format!("github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{}", fake(70, &b64, 43));
        assert!(redact(&host).1.is_empty(), "{host}");
        let mut r = Redactor::default();
        for what in ["PUBLIC KEY", "RSA PUBLIC KEY", "CERTIFICATE"] {
            let body = if what == "CERTIFICATE" { pem_lines(&certificate(71, 40)) } else { der_lines(71, 110) };
            let mut lines = [vec![format!("-----BEGIN {what}-----")], body, vec![format!("-----END {what}-----")]].concat();
            // whole: as the lines of one answer, and as one text
            assert!(r.redact(&lines.join("\n")).1.is_empty(), "{what}");
            assert_eq!(r.redact_shown(&mut lines, 0, usize::MAX).iter().sum::<usize>(), 0, "{what}");
        }
        let body = fake(74, &b64, 64);
        let out: Vec<String> = [armour("BEGIN", ""), body.clone(), armour("END", "")].iter().map(|l| r.redact(l).0.into_owned()).collect();
        assert_eq!(out[1], "[redacted:private-key]");
        // a line of base64 alone outside any armour is still taken for key material
        assert!(redact(&fake(75, &b64, 64)).1.iter().any(|m| m.kind == "key-material"));
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

    /// A token after `Bearer` or an `Authorization: Token` scheme, or as a URL's user, is redacted
    /// in any of the shapes issuers give one: lower case or upper case with digits through it, a
    /// UUID, hex; a word with a number after it and a placeholder are not.
    #[test]
    fn a_token_of_one_case_or_a_uuid_is_redacted() {
        let ten = |seed: u64| fake(seed, "abcdefghijklmnopqrstuvwxyz0123456789", 40);
        let upper = fake(77, "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", 40);
        let uuid = format!("{}-{}-{}-{}-{}", fake(78, HEX, 8), fake(79, HEX, 4), fake(80, HEX, 4), fake(81, HEX, 4), fake(82, HEX, 12));
        let (lower, drf, user) = (ten(83), ten(84), ten(85));
        let hex = fake(86, HEX, 40);
        for (line, secret) in [
            (format!("curl -H \"Authorization: Bearer {lower}\" https://api.example.com"), &lower),
            (format!("Authorization: Bearer {upper}"), &upper),
            (format!("curl -H \"Authorization: Bearer {uuid}\" https://api.heroku.com/apps"), &uuid),
            (format!("curl -H \"Authorization: Token {hex}\" https://api.example.com/"), &hex),
            (format!("headers = {{'Authorization': 'Token {drf}'}}"), &drf),
            (format!("git clone https://{user}@github.com/org/app.git"), &user),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty(), "{line} -> {out}");
        }
        for line in [
            "Authorization: Bearer accesstoken12345678",
            "Authorization: Bearer YOUR_ACCESS_TOKEN_HERE",
            "Authorization: Token tokenauthentication2024",
            "Set Authorization to Token based auth for the service.",
            "git clone https://build_user_2024_ci_robot@github.com/org/app.git",
        ] {
            assert!(redact(line).1.is_empty(), "{line}");
        }
    }

    /// A URL's password is redacted when it is letters alone or the URL names no user; a variable,
    /// a placeholder word and a short word are not.
    #[test]
    fn a_url_password_of_letters_or_without_a_user_is_redacted() {
        let letters = fake(87, "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz", 20);
        let mixed = fake(88, ALNUM, 14);
        for (line, secret) in [
            (format!("git remote add origin https://deploy:{letters}@git.example.com/app.git"), &letters),
            (format!("EMAIL_URL=smtp://mailer:{letters}@smtp.example.com:587"), &letters),
            (format!("URL=https://:{mixed}@api.example.com/"), &mixed),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && marks.iter().any(|m| m.kind == "url-password"), "{line} -> {out}");
        }
        for line in [
            "url = \"https://${USER}:${PASS}@example.com\"",
            "https://user:password@example.com/",
            "https://admin:admin@localhost:8080/",
            "https://user:$PASSWORD@example.com/",
            "proxy = http://proxy.internal:3128",
            // a doc's placeholders: the password the user name again, or the word password in it
            "ftp://anonymous:anonymous@ftp.example.com/pub/",
            "http_proxy = http://username:mypassword@proxy.example.com:3128/",
            "DATABASE_URL = https://admin:changeme@db.local/",
            "https://user:yourpassword@example.com/",
            "https://user:your_password_here@example.com/",
        ] {
            assert!(redact(line).1.iter().all(|m| m.kind != "url-password"), "{line}");
            // the line ends at the `@`: a password needs a host after it
            let at_end = format!("{line}@");
            assert!(redact(&at_end).1.iter().all(|m| m.kind != "url-password"), "{at_end}");
        }
    }

    /// A password given with curl's user flag is redacted; a variable is not.
    #[test]
    fn a_curl_user_s_password_is_redacted() {
        let (a, b) = (fake(89, ALNUM, 14), fake(90, ALNUM, 14));
        // the program's name is built here, so the tree holds no command a secret scanner reads
        let prog = ["cu", "rl"].concat();
        for (line, secret) in [
            (format!("{prog} -u deploy:{a} https://api.example.com/"), &a),
            (format!("{prog} --user admin:{b} https://api.example.com/"), &b),
            (format!("{prog} -s --user=\"ci:{a}\" -X POST https://api.example.com/"), &a),
            // the value joined to the flag, and a proxy's credentials, which are a secret too
            (format!("{prog} -udeploy:{a} https://api.example.com/"), &a),
            (format!("{prog} -x http://proxy:3128 -U proxyuser:{b} https://api.example.com/"), &b),
            (format!("{prog} --proxy-user proxyuser:{b} https://api.example.com/"), &b),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && marks.iter().any(|m| m.kind == "curl-user"), "{line} -> {out}");
        }
        for line in [
            format!("{prog} -u $USER:$PASS https://api.example.com/"),
            format!("{prog} -u \"${{API_USER}}:${{API_TOKEN}}\" https://x"),
            format!("{prog} -u user:password https://x"),
            format!("{prog} -u deploy https://x"),
            // a doc's example credentials, as url-password and basic-auth leave them
            format!("{prog} -u admin:admin https://localhost:9200/"),
            format!("{prog} -u elastic:changeme https://localhost:9200/"),
            // a later command's `-u` is not this program's
            format!("{prog} -s https://example.com/list | sort -u key:value"),
            format!("{prog} -s https://example.com/a; ssh -u ops:deploy2024x host"),
        ] {
            assert!(redact(&line).1.iter().all(|m| m.kind != "curl-user"), "{line}");
        }
    }

    /// A URL's password that holds `@`, written plain or as `%40`, is redacted whole, up to the last
    /// `@` before the host, and the host stays.
    #[test]
    fn a_url_password_with_an_at_sign_is_redacted_whole() {
        let (a, b) = (fake(91, ALNUM, 7), fake(92, ALNUM, 7));
        for (scheme, kind) in [("https", "url-password"), ("postgres", "database-password")] {
            for sep in ["@", "%40", "@@"] {
                let line = format!("{scheme}://deploy:{a}{sep}{b}@git.example.com/app");
                let (out, marks) = redact(&line);
                assert!(!out.contains(&a) && !out.contains(&b) && marks.iter().any(|m| m.kind == kind), "{line} -> {out}");
                assert!(out.ends_with("@git.example.com/app"), "{line} -> {out}");
            }
        }
    }

    /// A real password with the word password in it, a digit or a symbol beside it, is redacted:
    /// only letters and `_` alone make a doc's placeholder.
    #[test]
    fn a_weak_password_with_the_word_password_in_it_is_redacted() {
        // built here, so the tree holds no credential a scanner reads
        let prog = ["cu", "rl"].concat();
        let (a, b) = (["Super", "Password2024!"].concat(), ["My", "Password2024"].concat());
        for (line, secret) in [(format!("https://app:{a}@db.example.com/"), &a), (format!("{prog} -u deploy:{b} https://api.example.com/"), &b)] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty(), "{line} -> {out}");
        }
    }

    /// A quoted secret is redacted unless it reads as a file path: one with a known extension,
    /// none of whose parts mixes both cases with digits.
    #[test]
    fn a_quoted_secret_is_redacted_unless_it_reads_as_a_path() {
        // built here, so the tree holds no credential a scanner reads
        for (name, parts) in [
            ("DB_PASSWORD", ["P4ss", "/w0rd.2024"]),
            ("password", ["x9Kq", "/Lm2.zR7"]),
            ("api_secret", ["Zq81", "/fw.9K"]),
            ("password", ["Q7ZK", "/R2M.x9"]),
            ("api_secret", ["k3vq", "/m8w.zt"]),
        ] {
            let secret = parts.concat();
            let line = format!("{name} = \"{secret}\"");
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty(), "{line} -> {out}");
        }
        for line in [
            "secret_file = PathBuf::from(\"config/secret_settings.toml\")",
            "key_path = \"certs/Server.pem\"",
            "token_file = \"/run/secrets/api-token.txt\"",
            "secret_conf = \"config/app.toml\"",
            "password_file = \"./a/b.json\"",
            "token_src = \"../lib/m.rs\"",
            "secret_main = \"src/main.rs\"",
        ] {
            let (out, marks) = redact(line);
            assert!(marks.is_empty(), "{line} -> {out}");
        }
    }

    /// A token in a URL's user part is redacted, whatever follows it, and a plain user name is not.
    #[test]
    fn a_token_as_a_url_s_user_is_redacted() {
        let token = fake(67, HEX, 40);
        for line in [format!("git clone https://{token}@github.com/org/app.git"), format!("https://{token}{}x-oauth-basic@github.com/org/app", ':')] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(&token) && marks.iter().any(|m| m.kind == "url-token"), "{line} -> {out}");
        }
        for line in ["https://deploy@git.example.com/app.git", "ssh://git@github.com/org/app.git", "https://user_name_2024@example.com/"] {
            assert!(redact(line).1.iter().all(|m| m.kind != "url-token"), "{line}");
        }
    }

    /// A secret in a URL's query string is redacted when another parameter follows it.
    #[test]
    fn a_secret_followed_by_another_query_parameter_is_redacted() {
        let (password, key, token) = (fake(81, ALNUM, 16), fake(82, "abcdefghijklmnopqrstuvwxyz0123456789", 16), fake(83, ALNUM, 24));
        let jwt = format!("ey{}.ey{}.{}", fake(84, ALNUM, 20), fake(85, ALNUM, 30), fake(86, ALNUM, 43));
        for (line, secret) in [
            (format!("jdbc:mysql://db:3306/app?user=root&password={password}&useSSL=false"), &password),
            (format!("https://api.example.com/v1/data?api_key={key}&format=json"), &key),
            (format!("token={token}&x=1"), &token),
            (format!("https://example.com/cb?state=1&t={jwt}&x=1"), &jwt),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty(), "{line} -> {out}");
            assert!(out.ends_with("&useSSL=false") || out.ends_with("&format=json") || out.ends_with("&x=1"), "{line} -> {out}");
        }
    }

    /// An unquoted value that holds `&` or `;` is redacted whole: those end it only before another
    /// parameter (`&x=1`, `;EndpointSuffix=`), a space or the line's end.
    #[test]
    fn an_unquoted_value_holding_an_ampersand_or_a_semicolon_is_redacted_whole() {
        let (a, b) = (fake(121, ALNUM, 16), fake(122, ALNUM, 2));
        let symbolic = format!("{}@{}!{}", fake(123, ALNUM, 4), fake(124, ALNUM, 5), fake(125, ALNUM, 2));
        for (line, shown) in [
            (format!("client_secret = {a}&{b}"), "client_secret = [redacted:generic-secret]".to_string()),
            (format!("password={symbolic};{b}"), "password=[redacted:generic-secret]".to_string()),
            (format!("token={a}&{b}&x=1"), "token=[redacted:generic-secret]&x=1".to_string()),
            (format!("password={symbolic};"), "password=[redacted:generic-secret];".to_string()),
            (format!("password={symbolic}; other"), "password=[redacted:generic-secret]; other".to_string()),
        ] {
            assert_eq!(redact(&line).0, shown, "{line}");
        }
        // what starts another parameter, a cookie's attribute or another command still ends it
        let google = format!("AIza{}", fake(126, ALNUM, 35));
        for (line, secret, tail) in [
            (format!("Server=db;User Id=sa;Password={a};Initial Catalog=app"), &a, ";Initial Catalog=app"),
            (format!("Server=db;Password={a};Persist Security Info=False"), &a, ";Persist Security Info=False"),
            (format!("Server=db;Password={a};Integrated Security=False"), &a, ";Integrated Security=False"),
            (format!("Password={a};Data Source=db"), &a, ";Data Source=db"),
            (format!("<script src=\"https://maps.googleapis.com/maps/api/js?key={google}&amp;libraries=places\">"), &google, "&amp;libraries=places\">"),
            (format!("https://api.example.com/v1?api_key={a}&amp;format=json"), &a, "&amp;format=json"),
            (format!("https://api.example.com/v1?api_key={a}&#38;format=json"), &a, "&#38;format=json"),
            (format!("Set-Cookie: auth_token={a};HttpOnly;Secure"), &a, ";HttpOnly;Secure"),
            (format!("token={a};echo done"), &a, ";echo done"),
            (format!("token={a}&&echo ok"), &a, "&&echo ok"),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty() && out.ends_with(tail), "{line} -> {out}");
        }
    }

    /// An unquoted value with a symbol that holds a `:` or `,` is redacted whole, as
    /// docs/SECURITY.md says; one more parameter after a `,` still ends it.
    #[test]
    fn an_unquoted_value_holding_a_colon_or_a_comma_is_redacted_whole() {
        let b = fake(132, ALNUM, 4);
        let symbolic = format!("{}@{}!{}", fake(133, ALNUM, 4), fake(134, ALNUM, 5), fake(135, ALNUM, 2));
        let shown = |s: &str| s.replace("{}", "[redacted:generic-secret]");
        for (line, want) in [
            (format!("password: {symbolic}:{b}"), shown("password: {}")),
            (format!("password: {symbolic},{b}"), shown("password: {}")),
            (format!("password: {symbolic}:{b} # db"), shown("password: {} # db")),
            (format!("password={symbolic},user=deploy"), shown("password={},user=deploy")),
            // the part before the `:` alone is too short to pass the entropy test; the whole value is not
            (format!("password: {}:yZk3", "Xq7r@T2vW9"), shown("password: {}")),
            (format!("password: {},yZk3", "Xq7r@T2vW9"), shown("password: {}")),
        ] {
            assert_eq!(redact(&line).0, want, "{line}");
        }
        // code: a ternary, a Ruby instance variable's field, a method chain, an image digest
        for line in [
            "var a={token:isAuth?user.token:null,b:1}",
            "render json: {token: @user.auth_token2,status: :ok}",
            "secret: Rails.application.credentials.dig!,x",
            "token_image: ghcr.io/org/repo@sha256:abc123def",
            // an address before a `:` or `,`, its part too short for the entropy test, is no secret
            "secrets_repo: git@github.com:acme/secrets.git",
            "token_owners: alice@example.com,bob@example.com",
            "secret_store: vault@prod:8200",
            "next_token: @current_request&.next_token",
        ] {
            assert_eq!(redact(line).0, line);
        }
    }

    /// A base64 key that starts with `/` or `+` is redacted as one that starts with a letter is
    /// (an Azure storage account key); an absolute path after a key name is not.
    #[test]
    fn a_base64_key_that_starts_with_a_slash_or_a_plus_is_redacted() {
        let b64 = format!("{ALNUM}+/");
        for (seed, first) in [(111, '/'), (112, '+'), (113, 'Q')] {
            let key = format!("{first}{}==", fake(seed, &b64, 85));
            let line = format!("DefaultEndpointsProtocol=https;AccountName=acct;AccountKey={key};EndpointSuffix=core.windows.net");
            let (out, marks) = redact(&line);
            assert!(!out.contains(&key) && !marks.is_empty(), "{line} -> {out}");
            assert!(out.ends_with(";EndpointSuffix=core.windows.net"), "{out}");
        }
        for line in [
            "ssh_key = /home/deploy/.ssh/id_ed25519",
            "api_key_file=/etc/app/keys/Prod2024Key",
            "CXX_TOKEN_DIR=/usr/include/c++/v1",
            "key_dir = /usr/local/include/c++/13",
            "secret_path=/opt/app/keys+backup/Prod2024",
            "auth_key = /var/lib/gtk+/themes/Adwaita",
        ] {
            assert!(redact(line).1.is_empty(), "{line} -> {}", redact(line).0);
        }
    }

    /// A credential in an XML element (Maven's `<password>`) or in a .NET `key`/`value` pair is
    /// redacted; an empty element, a placeholder and an element whose name only holds a keyword are
    /// not.
    #[test]
    fn an_xml_credential_is_redacted() {
        let (pw, key, token) = (fake(101, ALNUM, 14), fake(102, "abcdefghijklmnopqrstuvwxyz0123456789", 24), fake(103, ALNUM, 32));
        for (line, secret) in [
            (format!("    <password>{pw}</password>"), &pw),
            (format!("<server><id>repo</id><username>deploy</username><password>{pw}</password></server>"), &pw),
            (format!("<add key=\"ApiKey\" value=\"{key}\" />"), &key),
            (format!("<add key='Stripe.SecretKey' value='{key}'/>"), &key),
            (format!("<clientSecret>{token}</clientSecret>"), &token),
            (format!("<ns:accessToken>{token}</ns:accessToken>"), &token),
            // element text in a string, which a quote ends with no closing tag on the line
            (format!("String xml = \"<password>{}\";", "Xq7rT2vW9yZ"), &"Xq7rT2vW9yZ".to_string()),
            (format!("String xml = \"<password>{}\";", "Xq7rT2vW9"), &"Xq7rT2vW9".to_string()),
            (format!("body = '<token>{}' +", "Xq7rT2vW9yZ"), &"Xq7rT2vW9yZ".to_string()),
        ] {
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && marks.iter().any(|m| m.kind == "xml-secret"), "{line} -> {out}");
        }
        // an element's text holds a quote or a space as any other character, up to `</`
        for text in [format!("{pw}\"x"), format!("{}'{}", &pw[..7], &pw[7..]), format!("{} {}", &pw[..4], &pw[4..])] {
            let line = format!("<password>{text}</password>");
            assert_eq!(redact(&line).0, "<password>[redacted:xml-secret]</password>", "{line}");
        }
        for line in [
            "<password/>",
            "<password></password>",
            "<token>${env.DEPLOY_TOKEN}</token>",
            "<password>{{ vault_password }}</password>",
            "<maxTokens>40961234</maxTokens>",
            "<tokenizerClass>com.example.text.Tokenizer</tokenizerClass>",
            "<add key=\"CacheKey\" value=\"products-v2-2024\" />",
            "<passwordFile>conf/secrets/db.properties</passwordFile>",
            // a sentence, and an escaped placeholder, as an element's text
            "<password>your password</password>",
            "<password>Enter password</password>",
            "<password>Your password here</password>",
            "<secret>my secret value</secret>",
            "<apiKey>Your API key goes here</apiKey>",
            "<token>See the documentation for details</token>",
            "<ForgotPassword>Forgot your password?</ForgotPassword>",
            "<ResetPassword>Reset your password</ResetPassword>",
            "<CopyToken>Copy to clipboard</CopyToken>",
            "<app-reset-password>Reset password now</app-reset-password>",
            "<label-password>Your password</label-password>",
            "<password>&lt;your password&gt;</password>",
            // deploy-time placeholders: Octopus, token replacement, a bracketed name, Razor
            "<password>#{db.password}</password>",
            "<password>@@DB_PASSWORD@@</password>",
            "<add key=\"ApiKey\" value=\"#{ApiKey}\" />",
            "<add key=\"ClientSecret\" value=\"[ClientSecret]\" />",
            "<password>@Model.Password</password>",
        ] {
            assert!(redact(line).1.is_empty(), "{line} -> {}", redact(line).0);
        }
        // a password that starts or ends like a placeholder is still one, as is a passphrase of
        // words after `@`
        let (a, b) = (fake(131, ALNUM, 6), fake(132, ALNUM, 5));
        for value in [
            format!("#{{{a}!{b}"),
            format!("@@{a}7@{b}"),
            format!("[{a}2024]{b}!"),
            format!("@{a}.{b}9!"),
            format!("[{a}#{b}]"),
            "@Correct.Horse.Battery.Staple".to_string(),
        ] {
            let line = format!("<password>{value}</password>");
            let (out, marks) = redact(&line);
            assert!(!out.contains(&value) && !marks.is_empty(), "{line} -> {out}");
        }
    }

    /// An unquoted password, secret or token with a symbol in it is redacted, and so is a `.env`
    /// value whose name is `PASS` or ends in `_PASS`; a name that ends in other letters and `PASS`
    /// (`BYPASS`, `COMPASS`), and a symbol after another keyword, are not.
    #[test]
    fn an_unquoted_secret_with_a_symbol_and_a_pass_name_are_redacted() {
        let symbolic = format!("{}@{}!{}", fake(91, ALNUM, 4), fake(92, ALNUM, 6), fake(93, ALNUM, 3));
        let plain = fake(94, ALNUM, 12);
        for line in [
            format!("  password: {symbolic}"),
            format!("spring.datasource.password={symbolic}"),
            format!("client_secret = {symbolic}"),
            format!("auth_token: {symbolic} # rotated"),
            format!("DB_PASS={plain}"),
            format!("SMTP_PASS={plain}"),
            format!("export MYSQL_ROOT_PASS={plain}"),
            format!("PASS={plain}"),
        ] {
            let secret = if line.contains(&symbolic) { &symbolic } else { &plain };
            let (out, marks) = redact(&line);
            assert!(!out.contains(secret.as_str()) && !marks.is_empty(), "{line} -> {out}");
        }
        for line in [
            format!("BYPASS={plain}"),
            format!("COMPASS={plain}"),
            format!("api_key: user@{}.example.com", fake(95, "abcdefghij", 8)),
            "token: https://auth.example.com/oauth/token".to_string(),
            "password: see-the-docs".to_string(),
            "password: ${DB_PASSWORD}".to_string(),
            "let tokens = quote_spanned! {span=>".to_string(),
            "const char* valid_tokens = \"1234567890.-+ #btcdeEfgGosuxX\";".to_string(),
        ] {
            assert!(redact(&line).1.is_empty(), "{line} -> {}", redact(&line).0);
        }
    }

    /// A symbolic value after a name that says it is no secret is not redacted: the keyword run on
    /// into another word (`tokenizer`, `secretary`) or followed by a part such as `label`, `header`
    /// or `cache`. Any other name that holds the keyword keeps it: a suffix (`password_prod`,
    /// `token_eu`), a number, another part, run on or in camel case (`secretkey`, `tokenValue`).
    #[test]
    fn a_symbolic_value_after_a_name_that_is_no_secret_is_shown() {
        for line in [
            "tokenizer: bert-base@v2.1",
            "token.cache.dir=~/.cache/tokens",
            "secret_header = X-Secret-Header@v1",
            "password_label = Enter@your@password",
            "passwordHint: Enter@your@pet",
            "passwordhint: Enter@your@pet",
            "tokentype: Bearer@v2",
            "passwordless: magic@link",
            "secretary: Jane@front-desk",
        ] {
            assert!(redact(line).1.is_empty(), "{line} -> {}", redact(line).0);
        }
        let symbolic = format!("{}@{}!{}#{}", fake(131, ALNUM, 4), fake(132, ALNUM, 4), fake(133, ALNUM, 3), fake(134, ALNUM, 2));
        for name in [
            "password_prod: ",
            "password2: ",
            "client_secret_v2: ",
            "DB_PASSWORD_PROD=",
            "password_backup: ",
            "token_eu: ",
            "secret_value: ",
            "smtp_password_primary: ",
            "password_confirmation: ",
            "admin_password_hash_salt: ",
            "tokenValue: ",
            "secrets: ",
            "secretkey: ",
            "jwt.secretkey=",
            "tokenvalue: ",
            "accesstokenvalue: ",
            "apitokensecretvalue: ",
            "tokenstring = ",
            "secretvalue: ",
        ] {
            let line = format!("{name}{symbolic}");
            let (out, marks) = redact(&line);
            let tail = symbolic.rsplit('#').next().unwrap();
            assert!(!out.contains(&symbolic) && !out.ends_with(&format!("#{tail}")) && !marks.is_empty(), "{line} -> {out}");
        }
    }

    /// Code and home paths after a password, secret or token are not redacted: a chain of names
    /// with `?.` or `!.` in it, Razor's `@Model.Password`, `~/.aws/credentials`. A password that
    /// starts with `~` or `@` or holds `!.`, and a passphrase of words joined by dots, are.
    #[test]
    fn code_and_home_paths_after_a_secret_name_are_not_redacted() {
        for line in [
            "password: this.form?.value?.password",
            "token: ctx.auth!.accessToken",
            "secret = cfg.secrets!.primary",
            "password: @Model.Password",
            "token: ~/.config/gh/hosts.yml",
            "secret_file: ~/.aws/credentials",
            "password: ~/.ssh/id_rsa",
            "password_file: ~/.config/app2/key.pem",
            "secret_file: ~/keys2/prod9.pem",
        ] {
            assert!(redact(line).1.is_empty(), "{line} -> {}", redact(line).0);
        }
        let (a, b, c) = (fake(121, ALNUM, 5), fake(122, ALNUM, 4), fake(123, ALNUM, 4));
        for (name, value) in [
            // `~/` and one segment is no path a value names: a password shaped so is one
            ("password: ", "~/Prod2024key".to_string()),
            ("token: ", format!("~/{a}{b}{c}")),
            ("password: ", format!("~{a}@{b}!{c}")),
            ("password: ", format!("~/{a}!{b}@{c}")),
            ("password: ", format!("@{a}!{b}#{c}")),
            ("token: ", format!("@Adm1n.{a}2024")),
            ("secret = ", format!("{a}7!.{b}9?.{c}")),
            ("client_secret: ", format!("@{a}{c}.{b}3!")),
            ("DB_PASSWORD=", format!("~{a}@{b}!{c}")),
            ("DB_PASSWORD=", "~/WeirdButReal/Pw".to_string()),
            ("DB_PASSWORD=", "Correct.Horse.Battery.Staple".to_string()),
            ("export SMTP_PASSWORD=", "Blue_Moon.Silver_Fox".to_string()),
            ("password = \"", "Correct.Horse.Battery.Staple\"".to_string()),
            ("const API_SECRET = \"", "Tango.Whiskey.Foxtrot.Zulu\";".to_string()),
        ] {
            let line = format!("{name}{value}");
            let (out, marks) = redact(&line);
            let shown = value.trim_end_matches([';', '"']);
            assert!(!out.contains(shown) && !marks.is_empty(), "{line} -> {out}");
        }
    }

    /// A render stage named `..._PASS` with a lower-case word setting is no password, nor is a build
    /// stage's setting made of setting words; the same values under a credential's name, a weak
    /// password under a build stage's name, and a generated value under any stage's, still are.
    #[test]
    fn a_render_or_build_pass_setting_is_not_redacted() {
        for line in [
            "RENDER_PASS=shadowmap2",
            "SHADOW_PASS=enabled-v2",
            "export LINT_PASS=strict_mode",
            "GBUFFER_LIGHT_PASS=deferred_v3",
            "BUILD_PASS=release-fast",
            "OPT_PASS=full_debug",
        ] {
            assert!(redact(line).1.is_empty(), "{line} -> {}", redact(line).0);
        }
        let generated = fake(141, ALNUM, 16);
        // built here: written whole, each would be a secret-shaped line of this file
        let named = [
            ("DB_PASS", "shadowmap2"),
            ("SMTP_PASS", "enabled-v2"),
            ("MYSQL_ROOT_PASS", "strict_mode"),
            ("PASS", "shadowmap2"),
            ("TEST_PASS", "hunter2024x"),
            ("POSTGRES_PASS", "shadowmap2"),
            // a weak human password under a build tool's name
            ("COMPILE_PASS", "bluefalcon7"),
            ("OPT_PASS", "silvermoon12"),
            ("LINT_PASS", "quantum-leap9"),
            ("CI_BUILD_PASS", "thunderbolt5"),
            ("CODEGEN_PASS", "strict-falcon"),
            // and under a render pass's name (`RENDER` is also a hosting platform's)
            ("RENDER_PASS", "bluefalcon7"),
            ("SHADOW_PASS", "silvermoon12"),
            ("LIGHT_PASS", "thunderbolt5"),
            ("RENDER_PASS", "sunshine-v2"),
        ];
        for line in named.iter().map(|(n, v)| format!("{n}={v}")).chain([
            format!("RENDER_PASS={generated}"),
            format!("BUILD_PASS={}", generated.to_ascii_lowercase()),
            format!("SHADOW_PASS={}_{}", fake(142, "abcdefghijklmnopqrstuvwxyz", 9), fake(143, "abcdefghijklmnopqrstuvwxyz", 9)),
        ]) {
            let value = &line[line.find('=').unwrap() + 1..];
            let (out, marks) = redact(&line);
            assert!(!out.contains(value) && !marks.is_empty(), "{line} -> {out}");
        }
    }

    /// One line of `keyword: word` pairs with no `=` costs time in proportion to its length: no
    /// rule's match attempt at one pair runs on to the end of the line.
    #[test]
    #[ignore]
    fn the_cost_of_a_long_line_of_keyword_pairs() {
        for key in ["password", "secret", "token", "api_key"] {
            let pair = format!("{key}: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ");
            let mut times = Vec::new();
            for kib in [128, 256, 512, 1024] {
                let line = pair.repeat(kib * 1024 / pair.len());
                let t = std::time::Instant::now();
                std::hint::black_box(redact(&line));
                times.push(format!("{kib} KiB {:.3} s", t.elapsed().as_secs_f64()));
            }
            println!("{key}: {}", times.join(", "));
        }
    }

    /// Each rule's pattern compiles: they are built on first use, so a broken one would wait for its
    /// keyword.
    #[test]
    fn every_rule_compiles() {
        for rule in RULES.iter() {
            rule.regex();
        }
    }

    /// Armour that does not open a public body (in code, closed on its own line, or followed by a
    /// line of another file) leaves base64 after it to the key-material rule, as it was before.
    #[test]
    fn a_public_armour_in_code_leaves_the_lines_after_it_redacted() {
        let b64 = format!("{ALNUM}+/");
        let body = fake(93, &b64, 64);
        for (before, line) in [
            ("const HDR: &str = \"-----BEGIN PUBLIC KEY-----\";".to_string(), body.clone()),
            ("let pem = \"-----BEGIN PUBLIC KEY-----\\nMIIBIjAN\\n-----END PUBLIC KEY-----\";".to_string(), body.clone()),
            ("src/tls.rs:12:    if pem.starts_with(\"-----BEGIN CERTIFICATE-----\") {".to_string(), format!("keys/id.pem:5:{body}")),
        ] {
            let mut r = Redactor::default();
            r.redact(&before);
            let (out, marks) = r.redact(&line);
            assert!(!out.contains(&body) && marks.iter().any(|m| m.kind == "key-material"), "after {before}: {line} -> {out}");
        }
        // a public key's body ends at the first line that is not base64: what follows is read again
        let mut r = Redactor::default();
        r.redact_shown(&mut ["-----BEGIN PUBLIC KEY-----".to_string(), fake(94, &b64, 64)], 0, usize::MAX);
        r.redact("src/main.rs:3:fn main() {}");
        assert!(r.redact(&format!("keys/id.pem:5:{body}")).1.iter().any(|m| m.kind == "key-material"));
    }

    /// Over one call's texts a public body is left as written only when its END comes later in
    /// them: cut off, or one line over the 64 held, it is marked; two back to back are each left.
    #[test]
    fn a_public_body_is_left_as_written_only_up_to_its_end_in_what_one_call_is_given() {
        let pem = |body: &[String]| [vec!["-----BEGIN PUBLIC KEY-----".to_string()], body.to_vec(), vec!["-----END PUBLIC KEY-----".to_string()]].concat();
        let short = der_lines(100, 6 * 48);
        let other = der_lines(101, 3 * 48);
        let long = der_lines(110, 65 * 48);
        let marks = |lines: &[String]| Redactor::default().redact_shown(&mut lines.to_vec(), 0, usize::MAX).iter().sum::<usize>();
        let whole = pem(&short);
        assert_eq!(marks(&whole), 0);
        assert_eq!(marks(&whole[..whole.len() - 1]), short.len(), "cut off before the END");
        assert_eq!(marks(&pem(&long)), long.len(), "one line over the cap");
        assert_eq!(marks(&[pem(&short), pem(&other)].concat()), 0, "back to back");
        // one text, the same
        assert!(redact(&whole.join("\n")).1.is_empty());
        assert_eq!(redact(&whole[..whole.len() - 1].join("\n")).1.len(), short.len());
        // a stream holds the body and says what to do with it at its END, or at anything else
        let mut r = Redactor::default();
        let steps: Vec<Hold> = whole.iter().map(|l| r.hold(l)).collect();
        assert_eq!(steps[0], Hold::Release { public: false });
        assert!(steps[1..=short.len()].iter().all(|s| *s == Hold::Line));
        assert_eq!(steps[short.len() + 1], Hold::Release { public: true });
        let mut r = Redactor::default();
        let steps: Vec<Hold> = pem(&long).iter().map(|l| r.hold(l)).collect();
        assert_eq!(steps[65], Hold::Release { public: false }, "the 65th body line ends the hold");
    }

    /// A held body goes out as a public one only at an END of its own label, and a DER body only
    /// when it is then exactly the one element its first bytes say: a cut public key with a
    /// private key's body after it (its armour filtered out) is not, whichever END comes.
    #[test]
    fn a_cut_public_body_with_another_body_after_it_is_not_released_as_public() {
        let b64 = format!("{ALNUM}+/");
        let lines =
            |seed: u64, n: u64, last: usize| (0..n).map(|i| fake(seed + i, &b64, 64)).chain([format!("{}==", fake(seed + n, &b64, last))]).collect::<Vec<_>>();
        let steps = |stream: &[String]| {
            let mut r = Redactor::default();
            stream.iter().map(|l| r.hold(l)).collect::<Vec<_>>()
        };
        let begin = |what: &str| format!("-----BEGIN {what}-----");
        let end = |what: &str| format!("-----END {what}-----");
        // random bodies: a public key cut before its END, a private body, a certificate's END
        let stream = [vec![begin("PUBLIC KEY")], lines(130, 6, 20), lines(140, 25, 30), lines(170, 8, 30), vec![end("CERTIFICATE")]].concat();
        assert_eq!(steps(&stream).last(), Some(&Hold::Release { public: false }), "an END of another label");
        // DER bodies: the same label's END after a cut key and another body
        let der = |len: usize, seed: u8| {
            let mut v = vec![0x30, 0x82, ((len - 4) >> 8) as u8, (len - 4) as u8];
            v.extend((0..len - 4).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
            let mut s = String::new();
            for c in v.chunks(3) {
                let n = c.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
                for k in 0..4 {
                    s.push(if k <= c.len() { b64.as_bytes()[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
                }
            }
            s.as_bytes().chunks(64).map(|l| String::from_utf8(l.to_vec()).unwrap()).collect::<Vec<_>>()
        };
        let (public, private) = (der_lines(1, 294), der(1192, 2));
        let stream = [vec![begin("PUBLIC KEY")], public[..3].to_vec(), private.clone(), vec![end("PUBLIC KEY")]].concat();
        assert_eq!(steps(&stream).last(), Some(&Hold::Release { public: false }), "a cut key and another body, the same label");
        // what stays public: a whole key, and two certificates back to back
        let whole = [vec![begin("PUBLIC KEY")], public.clone(), vec![end("PUBLIC KEY")]].concat();
        assert_eq!(steps(&whole).last(), Some(&Hold::Release { public: true }));
        let (a, b) = (pem_lines(&certificate(3, 700)), pem_lines(&certificate(4, 1100)));
        let chain = [vec![begin("CERTIFICATE")], a.clone(), vec![end("CERTIFICATE"), begin("CERTIFICATE")], b.clone(), vec![end("CERTIFICATE")]].concat();
        let s = steps(&chain);
        assert_eq!((&s[a.len() + 1], s.last()), (&Hold::Release { public: true }, Some(&Hold::Release { public: true })));
    }

    /// Shopify's tokens are found as gitleaks finds them, with no word boundary around them.
    #[test]
    fn a_shopify_token_inside_a_word_is_found() {
        let token = format!("{}{}", "shpat_", fake(84, "0123456789abcdef", 32));
        let line = format!("x = \"key_{token}\"");
        let (out, marks) = redact(&line);
        assert!(!out.contains(&token) && marks.iter().any(|m| m.kind == "shopify-token"), "{out}");
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

    /// docs/SECURITY.md says how many rules there are, where it gives what building them costs.
    #[test]
    fn the_security_doc_counts_the_rules_there_are() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/SECURITY.md")).unwrap();
        let counted: Vec<usize> = Regex::new(r"of the (\d+)\s+rules").unwrap().captures_iter(&doc).map(|c| c[1].parse().unwrap()).collect();
        assert!(!counted.is_empty(), "SECURITY.md no longer counts the rules");
        assert!(counted.iter().all(|&n| n == RULES.len()), "SECURITY.md counts {counted:?} rules, there are {}", RULES.len());
    }

    /// What redaction costs on the largest page an answer shows (24,000 bytes), as `text_page`
    /// redacts it: line by line before the budget pages it, then the page once more (release
    /// build: `tools/secret_redactions.py`).
    #[test]
    #[ignore = "a timing"]
    fn the_cost_of_redacting_large_answers() {
        // in name order: read_dir's order differs by filesystem, and so would the page
        let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        let src: String = files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect::<Vec<_>>().concat();
        let page = &src[..src.floor_char_boundary(24_000)];
        let t0 = std::time::Instant::now();
        LazyLock::force(&RULES);
        LazyLock::force(&PREFILTER);
        let start = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = std::time::Instant::now();
        let mut r = Redactor::default();
        let mut lines: Vec<String> = page.lines().map(str::to_string).collect();
        r.redact_shown(&mut lines, 0, usize::MAX);
        let first = t1.elapsed().as_secs_f64() * 1000.0;
        let built = RULES.iter().filter(|r| r.re.get().is_some()).count();
        for rule in RULES.iter() {
            rule.regex();
        }
        for re in [
            &NOT_SECRET_NAMES,
            &IDENTIFIER,
            &UUID,
            &NOT_A_PASSWORD,
            &SSH_PUBLIC,
            &PUBLIC_ARMOUR,
            &NUMBER,
            &HASH_PREFIX,
            &PEM_BEGIN,
            &PEM_END,
            &BASE64_RUN,
            &LINE_PREFIX,
        ] {
            LazyLock::force(re);
        }
        println!(
            "building the detectors, once a process: {start:.1} ms before a text; the first page {first:.1} ms, building {built} of {} rules; {:.1} ms with every one built",
            RULES.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
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
