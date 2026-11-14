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

/// 更新前のスクロール位置を読むスクリプト（`ExecuteScript` で実行する）。
///
/// 文書のスクリプトは無効のまま（`IsScriptEnabled(false)`）。`ExecuteScript` は
/// アプリ側からの実行なので、その設定や文書の CSP に関係なく動く。実行するのはここの
/// 固定の文字列と [`scroll_restore_script`] だけで、文書の内容は入れない。
pub const SCROLL_CAPTURE_SCRIPT: &str = "[window.scrollX, window.scrollY]";

/// スクロール位置（CSS ピクセル）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollPos {
    pub x: f64,
    pub y: f64,
}

/// [`SCROLL_CAPTURE_SCRIPT`] の結果（JSON の `[x,y]`）を読む。読めなければ `None`。
pub fn parse_scroll_result(json: &str) -> Option<ScrollPos> {
    let inner = json.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inner.split(',');
    let x: f64 = parts.next()?.trim().parse().ok()?;
    let y: f64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some(ScrollPos {
        x: x.max(0.0),
        y: y.max(0.0),
    })
}

/// 新しい文書を表示したあとで、スクロール位置を戻すスクリプト。先頭なら `None`（不要）。
/// 文書が短くなっていれば、ブラウザーが末尾に収める。
pub fn scroll_restore_script(pos: ScrollPos) -> Option<String> {
    if pos.x < 1.0 && pos.y < 1.0 {
        return None;
    }
    Some(format!("window.scrollTo({:.1}, {:.1})", pos.x, pos.y))
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

    #[test]
    fn scroll_result_is_parsed_strictly() {
        assert_eq!(
            parse_scroll_result("[0,1234.5]"),
            Some(ScrollPos { x: 0.0, y: 1234.5 })
        );
        assert_eq!(
            parse_scroll_result(" [ 3 , 4 ] "),
            Some(ScrollPos { x: 3.0, y: 4.0 })
        );
        assert_eq!(
            parse_scroll_result("[-5,10]"),
            Some(ScrollPos { x: 0.0, y: 10.0 })
        );
        for bad in [
            "",
            "null",
            "\"0,1\"",
            "[1]",
            "[1,2,3]",
            "[a,1]",
            "[NaN,1]",
            "[1,Infinity]",
        ] {
            assert_eq!(parse_scroll_result(bad), None, "{bad}");
        }
    }

    #[test]
    fn restore_script_only_contains_numbers() {
        assert_eq!(scroll_restore_script(ScrollPos { x: 0.0, y: 0.4 }), None);
        assert_eq!(
            scroll_restore_script(ScrollPos { x: 0.0, y: 1234.56 }).as_deref(),
            Some("window.scrollTo(0.0, 1234.6)")
        );
    }
}
