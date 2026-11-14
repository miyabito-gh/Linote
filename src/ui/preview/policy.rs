//! プレビューの安全方針のうち、WebView2 を使わずに判定できる部分（純粋な関数）。
//!
//! - ナビゲーション: こちらが `NavigateToString` で始めた読み込みだけを通す。リンクの
//!   クリックは常に取り消し、`http`・`https`・`mailto` だけは利用者の操作のときに
//!   既定のブラウザー（メールソフト）に渡す（[`NavDecision::OpenExternal`]）。同じ文書内の
//!   `#` への移動は通す。
//! - 大きすぎる文書の案内、ランタイムがないときの案内文。

use crate::markdown::{classify_link, LinkPolicy, PreviewPalette};

/// `NavigationStarting` に対する判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavDecision {
    /// そのまま進める。
    Allow,
    /// 取り消す。
    Cancel,
    /// 取り消したうえで、既定のアプリで開く（`http`、`https`、`mailto` で利用者の操作のとき）。
    OpenExternal(String),
}

/// 既定のアプリに渡す URL の長さの上限。
const MAX_EXTERNAL_URL_LEN: usize = 2048;

/// ナビゲーションの判断。
///
/// - `uri`: 移動先。
/// - `ours`: こちらが `NavigateToString` を呼んだ直後の読み込みか。
/// - `current_source`: 現在の文書の `Source`（`data:text/html...`）。空なら不明。
/// - `user_initiated`: 利用者の操作（リンクのクリック）による移動か。
pub fn decide_navigation(
    uri: &str,
    ours: bool,
    current_source: &str,
    user_initiated: bool,
) -> NavDecision {
    let lower = uri.trim_start().to_ascii_lowercase();
    if ours && lower.starts_with("data:text/html") {
        return NavDecision::Allow;
    }
    if is_fragment_of_current(uri, current_source) {
        return NavDecision::Allow;
    }
    if !user_initiated {
        return NavDecision::Cancel;
    }
    match classify_link(uri) {
        LinkPolicy::External | LinkPolicy::Mail
            if uri.len() <= MAX_EXTERNAL_URL_LEN
                && !uri.chars().any(|c| c.is_control() || c == ' ' || c == '"') =>
        {
            NavDecision::OpenExternal(uri.to_owned())
        }
        _ => NavDecision::Cancel,
    }
}

/// `uri` が現在の文書の同じ文書内の移動（`<source>#...`）か。
fn is_fragment_of_current(uri: &str, current_source: &str) -> bool {
    if current_source.is_empty()
        || !current_source
            .to_ascii_lowercase()
            .starts_with("data:text/html")
    {
        return false;
    }
    let base = current_source.split('#').next().unwrap_or("");
    match uri.strip_prefix(base) {
        Some(rest) => rest.is_empty() || rest.starts_with('#'),
        None => false,
    }
}

/// `NavigateToString` に渡せる大きさか（WebView2 の上限は 2 MB。余裕を見る）。
pub const MAX_NAVIGATE_BYTES: usize = 1_800_000;

pub fn fits_navigate_to_string(html: &str) -> bool {
    html.len() <= MAX_NAVIGATE_BYTES
}

/// 大きすぎて表示できないときに出す文書（本文の写しは入れない）。
pub fn oversize_notice_html(palette: &PreviewPalette, bytes: usize) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\
<style>body{{font-family:'Segoe UI','Yu Gothic UI',sans-serif;margin:24px;background:{bg};color:{fg}}}</style>\
</head><body><p>この文書は大きすぎてプレビューできません（約 {mb:.1} MB）。編集は続けられます。</p></body></html>",
        bg = palette.background.to_css(),
        fg = palette.foreground.to_css(),
        mb = bytes as f64 / 1_000_000.0,
    )
}

/// ランタイムがないとき（または初期化に失敗したとき）にペインへ表示する案内。
pub fn guidance_text(reason: &str) -> String {
    format!(
        "Markdown のプレビューには Microsoft Edge WebView2 ランタイムが必要です。\n\n\
         Windows 11 には通常含まれています。見つからない場合は Microsoft の配布ページ\n\
         （「WebView2 Runtime」で検索）から Evergreen 版をインストールし、\n\
         もう一度プレビューを開いてください。\n\n\
         編集はそのまま続けられます。\n\n\
         詳細: {reason}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "data:text/html;charset=utf-8,%3Chtml%3E";

    #[test]
    fn our_navigation_is_allowed_once_flagged() {
        let d = decide_navigation("data:text/html;charset=utf-8,abc", true, "", false);
        assert_eq!(d, NavDecision::Allow);
        // こちらが始めていない data: は通さない。
        let d = decide_navigation("data:text/html,abc", false, SRC, true);
        assert_eq!(d, NavDecision::Cancel);
    }

    #[test]
    fn fragments_of_current_document_are_allowed() {
        assert_eq!(
            decide_navigation(&format!("{SRC}#sec"), false, SRC, true),
            NavDecision::Allow
        );
        assert_eq!(
            decide_navigation(&format!("{SRC}x"), false, SRC, true),
            NavDecision::Cancel
        );
        assert_eq!(
            decide_navigation("about:blank#x", false, SRC, true),
            NavDecision::Cancel
        );
        // 現在の文書が不明なら通さない。
        assert_eq!(
            decide_navigation("data:text/html,x#a", false, "", true),
            NavDecision::Cancel
        );
    }

    #[test]
    fn user_links_open_externally_only_for_safe_schemes() {
        for u in [
            "https://example.com/a?b=c",
            "http://example.com/",
            "mailto:a@b.c",
        ] {
            assert_eq!(
                decide_navigation(u, false, SRC, true),
                NavDecision::OpenExternal(u.to_owned())
            );
        }
        for u in [
            "javascript:alert(1)",
            "file:///C:/Windows/notepad.exe",
            "ms-msdt:/x",
            "ftp://a/b",
            "https://a b/",
            "https://a/\"b",
            "\\\\server\\share",
            "data:text/html,<script>",
        ] {
            assert_eq!(
                decide_navigation(u, false, SRC, true),
                NavDecision::Cancel,
                "{u}"
            );
        }
    }

    #[test]
    fn script_initiated_navigation_is_cancelled() {
        assert_eq!(
            decide_navigation("https://example.com/", false, SRC, false),
            NavDecision::Cancel
        );
    }

    #[test]
    fn overlong_external_url_is_cancelled() {
        let u = format!("https://example.com/{}", "a".repeat(3000));
        assert_eq!(decide_navigation(&u, false, SRC, true), NavDecision::Cancel);
    }

    #[test]
    fn size_limit() {
        assert!(fits_navigate_to_string("abc"));
        assert!(!fits_navigate_to_string(
            &"a".repeat(MAX_NAVIGATE_BYTES + 1)
        ));
        let n = oversize_notice_html(&PreviewPalette::dark(), 3_500_000);
        assert!(n.contains("3.5 MB"));
        assert!(n.contains("default-src 'none'"));
    }

    #[test]
    fn guidance_mentions_runtime_and_reason() {
        let g = guidance_text("見つかりません");
        assert!(g.contains("WebView2"));
        assert!(g.contains("見つかりません"));
    }
}
