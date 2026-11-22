//! Markdown の解析と、安全な HTML 断片（`<body>` の中身）の生成。
//!
//! `pulldown-cmark` のイベント列を、HTML に書き出す前に次のように置き換える。
//!
//! - 生の HTML（ブロック・インライン）は文字として出す（エスケープされる）。
//!   ブロックは `<pre class="raw-html">` に入れて、書かれたとおりに見せる。
//! - リンクは [`classify_link`] が許可したものだけ `<a>` にする。それ以外は
//!   `<span class="link-blocked">` で文字として出す（URL は `title` に入れて見せる）。
//! - 画像は読み込まない。`<img>` を出さず、代替テキストを `<span class="image">` で示し、
//!   `http`／`https` の画像ならその URL へのリンクにする。
//! - 数式、メタデータ、見出しの属性、Wiki リンクなどの拡張は有効にしない。
//!
//! 書き出しは `pulldown-cmark` の HTML 出力を使う。文字（`Text`、`Code`）、
//! 属性の値（リンクの `title`、言語名、脚注名）はそこでエスケープされる。

use std::fmt::Write as _;
use std::ops::Range;

use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

use super::url::{classify_link, is_external_http, LinkPolicy};
use crate::core::{CancelToken, Error, Limits, Result};

/// 入力の既定の上限（バイト）。これより大きい文書はプレビューしない。
/// `pulldown-cmark` の最初の段階（ブロックの解析）は途中で中止できないので、
/// 上限で処理時間を抑える。[`Limits::max_bytes`] で上書きできる。
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

/// 中止を確かめる間隔（イベント数）。
const CANCEL_CHECK_INTERVAL: u32 = 1024;

/// 有効にする Markdown の拡張。
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

/// 変換の選択（設定の `preview` から作る）。既定は CommonMark どおり。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOptions {
    /// 段落の中の改行をそのまま改行（`<br>`）にする。オフなら CommonMark どおり
    /// 空白につなげる（改行にするには行末に空白 2 つか `\`）。
    pub line_breaks: bool,
    /// 見出しに `id`（`h-1`, `h-2`…）を付け、先頭に見出しの目次（`<nav class="toc">`）を置く。
    pub toc: bool,
    /// 最上位のブロックの前に、元の行番号の目印（`<span class="src-line" data-line="N">`）を
    /// 置く（編集位置に合わせたスクロール用。N は 0 始まり）。
    pub source_lines: bool,
}

/// Markdown を HTML 断片（`<body>` の中身）に変換する（既定の選択）。
///
/// - 入力が上限（`limits.max_bytes`、なければ [`DEFAULT_MAX_INPUT_BYTES`]）を超えたら
///   [`Error::LimitExceeded`]。
/// - `cancel` が合図されたら [`Error::Cancelled`]。
pub fn render_body(markdown: &str, cancel: &CancelToken, limits: Limits) -> Result<String> {
    render_body_with(markdown, cancel, limits, RenderOptions::default())
}

/// [`render_body`] の選択つき。
pub fn render_body_with(
    markdown: &str,
    cancel: &CancelToken,
    limits: Limits,
    render_options: RenderOptions,
) -> Result<String> {
    let limit = limits.max_bytes.unwrap_or(DEFAULT_MAX_INPUT_BYTES);
    if markdown.len() as u64 > limit {
        return Err(Error::LimitExceeded {
            what: "プレビューする文書の大きさ".into(),
            limit,
        });
    }
    cancel.check()?;
    let parser = Parser::new_ext(markdown, options()).into_offset_iter();
    cancel.check()?;
    let mut events = SafeEvents::new(parser, cancel);
    events.line_breaks = render_options.line_breaks;
    events.collect_toc = render_options.toc;
    if render_options.source_lines {
        // 行番号は、改行（LF）の位置から二分探索で引く。
        events.newlines = Some(
            markdown
                .bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == 0x0a).then_some(i))
                .collect(),
        );
    }
    // 出力はおおむね入力より少し大きい。
    let mut out = String::with_capacity(markdown.len() + markdown.len() / 2 + 64);
    html::push_html(&mut out, &mut events);
    if events.cancelled {
        return Err(Error::Cancelled);
    }
    if render_options.toc && !events.toc.is_empty() {
        out.insert_str(0, &toc_html(&events.toc));
    }
    Ok(out)
}

