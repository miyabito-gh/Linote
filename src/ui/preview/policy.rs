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

/// `NavigateToString` に渡せる大きさ（UTF-8 のバイト数）。
///
/// WebView2 は文字列を `data:text/html;charset=utf-8;base64,…` の URL にして読み込み、その長さの
/// 上限は 2 MiB（2 097 152 文字）。base64 は 4/3 倍になるので、本文は約 1.57 MB（1 572 837 バイト）
/// までしか表示できない。これを超えると、エラーも案内も出ないまま空白（背景色だけ）になる。
/// 実機では 1.49 MB は表示でき、1.6 MB は空白だった。余裕を見て 1.5 MB（1 500 000 バイト）にする。
pub const MAX_NAVIGATE_BYTES: usize = 1_500_000;

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

/// 読み込みが失敗したとき（`NavigationCompleted` が失敗）に出す文書。空白のまま放置しない。
pub fn navigation_failed_notice_html(palette: &PreviewPalette) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\
<style>body{{font-family:'Segoe UI','Yu Gothic UI',sans-serif;margin:24px;background:{bg};color:{fg}}}</style>\
</head><body><p>プレビューを表示できませんでした。編集は続けられます。</p></body></html>",
        bg = palette.background.to_css(),
        fg = palette.foreground.to_css(),
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

/// 同じ文書の更新を、読み込み直さずにその場で差し替えるスクリプト（`ExecuteScript` で実行する）。
///
/// `NavigateToString` で読み込み直すと、毎回いったん空白になってから先頭に描かれ、位置を戻すので
/// 入力のたびにちらつく。ここでは新しい HTML を `DOMParser` で読み、`<head>` と `<body>` を
/// 入れ替えて、元のスクロール位置に戻す（同じスクリプトの中なので途中の状態は描かれない）。
///
/// 文書の内容は JSON の文字列リテラルとしてだけ渡す（コードとしては評価しない）。`DOMParser` で
/// 作った要素の `<script>` は動かず、元の文書の CSP（`script-src 'none'`）と
/// `IsScriptEnabled(false)` もそのまま効く。成功すれば `"ok"` を返す。
pub fn patch_script(html: &str) -> Option<String> {
    let literal = serde_json::to_string(html).ok()?;
    Some(format!(
        "(function(h){{try{{var x=window.scrollX,y=window.scrollY;\
         var d=new DOMParser().parseFromString(h,'text/html');\
         if(!d.head||!d.body||!document.head||!document.body)return 'no';\
         document.head.replaceWith(document.adoptNode(d.head));\
         document.body.replaceWith(document.adoptNode(d.body));\
         window.scrollTo(x,y);return 'ok';}}catch(e){{return 'no';}}}})({literal})"
    ))
}

/// 元の行（0 始まり）に対応する位置までスクロールするスクリプト（編集位置への追従）。
///
/// 目印（`.src-line`、[`crate::markdown::RenderOptions::source_lines`]）のうち `data-line` が
/// `line` 以下の最後のものを先頭に出す。なければ文書の先頭。固定の文字列に整数だけを入れる。
pub fn scroll_to_line_script(line: u32) -> String {
    format!(
        "(function(n){{var a=document.querySelectorAll('.src-line'),t=null;         for(var i=0;i<a.length;i++){{if(+a[i].getAttribute('data-line')<=n)t=a[i];else break;}}         if(t)t.scrollIntoView({{block:'start'}});else window.scrollTo(0,0);}})({line})"
    )
}

/// [`patch_script`] の結果（JSON）が成功か。
pub fn patch_succeeded(json: &str) -> bool {
    json.trim() == "\"ok\""
}

