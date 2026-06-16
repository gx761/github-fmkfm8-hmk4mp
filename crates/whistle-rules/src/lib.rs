//! whistle 规则 DSL：解析、匹配与求值。
//!
//! 对应 whistle 的 `lib/rules`。详见 `docs/04-rules-engine.md`。
//!
//! # 语法
//!
//! 每行一条规则，形如 `<匹配模式> <协议>://<参数> ...`。本实现采用
//! 「第一个匹配模式候选 token 作为 pattern，其余 token 作为 operation」的策略，
//! 兼容 pattern-first 与常见的 operator-first 写法。
//!
//! ## 匹配模式（[`Pattern`]）
//! - **正则**：`/regex/` 或 `/regex/i`，对完整 URL 做搜索匹配；
//! - **通配符**：含 `*`，转为正则做前缀匹配；
//! - **URL 前缀**：含 `scheme://`（http/https/ws/wss/tunnel），对 URL 做前缀匹配；
//! - **域名+路径**：无 scheme，按 host 精确 + path 前缀匹配。
//!
//! ## 操作（[`Operation`]）
//! `协议://参数`；无 `://` 的裸 token 视为 `host` 简写。

use regex::Regex;

/// 解析错误。
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("第 {line} 行正则非法: {source}")]
    BadRegex {
        line: usize,
        #[source]
        source: regex::Error,
    },
}

/// 被识别为「匹配模式」的协议 scheme（出现在 `scheme://` 中时按 pattern 处理）。
const PATTERN_SCHEMES: &[&str] = &["http", "https", "ws", "wss", "tunnel"];

/// 一条规则的「操作」。
#[derive(Debug, Clone)]
pub struct Operation {
    /// 协议名（小写），如 `host`、`file`、`reqHeaders`。
    pub protocol: String,
    /// 协议参数（`://` 之后的部分）。
    pub value: String,
    /// 原始 token。
    pub raw: String,
}

impl Operation {
    fn parse(token: &str) -> Self {
        match token.split_once("://") {
            // 协议名保留原始大小写（whistle 协议为 camelCase，如 reqHeaders）。
            Some((proto, value)) => Operation {
                protocol: proto.to_string(),
                value: value.to_string(),
                raw: token.to_string(),
            },
            // 无 scheme 的裸 token：host 简写（如 `1.2.3.4:8080`）。
            None => Operation {
                protocol: "host".to_string(),
                value: token.to_string(),
                raw: token.to_string(),
            },
        }
    }
}

/// 匹配模式。
#[derive(Debug, Clone)]
pub enum Pattern {
    /// `/regex/[i]`，对完整 URL 搜索匹配。
    Regex(Regex),
    /// 含 `*` 的通配符，转正则后对目标做前缀匹配。
    Wildcard { re: Regex, with_scheme: bool },
    /// `scheme://...` 前缀匹配。
    UrlPrefix(String),
    /// 域名 + 路径前缀。
    DomainPath { host: String, path: String },
}

impl Pattern {
    fn parse(token: &str, line: usize) -> Result<Pattern, ParseError> {
        // 正则：以 '/' 开头。
        if let Some(rest) = token.strip_prefix('/') {
            if let Some(close) = rest.rfind('/') {
                let body = &rest[..close];
                let flags = &rest[close + 1..];
                let mut builder = regex::RegexBuilder::new(body);
                if flags.contains('i') {
                    builder.case_insensitive(true);
                }
                let re = builder
                    .build()
                    .map_err(|source| ParseError::BadRegex { line, source })?;
                return Ok(Pattern::Regex(re));
            }
        }

        let has_scheme = token
            .split_once("://")
            .map(|(s, _)| PATTERN_SCHEMES.contains(&s))
            .unwrap_or(false);

        // 通配符。
        if token.contains('*') {
            let re_src = format!("^{}", wildcard_to_regex(token));
            let re = Regex::new(&re_src).map_err(|source| ParseError::BadRegex { line, source })?;
            return Ok(Pattern::Wildcard {
                re,
                with_scheme: token.contains("://"),
            });
        }

        if has_scheme {
            return Ok(Pattern::UrlPrefix(token.to_string()));
        }

        // 域名 + 路径。
        let (host, path) = match token.split_once('/') {
            Some((h, p)) => (h.to_string(), format!("/{p}")),
            None => (token.to_string(), String::new()),
        };
        Ok(Pattern::DomainPath {
            host: strip_port(&host).to_ascii_lowercase(),
            path,
        })
    }

