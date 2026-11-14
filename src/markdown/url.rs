//! リンク先 URL の安全性の判定。
//!
//! 許可するのは明示した安全なスキーム（`http`、`https`、`mailto`）と、
//! 同じ文書内の位置（`#...`）だけ。`javascript:`、`vbscript:`、`data:`、`file:` などの
//! スキーム、相対パス、スキームを省略した `//host` は許可しない。
//!
//! ブラウザーは URL の前後の空白・制御文字と、途中のタブ・改行を無視してスキームを
//! 解釈する（`java\tscript:` が `javascript:` になる）。判定はそれらを取り除いた形で行う。
//! 文字参照（`&#58;` など）は `pulldown-cmark` が解析時に展開済みなので、ここに届く
//! 文字列には実際の `:` が入っている。

/// リンク先の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPolicy {
    /// 外部の Web ページ（`http`、`https`）。クリックで開く処理は表示側（WebView2 の
    /// ナビゲーションの横取り）が決める。
    External,
    /// メールアドレス（`mailto`）。
    Mail,
    /// 同じ文書内の位置（`#見出し` や脚注）。
    Fragment,
    /// 許可しない。リンクにせず文字として表示する。
    Blocked,
}

impl LinkPolicy {
    pub fn is_allowed(self) -> bool {
        !matches!(self, LinkPolicy::Blocked)
    }
}

/// リンク先 URL を分類する。
pub fn classify_link(url: &str) -> LinkPolicy {
    // 前後の空白・制御文字（C0、DEL、空白）を除き、途中のタブ・改行も除く。
    let normalized: String = url
        .trim_matches(|c: char| c <= ' ' || c == '\u{7f}')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    if normalized.starts_with('#') {
        return LinkPolicy::Fragment;
    }
    match scheme_of(&normalized) {
        Some(s) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https") => {
            // `http:foo` のような権限部のない形は相対扱いになりうるので許可しない。
            let rest = &normalized[s.len() + 1..];
            if rest.starts_with("//") && rest.len() > 2 {
                LinkPolicy::External
            } else {
                LinkPolicy::Blocked
            }
        }
        Some(s) if s.eq_ignore_ascii_case("mailto") => LinkPolicy::Mail,
        // 他のスキーム、相対パス、`//host`、空の URL。
        _ => LinkPolicy::Blocked,
    }
}

/// 画像の URL のうち、リンクとして示してよいもの（`http`、`https`）か。
/// 画像そのものは読み込まない。
pub fn is_external_http(url: &str) -> bool {
    classify_link(url) == LinkPolicy::External
}

/// URL のスキーム（`:` より前）。スキームの文法（英字で始まり英数字と `+-.`）に
/// 合わなければ `None`（相対 URL）。
fn scheme_of(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        Some(scheme)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_web_mail_and_fragments() {
        assert_eq!(
            classify_link("https://example.com/a?b#c"),
            LinkPolicy::External
        );
        assert_eq!(classify_link("HTTP://example.com"), LinkPolicy::External);
        assert_eq!(
            classify_link("  https://example.com  "),
            LinkPolicy::External
        );
        assert_eq!(classify_link("mailto:a@example.com"), LinkPolicy::Mail);
        assert_eq!(classify_link("#heading"), LinkPolicy::Fragment);
    }

    #[test]
    fn blocks_dangerous_and_obfuscated_schemes() {
        for url in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            " javascript:alert(1)",
            "\u{1}javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///C:/Windows/win.ini",
            "ms-settings:privacy",
            "about:blank",
            "http:relative",
            "https://",
        ] {
            assert_eq!(classify_link(url), LinkPolicy::Blocked, "{url:?}");
        }
    }

    #[test]
    fn blocks_relative_and_scheme_relative_urls() {
        for url in [
            "",
            "foo.md",
            "./a/b.html",
            "../x",
            "/abs",
            "//evil.example/x",
            "?q",
        ] {
            assert_eq!(classify_link(url), LinkPolicy::Blocked, "{url:?}");
        }
    }

    #[test]
    fn image_links_only_for_http() {
        assert!(is_external_http("https://example.com/a.png"));
        assert!(!is_external_http("data:image/png;base64,AAAA"));
        assert!(!is_external_http("C:\\pictures\\a.png"));
    }
}