/// プレビューの上で押されたキーのうち、ブラウザーの機能として働かせないものか
/// （`AcceleratorKeyPressed` で `Handled` にして止める）。
///
/// ページ内検索（Ctrl+F、F3、Ctrl+G）を使えるようにブラウザーのショートカットを有効にするので、
/// 再読み込み・印刷・保存・ソース表示・開発者ツール・戻る/進む・キャレットブラウズなどは
/// ここで止める。Ctrl を押しながらのキーは、検索とコピー・全選択と移動だけを通す。
pub fn blocks_browser_key(vk: u32, ctrl: bool, alt: bool) -> bool {
    const VK_LEFT: u32 = 0x25;
    const VK_RIGHT: u32 = 0x27;
    const VK_HOME: u32 = 0x24;
    const VK_F5: u32 = 0x74;
    const VK_F7: u32 = 0x76;
    const VK_F12: u32 = 0x7B;
    // VK_BROWSER_BACK ..= VK_BROWSER_HOME
    if (0xA6..=0xAC).contains(&vk) {
        return true;
    }
    if matches!(vk, VK_F5 | VK_F7 | VK_F12) {
        return true;
    }
    if alt && !ctrl {
        // 戻る・進む・ホーム。Alt+F4 などは通す（ウィンドウの操作）。
        return matches!(vk, VK_LEFT | VK_RIGHT | VK_HOME);
    }
    if ctrl {
        const ALLOW: &[u32] = &[
            b'F' as u32, // 検索
            b'G' as u32, // 次を検索（Shift で前）
            b'C' as u32, // コピー
            b'A' as u32, // 全選択
            0x2D,        // Insert（Ctrl+Insert のコピー）
            0x21,        // PageUp
            0x22,        // PageDown
            0x23,        // End
            VK_HOME,
            VK_LEFT,
            0x26, // Up
            VK_RIGHT,
            0x28, // Down
            0x10, // Shift
            0x11, // Ctrl
            0x12, // Alt
        ];
        return !ALLOW.contains(&vk);
    }
    false
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
    fn patch_script_passes_document_only_as_string_literal() {
        let html = "<p>a</p>'); alert(1); ('\u{2028}</script>";
        let s = patch_script(html).unwrap();
        // 文書は JSON 文字列として最後の引数に 1 つだけ入る。
        let literal = serde_json::to_string(html).unwrap();
        assert!(s.ends_with(&format!("({literal})")));
        assert_eq!(s.matches(&literal).count(), 1);
        assert!(patch_succeeded("\"ok\""));
        assert!(!patch_succeeded("\"no\""));
        assert!(!patch_succeeded("null"));
    }

    #[test]
    fn browser_keys_except_find_and_copy_are_blocked() {
        // 検索・コピー・全選択・移動は通す。
        for vk in *b"FGCA" {
            assert!(!blocks_browser_key(vk as u32, true, false));
        }
        assert!(!blocks_browser_key(0x72, false, false)); // F3
        assert!(!blocks_browser_key(0x1B, false, false)); // Esc
        assert!(!blocks_browser_key(0x28, false, false)); // Down
        assert!(!blocks_browser_key(0x73, false, true)); // Alt+F4

        // 再読み込み・印刷・保存・ソース・開発者ツール・戻るは止める。
        for vk in *b"RPSUIJ" {
            assert!(blocks_browser_key(vk as u32, true, false));
        }
        assert!(blocks_browser_key(0x74, false, false)); // F5
        assert!(blocks_browser_key(0x7B, false, false)); // F12
        assert!(blocks_browser_key(0x25, false, true)); // Alt+Left
        assert!(blocks_browser_key(0xA6, false, false)); // BrowserBack
    }

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
        // base64 の data: URL が 2 MiB に収まること（これを超えると空白になる）。
        let url_chars =
            "data:text/html;charset=utf-8;base64,".len() + MAX_NAVIGATE_BYTES.div_ceil(3) * 4;
        assert!(url_chars < 2 * 1024 * 1024, "{url_chars}");
        assert!(fits_navigate_to_string("abc"));
        assert!(!fits_navigate_to_string(
            &"a".repeat(MAX_NAVIGATE_BYTES + 1)
        ));
        let n = oversize_notice_html(&PreviewPalette::dark(), 3_500_000);
        assert!(n.contains("3.5 MB"));
        assert!(n.contains("default-src 'none'"));
        let f = navigation_failed_notice_html(&PreviewPalette::dark());
        assert!(f.contains("表示できませんでした"));
        assert!(f.contains("default-src 'none'"));
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
    fn scroll_to_line_script_only_interpolates_the_number() {
        let a = scroll_to_line_script(7);
        let b = scroll_to_line_script(123456);
        assert!(a.ends_with("})(7)"));
        assert_eq!(a.replace("(7)", "(123456)"), b);
        assert!(!a.contains("http"));
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
