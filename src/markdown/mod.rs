//! Markdown から HTML への変換（タスク G の担当。`src/markdown/` 配下）。
//!
//! 責務: `pulldown-cmark` による解析と HTML の生成（表、チェックリスト、取り消し線、
//! 脚注、コードブロック）、プレビュー用のスタイル（明暗テーマ）、埋め込み HTML・
//! スクリプト・外部リソースの無効化。GUI に依存せず、`windows` crate を使わない。
//! 表示は `ui::preview`（タスク H）。
//!
//! 取り決め:
//! - 入力は本文の写し（UTF-8）と [`crate::core::DocStamp`]。変換は
//!   [`crate::core::WorkerPool`] で行い（[`spawn_render`]）、結果に同じ印を付けて返す。
//!   UI は反映前に [`PreviewScheduler::accept`] で照合し、編集や閉鎖で古くなった結果を捨てる。
//! - 変換は入力が落ち着いてから始める（待ち時間は UI のタイマー、予定の管理は
//!   [`PreviewScheduler`]）。変換中に次の版が来たら [`crate::core::CancelToken`] で中止する。
//! - 埋め込み HTML は文字として表示する（エスケープ）。スクリプト、イベント属性、
//!   外部 URL の自動読み込み（画像を含む）を出力しない。リンクは `http`、`https`、
//!   `mailto`、文書内の `#` だけを許す（[`classify_link`]）。出力する HTML には
//!   外部リソースを禁じる Content-Security-Policy（[`CONTENT_SECURITY_POLICY`]）を付ける。
//! - 配色は `ui` に依存せず、色の値（[`PreviewPalette`]）で受け取る。
//! - 右ペインと別ウィンドウは同じ変換結果（[`PreviewUpdate`] の `Arc<str>`）と
//!   スタイル（[`Stylesheet`]）を使い、[`compose_document`] で同じ文書を作る。
//! - 初めてプレビューを開くまで何も初期化しない（このモジュールは静的な状態を持たない）。
//!
//! 使い方の流れ:
//!
//! ```text
//! 編集 → scheduler.note_change(stamp, len, now) → SetTimer(next_due)
//! タイマー → scheduler.take_due(now) → spawn_render(pool, ticket, 本文の写し, …)
//! 結果のメッセージ → scheduler.accept(update, 現在の版) → compose_document(body, &sheet, title)
//!                 → 右ペインと別ウィンドウの WebView2 に NavigateToString
//! ```

mod render;
mod schedule;
mod style;
mod url;

pub use render::{render_body, render_body_with, RenderOptions, DEFAULT_MAX_INPUT_BYTES};
pub use schedule::{spawn_render, PreviewScheduler, PreviewUpdate, RenderTicket};
pub use style::{
    compose_document, PreviewPalette, PreviewStyle, Rgb, Stylesheet, CONTENT_SECURITY_POLICY,
};
pub use url::{classify_link, LinkPolicy};

/// 変換できなかったときなどに、本文の代わりに出す 1 段落（`<body>` の中身）。`message` は
/// 文字として出す（エスケープする）。
pub fn notice_body(message: &str) -> String {
    let mut out = String::from("<p>");
    render::escape_attr(&mut out, message);
    out.push_str("</p>\n");
    out
}

use crate::core::{CancelToken, Limits, Result};

/// Markdown を、表示用の完全な HTML 文書に変換する（同期。短い文書やテスト向け）。
pub fn render_document(
    markdown: &str,
    stylesheet: &Stylesheet,
    title: &str,
    cancel: &CancelToken,
    limits: Limits,
) -> Result<String> {
    let body = render_body(markdown, cancel, limits)?;
    Ok(compose_document(&body, stylesheet, title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_body_escapes_the_message() {
        let b = notice_body("a <b>&\"x\"");
        assert_eq!(b, "<p>a &lt;b&gt;&amp;&quot;x&quot;</p>\n");
    }

    #[test]
    fn full_document_is_safe() {
        let md = "# T\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1)) ![i](https://e.example/i.png)\n";
        let doc = render_document(
            md,
            &Stylesheet::default(),
            "t",
            &CancelToken::new(),
            Limits::UNLIMITED,
        )
        .unwrap();
        assert!(doc.contains("Content-Security-Policy"));
        assert!(!doc.contains("<script"));
        assert!(!doc.contains("<img"));
        assert!(!doc.contains("href=\"javascript"));
        assert!(doc.contains("<h1>T</h1>"));
    }

    #[test]
    fn large_document_renders() {
        let unit = "## 見出し\n\n本文 *強調* `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] 済み\n\n```\ncode\n```\n\n";
        let md = unit.repeat(20_000); // 約 2 MiB
        let body = render_body(&md, &CancelToken::new(), Limits::UNLIMITED).unwrap();
        assert!(body.len() > md.len());
        assert_eq!(body.matches("<table>").count(), 20_000);
    }
}