    /// 判断该模式是否匹配请求。
    pub fn matches(&self, input: &MatchInput) -> bool {
        match self {
            Pattern::Regex(re) => re.is_match(&input.url),
            Pattern::Wildcard { re, with_scheme } => {
                if *with_scheme {
                    re.is_match(&input.url)
                } else {
                    re.is_match(&input.host_path())
                }
            }
            Pattern::UrlPrefix(p) => input.url.starts_with(p),
            Pattern::DomainPath { host, path } => {
                input.host.eq_ignore_ascii_case(host)
                    && (path.is_empty() || input.path.starts_with(path.as_str()))
            }
        }
    }
}

/// 把通配符模式转为正则源串（不含锚点）。`*` → `.*`，其余字符转义。
fn wildcard_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for ch in pattern.chars() {
        if ch == '*' {
            out.push_str(".*");
        } else if "\\.+?()|[]{}^$".contains(ch) {
            out.push('\\');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// 去掉 host 中的端口。
fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// 一条规则。
#[derive(Debug, Clone)]
pub struct Rule {
    pub pattern: Pattern,
    pub operations: Vec<Operation>,
    pub raw: String,
    pub line: usize,
}

/// 解析后的规则集。
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

/// 请求匹配输入。
#[derive(Debug, Clone)]
pub struct MatchInput {
    /// 归一化 URL：`scheme://host/path`（不含端口与 query）。
    pub url: String,
    /// 请求 host（不含端口，小写比较）。
    pub host: String,
    /// 请求路径。
    pub path: String,
}

impl MatchInput {
    /// 由各部分构造归一化输入。
    pub fn new(scheme: &str, host: &str, path: &str) -> Self {
        let host = strip_port(host).to_string();
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        };
        MatchInput {
            url: format!("{scheme}://{host}{path}"),
            host,
            path,
        }
    }

    fn host_path(&self) -> String {
        format!("{}{}", self.host, self.path)
    }
}

impl RuleSet {
    /// 解析规则文本。空行与 `#` 注释会被忽略。
    pub fn parse(text: &str) -> Result<RuleSet, ParseError> {
        let mut rules = Vec::new();
        for (idx, raw_line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let tokens: Vec<&str> = line.split_whitespace().collect();
            // 选第一个「模式候选」token 作为 pattern。
            let Some(pat_idx) = tokens.iter().position(|t| is_pattern_candidate(t)) else {
                continue;
            };
            let pattern = Pattern::parse(tokens[pat_idx], line_no)?;
            let operations: Vec<Operation> = tokens
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != pat_idx)
                .map(|(_, t)| Operation::parse(t))
                .collect();
            if operations.is_empty() {
                continue;
            }
            rules.push(Rule {
                pattern,
                operations,
                raw: line.to_string(),
                line: line_no,
            });
        }
        Ok(RuleSet { rules })
    }

    /// 规则条数。
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 对一个请求求值，按规则书写顺序收集所有命中规则的操作。
    pub fn match_request(&self, input: &MatchInput) -> Vec<Operation> {
        let mut ops = Vec::new();
        for rule in &self.rules {
            if rule.pattern.matches(input) {
                ops.extend(rule.operations.iter().cloned());
            }
        }
        ops
    }
}

/// 判断 token 是否可作为「匹配模式」。
fn is_pattern_candidate(token: &str) -> bool {
    if token.starts_with('/') {
        return true; // 正则
    }
    match token.split_once("://") {
        Some((scheme, _)) => PATTERN_SCHEMES.contains(&scheme),
        None => true, // 无 scheme 的域名/路径/通配符
    }
}

/// 去掉行内 `#` 注释。
fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(pos) => &line[..pos],
        None => line,
    }
}

/// 从一组操作中取某协议「最后一次」出现的值（单值协议用）。
pub fn last_value<'a>(ops: &'a [Operation], protocol: &str) -> Option<&'a str> {
    ops.iter()
        .rev()
        .find(|o| o.protocol == protocol)
        .map(|o| o.value.as_str())
}