/// 見出しの目次の HTML。項目は（レベル, 文字）で、リンク先は見出しの `h-N`（N は 1 から）。
fn toc_html(headings: &[(u8, usize, String)]) -> String {
    let mut s = String::from("<nav class=\"toc\"><ul>");
    for (level, n, text) in headings {
        let _ = write!(s, "<li class=\"toc-l{level}\"><a href=\"#h-{n}\">");
        escape_attr(&mut s, text);
        s.push_str("</a></li>");
    }
    s.push_str(
        "</ul></nav>
",
    );
    s
}

/// 画像を閉じるときに出す終わりのタグ。
#[derive(Clone, Copy)]
enum ImageClose {
    Span,
    SpanAndAnchor,
}

/// イベント列を安全な形に置き換える反復子。
struct SafeEvents<'a, 'c, I> {
    inner: I,
    cancel: &'c CancelToken,
    cancelled: bool,
    counter: u32,
    /// 置き換えで 1 つのイベントが複数になったときの残り（後ろから取り出す）。
    queue: Vec<Event<'a>>,
    /// 開いているリンクごとに、`<a>` として出したか。
    links: Vec<bool>,
    /// 開いている画像ごとの閉じ方。
    images: Vec<ImageClose>,
    /// 段落の中の改行を `<br>` にする（[`RenderOptions::line_breaks`]）。
    line_breaks: bool,
    /// 見出しを集めて `id` を付ける（[`RenderOptions::toc`]）。
    collect_toc: bool,
    /// 集めた見出し（レベル, 番号, 文字）。
    toc: Vec<(u8, usize, String)>,
    /// 集めている最中の見出し（レベル, 番号, 文字）。
    heading: Option<(u8, usize, String)>,
    /// 見出しに振った番号（`h-N` の N）。
    heading_seq: usize,
    /// 行番号の目印を出すときの、LF の位置の表（[`RenderOptions::source_lines`]）。
    newlines: Option<Vec<usize>>,
    /// 入れ子の深さ（最上位のブロックの前かどうかの判定）。
    depth: usize,
}