/// 取某协议的所有值（多值协议用），按出现顺序。
pub fn all_values<'a>(ops: &'a [Operation], protocol: &str) -> Vec<&'a str> {
    ops.iter()
        .filter(|o| o.protocol == protocol)
        .map(|o| o.value.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(url_scheme: &str, host: &str, path: &str) -> MatchInput {
        MatchInput::new(url_scheme, host, path)
    }

    #[test]
    fn parses_pattern_first() {
        let rs = RuleSet::parse("www.example.com host://1.2.3.4:8080").unwrap();
        assert_eq!(rs.len(), 1);
        let ops = rs.match_request(&input("http", "www.example.com", "/a"));
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].protocol, "host");
        assert_eq!(ops[0].value, "1.2.3.4:8080");
    }

    #[test]
    fn operator_first_is_supported() {
        // file:// 不是模式候选，域名才是 pattern。
        let rs = RuleSet::parse("file:///tmp/x.json www.example.com").unwrap();
        let ops = rs.match_request(&input("http", "www.example.com", "/"));
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].protocol, "file");
        assert_eq!(ops[0].value, "/tmp/x.json");
    }

    #[test]
    fn bare_value_is_host_shorthand() {
        let rs = RuleSet::parse("example.com 127.0.0.1:9000").unwrap();
        let ops = rs.match_request(&input("http", "example.com", "/"));
        assert_eq!(ops[0].protocol, "host");
        assert_eq!(ops[0].value, "127.0.0.1:9000");
    }

    #[test]
    fn domain_path_prefix_match() {
        let rs = RuleSet::parse("api.example.com/v1 statusCode://503").unwrap();
        // 路径前缀命中
        assert!(!rs
            .match_request(&input("http", "api.example.com", "/v1"))
            .is_empty());
        assert!(!rs
            .match_request(&input("http", "api.example.com", "/v1/users"))
            .is_empty());
        // 路径前缀不符则不命中
        assert!(rs
            .match_request(&input("http", "api.example.com", "/v2"))
            .is_empty());
        assert!(rs
            .match_request(&input("http", "api.example.com", "/other"))
            .is_empty());
    }

    #[test]
    fn wildcard_subdomain() {
        let rs = RuleSet::parse("*.example.com host://10.0.0.1").unwrap();
        assert!(!rs
            .match_request(&input("http", "a.example.com", "/"))
            .is_empty());
        assert!(!rs
            .match_request(&input("http", "b.a.example.com", "/x"))
            .is_empty());
        assert!(rs
            .match_request(&input("http", "example.org", "/"))
            .is_empty());
    }

    #[test]
    fn regex_pattern() {
        let rs = RuleSet::parse(r#"/\.(png|jpg)$/ statusCode://404"#).unwrap();
        assert!(!rs
            .match_request(&input("http", "cdn.x.com", "/a.png"))
            .is_empty());
        assert!(rs
            .match_request(&input("http", "cdn.x.com", "/a.gif"))
            .is_empty());
    }

    #[test]
    fn regex_case_insensitive_flag() {
        let rs = RuleSet::parse("/EXAMPLE/i host://1.1.1.1").unwrap();
        assert!(!rs
            .match_request(&input("http", "example.com", "/"))
            .is_empty());
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let text = "# 注释\n\nexample.com host://1.2.3.4 # 行内注释\n";
        let rs = RuleSet::parse(text).unwrap();
        assert_eq!(rs.len(), 1);
        let ops = rs.match_request(&input("http", "example.com", "/"));
        assert_eq!(ops[0].value, "1.2.3.4");
    }

    #[test]
    fn multiple_ops_and_helpers() {
        let rs =
            RuleSet::parse("example.com host://1.2.3.4 reqHeaders://a=1 reqHeaders://b=2").unwrap();
        let ops = rs.match_request(&input("http", "example.com", "/"));
        assert_eq!(last_value(&ops, "host"), Some("1.2.3.4"));
        assert_eq!(all_values(&ops, "reqHeaders"), vec!["a=1", "b=2"]);
    }

    #[test]
    fn url_prefix_pattern() {
        let rs = RuleSet::parse("http://example.com/api redirect://http://localhost/api").unwrap();
        assert!(!rs
            .match_request(&input("http", "example.com", "/api/x"))
            .is_empty());
        assert!(rs
            .match_request(&input("http", "example.com", "/other"))
            .is_empty());
    }

    #[test]
    fn later_rules_override_via_last_value() {
        let text = "example.com host://1.1.1.1\nexample.com host://2.2.2.2";
        let rs = RuleSet::parse(text).unwrap();
        let ops = rs.match_request(&input("http", "example.com", "/"));
        assert_eq!(last_value(&ops, "host"), Some("2.2.2.2"));
    }
}