impl<'a, 'c, I: Iterator<Item = (Event<'a>, Range<usize>)>> SafeEvents<'a, 'c, I> {
    fn new(inner: I, cancel: &'c CancelToken) -> Self {
        Self {
            inner,
            cancel,
            cancelled: false,
            counter: 0,
            queue: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
            line_breaks: false,
            collect_toc: false,
            toc: Vec::new(),
            heading: None,
            heading_seq: 0,
            newlines: None,
            depth: 0,
        }
    }

    /// `<a>` を出力中か（入れ子の `<a>` を作らないため）。
    fn inside_anchor(&self) -> bool {
        self.links.iter().any(|&a| a)
            || self
                .images
                .iter()
                .any(|c| matches!(c, ImageClose::SpanAndAnchor))
    }

    fn transform(&mut self, event: Event<'a>) -> Event<'a> {
        match event {
            Event::SoftBreak if self.line_breaks => Event::HardBreak,
            // 生の HTML は文字として出す。
            Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
            Event::Start(Tag::HtmlBlock) => markup("<pre class=\"raw-html\"><code>"),
            Event::End(TagEnd::HtmlBlock) => markup("</code></pre>\n"),
            // 数式は有効にしていないが、念のため文字として出す。
            Event::InlineMath(text) | Event::DisplayMath(text) => Event::Code(text),

            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let policy = if link_type == LinkType::Email {
                    // `<user@example.com>`。HTML 出力が `mailto:` を付ける。
                    LinkPolicy::Mail
                } else {
                    classify_link(&dest_url)
                };
                if policy.is_allowed() && !self.inside_anchor() {
                    self.links.push(true);
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    })
                } else {
                    self.links.push(false);
                    let mut s = String::from("<span class=\"link-blocked\" title=\"");
                    escape_attr(&mut s, &dest_url);
                    s.push_str("\">");
                    markup_owned(s)
                }
            }
            Event::End(TagEnd::Link) => {
                if self.links.pop().unwrap_or(false) {
                    Event::End(TagEnd::Link)
                } else {
                    markup("</span>")
                }
            }

            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                let mut s = String::new();
                let close = if is_external_http(&dest_url) && !self.inside_anchor() {
                    s.push_str("<a class=\"image-link\" href=\"");
                    escape_attr(&mut s, dest_url.trim());
                    s.push_str("\">");
                    ImageClose::SpanAndAnchor
                } else {
                    ImageClose::Span
                };
                s.push_str("<span class=\"image\" title=\"");
                if title.is_empty() {
                    escape_attr(&mut s, &dest_url);
                } else {
                    escape_attr(&mut s, &title);
                }
                s.push_str("\">");
                self.images.push(close);
                markup_owned(s)
            }
            Event::End(TagEnd::Image) => match self.images.pop() {
                Some(ImageClose::SpanAndAnchor) => markup("</span></a>"),
                _ => markup("</span>"),
            },

            // 見出しの属性は有効にしていないが、届いても捨てる。目次では `h-N` を付ける。
            Event::Start(Tag::Heading { level, .. }) => {
                let id = if self.collect_toc {
                    self.heading_seq += 1;
                    self.heading = Some((level as u8, self.heading_seq, String::new()));
                    Some(CowStr::from(format!("h-{}", self.heading_seq)))
                } else {
                    None
                };
                Event::Start(Tag::Heading {
                    level,
                    id,
                    classes: Vec::new(),
                    attrs: Vec::new(),
                })
            }
            Event::End(TagEnd::Heading(level)) => {
                if let Some((lv, n, text)) = self.heading.take() {
                    let text = text.trim().to_owned();
                    if !text.is_empty() {
                        self.toc.push((lv, n, text));
                    }
                }
                Event::End(TagEnd::Heading(level))
            }
            // コードブロックの言語名は `class` に入る（エスケープされる）。
            // 空白以降（`rust,ignore` の後の情報など）は捨てる。
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let lang = info
                    .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang.into())))
            }
            // メタデータは有効にしていない。届いたら何も出さない（中身は Text として
            // 来るが、開始・終了を出さなければ段落の外の文字になるだけで安全）。
            Event::Start(Tag::MetadataBlock(_)) | Event::End(TagEnd::MetadataBlock(_)) => {
                markup("")
            }
            other => other,
        }
    }
}

impl<'a, I: Iterator<Item = (Event<'a>, Range<usize>)>> Iterator for SafeEvents<'a, '_, I> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if let Some(e) = self.queue.pop() {
            return Some(e);
        }
        if self.cancelled {
            return None;
        }
        self.counter += 1;
        if self.counter >= CANCEL_CHECK_INTERVAL {
            self.counter = 0;
            if self.cancel.is_cancelled() {
                self.cancelled = true;
                return None;
            }
        }
        let (event, range) = self.inner.next()?;
        // 最上位のブロックの前に、行番号の目印を置く。
        let marker = match (&self.newlines, self.depth, &event) {
            (Some(newlines), 0, Event::Start(_) | Event::Rule) => {
                let line = newlines.partition_point(|&p| p < range.start);
                Some(format!(
                    "<span class=\"src-line\" data-line=\"{line}\"></span>"
                ))
            }
            _ => None,
        };
        match &event {
            Event::Start(_) => self.depth += 1,
            Event::End(_) => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        let out = self.transform(event);
        if let Some((_, _, text)) = self.heading.as_mut() {
            match &out {
                Event::Text(t) | Event::Code(t) => text.push_str(t),
                Event::SoftBreak | Event::HardBreak => text.push(' '),
                _ => {}
            }
        }
        match marker {
            Some(m) => {
                self.queue.push(out);
                Some(markup_owned(m))
            }
            None => Some(out),
        }
    }
}

/// 自分で組み立てた HTML を出すイベント。中に利用者の文字を入れるときは必ず
/// [`escape_attr`] を通す。
fn markup(s: &'static str) -> Event<'static> {
    Event::InlineHtml(CowStr::Borrowed(s))
}

fn markup_owned<'a>(s: String) -> Event<'a> {
    Event::InlineHtml(CowStr::Boxed(s.into_boxed_str()))
}

/// 属性の値・本文のどちらに置いても安全なエスケープ。
pub(crate) fn escape_attr(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // 制御文字は見えないので番号で示す（NUL は置き換え文字にする）。
            '\0' => out.push('\u{fffd}'),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {
                let _ = write!(out, "&#{};", c as u32);
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(md: &str) -> String {
        render_body(md, &CancelToken::new(), Limits::UNLIMITED).unwrap()
    }

    #[test]
    fn line_breaks_option_turns_soft_breaks_into_br() {
        let md = "一行目\n二行目\n\n```\na\nb\n```\n";
        // 既定は CommonMark どおり（改行は空白につながる）。
        let plain = render(md);
        assert!(plain.contains("<p>一行目\n二行目</p>"), "{plain}");
        let opts = RenderOptions {
            line_breaks: true,
            ..RenderOptions::default()
        };
        let html = render_body_with(md, &CancelToken::new(), Limits::UNLIMITED, opts).unwrap();
        assert!(html.contains("<p>一行目<br />\n二行目</p>"), "{html}");
        // コードブロックの中は変えない。
        assert!(html.contains("<code>a\nb\n</code>"), "{html}");
    }

    fn render_with(md: &str, opts: RenderOptions) -> String {
        render_body_with(md, &CancelToken::new(), Limits::UNLIMITED, opts).unwrap()
    }

    #[test]
    fn toc_adds_ids_and_escapes_heading_text() {
        let md = "# A <b> & `c`

text

## 二つ目

###
";
        let opts = RenderOptions {
            toc: true,
            ..RenderOptions::default()
        };
        let html = render_with(md, opts);
        assert!(html.starts_with("<nav class=\"toc\"><ul>"), "{html}");
        assert!(html.contains("<h1 id=\"h-1\">"), "{html}");
        assert!(html.contains("<h2 id=\"h-2\">"), "{html}");
        assert!(
            html.contains("<li class=\"toc-l1\"><a href=\"#h-1\">A &lt;b&gt; &amp; c</a></li>"),
            "{html}"
        );
        assert!(html.contains("<li class=\"toc-l2\"><a href=\"#h-2\">二つ目</a>"));
        // 空の見出しは目次に載せない。
        assert_eq!(html.matches("<li ").count(), 2, "{html}");
        // 見出しがなければ目次も出さない。目次をオフなら id も付けない。
        assert!(!render_with(
            "text
", opts
        )
        .contains("toc"));
        assert!(!render(md).contains("id=\"h-"));
    }

    #[test]
    fn source_line_markers_only_before_top_level_blocks() {
        let md = "# T

para
more

- a
- b

> q

```
code
```

---
";
        let opts = RenderOptions {
            source_lines: true,
            ..RenderOptions::default()
        };
        let html = render_with(md, opts);
        let lines: Vec<&str> = html
            .split("data-line=\"")
            .skip(1)
            .map(|r| r.split('"').next().unwrap())
            .collect();
        assert_eq!(lines, ["0", "2", "5", "8", "10", "14"], "{html}");
        assert!(html.starts_with("<span class=\"src-line\" data-line=\"0\"></span>"));
        assert!(html.contains("</span>\n<h1>T</h1>"), "{html}");
        assert!(!render(md).contains("src-line"));
    }

    #[test]
    fn headings_paragraphs_and_emphasis() {
        let html = render("# 見出し\n\n段落の *強調* と **太字** と `code`。\n");
        assert!(html.contains("<h1>見出し</h1>"), "{html}");
        assert!(
            html.contains(
                "<p>段落の <em>強調</em> と <strong>太字</strong> と <code>code</code>。</p>"
            ),
            "{html}"
        );
    }

    #[test]
    fn tables() {
        let html = render("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        assert!(html.contains("<table>"), "{html}");
        assert!(
            html.contains("<th style=\"text-align: left\">a</th>"),
            "{html}"
        );
        assert!(
            html.contains("<td style=\"text-align: right\">2</td>"),
            "{html}"
        );
    }

    #[test]
    fn task_lists() {
        let html = render("- [ ] todo\n- [x] done\n");
        assert!(
            html.contains("<input disabled=\"\" type=\"checkbox\"/>"),
            "{html}"
        );
        assert!(
            html.contains("<input disabled=\"\" type=\"checkbox\" checked=\"\"/>"),
            "{html}"
        );
    }

    #[test]
    fn strikethrough() {
        assert!(render("~~消す~~").contains("<del>消す</del>"));
    }

    #[test]
    fn footnotes() {
        let html = render("本文[^1]\n\n[^1]: 注釈\n");
        assert!(html.contains("class=\"footnote-reference\""), "{html}");
        assert!(html.contains("<a href=\"#1\">1</a>"), "{html}");
        assert!(html.contains("class=\"footnote-definition\""), "{html}");
        assert!(html.contains("注釈"), "{html}");
    }

    #[test]
    fn code_blocks_escape_content_and_keep_language() {
        let html = render("```rust ignore\nfn main() { \"<b>\" }\n```\n\n    indented <i>\n");
        assert!(
            html.contains("<pre><code class=\"language-rust\">"),
            "{html}"
        );
        assert!(html.contains("&lt;b&gt;"), "{html}");
        assert!(html.contains("indented &lt;i&gt;"), "{html}");
    }

    #[test]
    fn hostile_code_language_is_escaped() {
        let html = render("```\"><script>x\n1\n```\n");
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn block_html_is_shown_as_text() {
        let html = render("<script>alert(1)</script>\n\n<div onclick=\"x()\">hi</div>\n");
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<div"), "{html}");
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
        assert!(html.contains("<pre class=\"raw-html\"><code>"), "{html}");
    }

    #[test]
    fn inline_html_is_shown_as_text() {
        let html = render("a <img src=x onerror=alert(1)> b <iframe src=\"https://e.x\"></iframe>");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("<iframe"), "{html}");
        assert!(
            html.contains("&lt;img src=x onerror=alert(1)&gt;"),
            "{html}"
        );
    }

    #[test]
    fn html_comments_and_cdata_are_text() {
        let html = render("x <!-- c --> y\n\n<![CDATA[z]]>\n");
        assert!(!html.contains("<!--"), "{html}");
        assert!(!html.contains("<![CDATA["), "{html}");
    }

    #[test]
    fn safe_links_are_kept() {
        let html = render("[web](https://example.com/?a=1&b=\"2\" \"t\") <https://x.example> <me@example.com> [top](#top)");
        assert!(
            html.contains("<a href=\"https://example.com/?a=1&amp;b=%222%22\" title=\"t\">web</a>"),
            "{html}"
        );
        assert!(html.contains("<a href=\"https://x.example\">"), "{html}");
        assert!(
            html.contains("<a href=\"mailto:me@example.com\">"),
            "{html}"
        );
        assert!(html.contains("<a href=\"#top\">top</a>"), "{html}");
    }

    #[test]
    fn dangerous_links_are_disabled() {
        let html = render(
            "[a](javascript:alert(1)) [b](JAVASCRIPT:x) [c](<java\tscript:x>) \
             [d](javascript&#58;alert(1)) [e](data:text/html,x) [f](vbscript:x) \
             [g](file:///C:/x) <javascript:alert(1)> [h](relative.md)\n\n\
             [r]: javascript:alert(2)\n\n[ref][r]\n",
        );
        assert!(!html.contains("<a "), "{html}");
        assert!(!html.contains("href"), "{html}");
        assert!(
            html.contains("<span class=\"link-blocked\" title=\"javascript:alert(1)\">a</span>"),
            "{html}"
        );
        assert!(html.contains(">ref</span>"), "{html}");
    }

    #[test]
    fn images_are_not_loaded() {
        let html = render("![猫の写真](https://example.com/cat.png \"猫\") ![local](C:/a.png) ![x](javascript:alert(1))");
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("<a class=\"image-link\" href=\"https://example.com/cat.png\"><span class=\"image\" title=\"猫\">猫の写真</span></a>"), "{html}");
        assert!(
            html.contains("<span class=\"image\" title=\"C:/a.png\">local</span>"),
            "{html}"
        );
        assert!(!html.contains("href=\"javascript"), "{html}");
    }

    #[test]
    fn image_inside_link_does_not_nest_anchors() {
        let html = render("[![alt](https://e.example/i.png)](https://e.example/)");
        assert_eq!(html.matches("<a ").count(), 1, "{html}");
        assert_eq!(html.matches("</a>").count(), 1, "{html}");
    }

    #[test]
    fn attributes_cannot_be_broken_out_of() {
        let html = render("[x](https://e.example/\"onmouseover=\"alert(1) \"\\\"><script>\")");
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("\"onmouseover"), "{html}");
        let html = render("![a](<foo\" onerror=\"x>)");
        assert!(!html.contains("\" onerror"), "{html}");
    }

    #[test]
    fn heading_attribute_syntax_is_plain_text() {
        let html = render("# T {#id .cls onclick=x}\n");
        assert!(
            !html.contains("onclick=\"") && !html.contains("id=\"id\""),
            "{html}"
        );
    }

    #[test]
    fn alerts_render_as_blockquotes() {
        let html = render("> [!NOTE]\n> 注意\n");
        assert!(
            html.contains("<blockquote class=\"markdown-alert-note\">"),
            "{html}"
        );
    }

    #[test]
    fn size_limit() {
        let err = render_body(
            "abcdef",
            &CancelToken::new(),
            Limits {
                max_items: None,
                max_bytes: Some(3),
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "limit_exceeded");
    }

    #[test]
    fn cancellation_stops_rendering() {
        let token = CancelToken::new();
        token.cancel();
        let err = render_body("# a", &token, Limits::UNLIMITED).unwrap_err();
        assert!(err.is_cancelled());

        // 変換の途中で中止される場合（反復子の中での確認）。
        let md = "- item\n".repeat(10_000);
        let token = CancelToken::new();
        let parser = Parser::new_ext(&md, options()).into_offset_iter();
        let mut events = SafeEvents::new(parser, &token);
        for _ in 0..10 {
            events.next();
        }
        token.cancel();
        let rest = events.by_ref().count();
        assert!(events.cancelled);
        assert!(rest < CANCEL_CHECK_INTERVAL as usize);
    }

    #[test]
    fn escape_attr_escapes_everything_dangerous() {
        let mut s = String::new();
        escape_attr(&mut s, "<a href='x' \"y\">&\0\u{1}");
        assert_eq!(
            s,
            "&lt;a href=&#39;x&#39; &quot;y&quot;&gt;&amp;\u{fffd}&#1;"
        );
    }
}
