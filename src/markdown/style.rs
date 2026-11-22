//! プレビューのスタイル（CSS）と、完全な HTML 文書の組み立て。
//!
//! 配色は `ui` のテーマ型に依存せず、色の値（[`Rgb`]）だけを [`PreviewPalette`] で
//! 受け取る（`ui::theme` が自分の配色からこれを作って渡す）。CSS はこれらの値だけから
//! 作る純粋な関数で、右ペインと別ウィンドウは同じ [`Stylesheet`] を共有する。

use std::sync::Arc;

use super::render::escape_attr;

/// Content-Security-Policy。スクリプト、外部リソース（画像・フォント・接続・フレーム）、
/// フォームの送信、`<base>` の書き換えをすべて禁じ、埋め込みのスタイルだけを許す。
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     script-src 'none'; img-src 'none'; font-src 'none'; connect-src 'none'; \
     media-src 'none'; object-src 'none'; frame-src 'none'; child-src 'none'; \
     worker-src 'none'; manifest-src 'none'; form-action 'none'; base-uri 'none'";

/// 色（sRGB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Win32 の `COLORREF`（`0x00BBGGRR`）から作る。
    pub const fn from_colorref(c: u32) -> Self {
        Self::new(c as u8, (c >> 8) as u8, (c >> 16) as u8)
    }

    /// CSS の `#rrggbb`。
    pub fn to_css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// プレビューの配色。編集欄のテーマと調和させるため、`ui::theme` が値を渡す。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewPalette {
    /// 暗い配色か（`color-scheme` とチェックボックスなどの既定部品の色に使う）。
    pub dark: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    /// 補助の文字（引用、脚注、画像の代替表示）。
    pub muted: Rgb,
    /// 罫線（表、見出しの下線、区切り線）。
    pub border: Rgb,
    /// コード・表の見出しの背景。
    pub surface: Rgb,
    pub link: Rgb,
    /// 引用の縦線などの強調色。
    pub accent: Rgb,
}

impl PreviewPalette {
    /// 既定の明るい配色。
    pub const fn light() -> Self {
        Self {
            dark: false,
            background: Rgb::new(0xff, 0xff, 0xff),
            foreground: Rgb::new(0x1f, 0x23, 0x28),
            muted: Rgb::new(0x59, 0x63, 0x6e),
            border: Rgb::new(0xd0, 0xd7, 0xde),
            surface: Rgb::new(0xf3, 0xf4, 0xf6),
            link: Rgb::new(0x09, 0x69, 0xda),
            accent: Rgb::new(0x8c, 0x95, 0x9f),
        }
    }

    /// 既定の暗い配色。
    pub const fn dark() -> Self {
        Self {
            dark: true,
            background: Rgb::new(0x1e, 0x1e, 0x1e),
            foreground: Rgb::new(0xd4, 0xd4, 0xd4),
            muted: Rgb::new(0x9d, 0xa5, 0xb0),
            border: Rgb::new(0x3c, 0x3f, 0x44),
            surface: Rgb::new(0x2a, 0x2d, 0x31),
            link: Rgb::new(0x4d, 0xa3, 0xff),
            accent: Rgb::new(0x5c, 0x63, 0x6b),
        }
    }
}

impl Default for PreviewPalette {
    fn default() -> Self {
        Self::light()
    }
}

/// プレビューの見た目の設定。色と文字の指定だけからなる値。
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewStyle {
    pub palette: PreviewPalette,
    /// 本文の書体名（優先順）。空なら既定（Segoe UI、Yu Gothic UI など）。
    pub body_fonts: Vec<String>,
    /// コードの書体名（優先順）。空なら既定（Cascadia Mono、Consolas など）。
    pub code_fonts: Vec<String>,
    /// 本文の文字の大きさ（CSS ピクセル）。
    pub font_size_px: f32,
    /// 本文の幅をウィンドウ幅いっぱいにする（false は読みやすい幅）。
    pub full_width: bool,
}

impl Default for PreviewStyle {
    fn default() -> Self {
        Self {
            palette: PreviewPalette::default(),
            body_fonts: Vec::new(),
            code_fonts: Vec::new(),
            font_size_px: 15.0,
            full_width: false,
        }
    }
}

const DEFAULT_BODY_FONTS: &str =
    "\"Segoe UI Variable Text\", \"Segoe UI\", \"Yu Gothic UI\", \"Meiryo UI\", sans-serif";
const DEFAULT_CODE_FONTS: &str =
    "\"Cascadia Mono\", Consolas, \"BIZ UDGothic\", \"MS Gothic\", monospace";

/// 書体名の並びを CSS の `font-family` の値にする。名前は引用符で囲み、CSS の
/// 構文を壊しうる文字（引用符、`\`、`;`、`{}`、`<>` など）を取り除く。
fn font_family(names: &[String], fallback_generic: &str, default: &str) -> String {
    let quoted: Vec<String> = names
        .iter()
        .map(|n| {
            n.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .filter(|n| !n.is_empty())
        .map(|n| format!("\"{n}\""))
        .collect();
    if quoted.is_empty() {
        default.to_owned()
    } else {
        format!("{}, {fallback_generic}", quoted.join(", "))
    }
}

/// 組み立て済みの CSS。複製は安く、右ペインと別ウィンドウで共有する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stylesheet {
    css: Arc<str>,
    dark: bool,
}

impl Stylesheet {
    /// 見た目の設定から CSS を作る（純粋な関数）。
    pub fn new(style: &PreviewStyle) -> Self {
        Self {
            css: build_css(style).into(),
            dark: style.palette.dark,
        }
    }

    pub fn css(&self) -> &str {
        &self.css
    }

    pub fn is_dark(&self) -> bool {
        self.dark
    }
}

impl Default for Stylesheet {
    fn default() -> Self {
        Self::new(&PreviewStyle::default())
    }
}

fn build_css(style: &PreviewStyle) -> String {
    let p = &style.palette;
    let size = if style.font_size_px.is_finite() {
        style.font_size_px.clamp(8.0, 48.0)
    } else {
        15.0
    };
    let body_font = font_family(&style.body_fonts, "sans-serif", DEFAULT_BODY_FONTS);
    let code_font = font_family(&style.code_fonts, "monospace", DEFAULT_CODE_FONTS);
    let scheme = if p.dark { "dark" } else { "light" };
    let max_width = if style.full_width { "none" } else { "52em" };
    format!(
        r#":root {{
  color-scheme: {scheme};
  --bg: {bg}; --fg: {fg}; --muted: {muted}; --border: {border};
  --surface: {surface}; --link: {link}; --accent: {accent};
}}
* {{ box-sizing: border-box; }}
html {{ background: var(--bg); color: var(--fg); }}
body {{
  margin: 0; padding: 24px 32px 48px;
  font-family: {body_font}; font-size: {size}px; line-height: 1.7;
  overflow-wrap: anywhere; word-break: normal;
  -webkit-text-size-adjust: 100%;
}}
.markdown-body {{ max-width: {max_width}; margin: 0 auto; }}
.markdown-body > :first-child {{ margin-top: 0; }}
p, ul, ol, dl, table, pre, blockquote, .footnote-definition {{ margin: 0 0 1em; }}
h1, h2, h3, h4, h5, h6 {{ margin: 1.6em 0 0.6em; line-height: 1.3; font-weight: 600; }}
h1 {{ font-size: 1.9em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }}
h2 {{ font-size: 1.5em; padding-bottom: 0.25em; border-bottom: 1px solid var(--border); }}
h3 {{ font-size: 1.25em; }}
h4 {{ font-size: 1.1em; }}
h5 {{ font-size: 1em; }}
h6 {{ font-size: 0.9em; color: var(--muted); }}
ul, ol {{ padding-left: 1.8em; }}
li + li {{ margin-top: 0.25em; }}
li > ul, li > ol {{ margin: 0.25em 0 0; }}
li > input[type="checkbox"] {{ margin: 0 0.4em 0 -1.4em; vertical-align: middle; }}
ul:has(> li > input[type="checkbox"]) {{ list-style: none; }}
a {{ color: var(--link); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
.link-blocked {{ text-decoration: underline dotted var(--muted); cursor: help; }}
.image {{ color: var(--muted); border: 1px dashed var(--border); border-radius: 4px; padding: 0 0.3em; }}
.image::before {{ content: "画像: "; font-size: 0.85em; }}
code, pre {{ font-family: {code_font}; font-size: 0.9em; }}
code {{ background: var(--surface); border-radius: 4px; padding: 0.1em 0.35em; }}
pre {{
  background: var(--surface); border-radius: 6px; padding: 12px 16px;
  overflow-x: auto; line-height: 1.5;
}}
pre code {{ background: none; padding: 0; font-size: 1em; white-space: pre; }}
pre.raw-html {{ border: 1px dashed var(--border); color: var(--muted); }}
blockquote {{ padding: 0 1em; color: var(--muted); border-left: 4px solid var(--accent); }}
blockquote[class^="markdown-alert-"] {{ color: var(--fg); border-left-color: var(--link); }}
hr {{ height: 0; margin: 1.6em 0; border: 0; border-top: 1px solid var(--border); }}
table {{ border-collapse: collapse; display: block; max-width: 100%; overflow-x: auto; }}
th, td {{ border: 1px solid var(--border); padding: 0.35em 0.8em; }}
th {{ background: var(--surface); font-weight: 600; }}
del {{ color: var(--muted); }}
sup.footnote-reference {{ font-size: 0.75em; }}
.footnote-definition {{ font-size: 0.9em; color: var(--muted); }}
.footnote-definition > p {{ display: inline; }}
.footnote-definition-label {{ margin-right: 0.4em; }}
.toc {{ margin: 0 0 1.6em; padding: 0.6em 1em; border: 1px solid var(--border); border-radius: 6px; font-size: 0.9em; line-height: 1.6; }}
.toc ul {{ margin: 0; padding: 0; list-style: none; }}
.toc li {{ margin: 0; color: var(--muted); }}
.toc li + li {{ margin-top: 0; }}
.toc a {{ color: var(--muted); }}
.toc a:hover {{ color: var(--link); }}
.toc-l2 {{ padding-left: 1.2em; }}
.toc-l3 {{ padding-left: 2.4em; }}
.toc-l4 {{ padding-left: 3.6em; }}
.toc-l5 {{ padding-left: 4.8em; }}
.toc-l6 {{ padding-left: 6em; }}
.src-line {{ display: block; height: 0; margin: 0; padding: 0; }}
::selection {{ background: color-mix(in srgb, var(--link) 35%, transparent); }}
"#,
        bg = p.background.to_css(),
        fg = p.foreground.to_css(),
        muted = p.muted.to_css(),
        border = p.border.to_css(),
        surface = p.surface.to_css(),
        link = p.link.to_css(),
        accent = p.accent.to_css(),
        max_width = max_width,
    )
}

/// HTML 断片（[`render_body`](super::render_body) の結果）とスタイルから、
/// 表示用の完全な HTML 文書を作る。CSP はどの要素より前に置く。
///
/// `title` は `<title>` に入れる（エスケープする）。
pub fn compose_document(body: &str, stylesheet: &Stylesheet, title: &str) -> String {
    let scheme = if stylesheet.is_dark() {
        "dark"
    } else {
        "light"
    };
    let mut out = String::with_capacity(body.len() + stylesheet.css().len() + 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"ja\"><head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"");
    out.push_str(CONTENT_SECURITY_POLICY);
    out.push_str("\">\n<meta name=\"referrer\" content=\"no-referrer\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"");
    out.push_str(scheme);
    out.push_str("\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    escape_attr(&mut out, title);
    out.push_str("</title>\n<style>\n");
    // CSS は色の値と、記号を除いた書体名だけから作るので `</style>` は現れない。
    out.push_str(stylesheet.css());
    out.push_str("</style>\n</head>\n<body><main class=\"markdown-body\">\n");
    out.push_str(body);
    out.push_str("</main></body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_come_from_the_palette() {
        let mut style = PreviewStyle::default();
        style.palette.background = Rgb::new(0x12, 0x34, 0x56);
        style.palette.link = Rgb::from_colorref(0x00ccbbaa);
        let sheet = Stylesheet::new(&style);
        assert!(sheet.css().contains("--bg: #123456;"));
        assert!(sheet.css().contains("--link: #aabbcc;"));
        assert!(sheet.css().contains("color-scheme: light;"));
        let dark = Stylesheet::new(&PreviewStyle {
            palette: PreviewPalette::dark(),
            ..PreviewStyle::default()
        });
        assert!(dark.is_dark());
        assert!(dark.css().contains("color-scheme: dark;"));
    }

    #[test]
    fn spacing_for_headings_paragraphs_and_code() {
        let css = Stylesheet::default().css().to_owned();
        assert!(css.contains("h1, h2, h3, h4, h5, h6 { margin:"));
        assert!(css.contains("p, ul, ol"));
        assert!(css.contains("pre {\n  background: var(--surface); border-radius: 6px; padding:"));
    }

    #[test]
    fn font_names_cannot_inject_css() {
        let style = PreviewStyle {
            body_fonts: vec![
                "游ゴシック".into(),
                "x\"; } body { background: url(https://evil) } </style><script>".into(),
                "  ".into(),
            ],
            code_fonts: vec!["Cascadia Code".into()],
            ..PreviewStyle::default()
        };
        let css = Stylesheet::new(&style).css().to_owned();
        assert!(css.contains("font-family: \"游ゴシック\", \"x  body  background urlhttpsevil  stylescript\", sans-serif;"), "{css}");
        assert!(css.contains("\"Cascadia Code\", monospace"));
        assert!(!css.contains("</style>") && !css.contains("url("));
    }

    #[test]
    fn full_width_and_font_size_reach_the_css() {
        assert!(Stylesheet::default().css().contains("max-width: 52em;"));
        let style = PreviewStyle {
            full_width: true,
            font_size_px: 24.0,
            ..PreviewStyle::default()
        };
        let css = Stylesheet::new(&style).css().to_owned();
        assert!(css.contains("max-width: none;"));
        assert!(css.contains("font-size: 24px;"));
    }

    #[test]
    fn font_size_is_clamped() {
        let style = PreviewStyle {
            font_size_px: f32::NAN,
            ..PreviewStyle::default()
        };
        assert!(Stylesheet::new(&style).css().contains("font-size: 15px;"));
        let style = PreviewStyle {
            font_size_px: 1000.0,
            ..PreviewStyle::default()
        };
        assert!(Stylesheet::new(&style).css().contains("font-size: 48px;"));
    }

    #[test]
    fn document_has_csp_first_and_no_scripts() {
        let doc = compose_document("<p>x</p>", &Stylesheet::default(), "a</title><script>");
        let csp = doc
            .find("Content-Security-Policy")
            .expect("CSP meta must exist");
        assert!(csp < doc.find("<title>").unwrap());
        assert!(csp < doc.find("<style>").unwrap());
        assert!(csp < doc.find("<body>").unwrap());
        assert!(doc.contains("default-src 'none'"));
        assert!(doc.contains("script-src 'none'"));
        assert!(doc.contains("img-src 'none'"));
        assert!(!doc.contains("<script"));
        assert!(doc.contains("<title>a&lt;/title&gt;&lt;script&gt;</title>"));
        assert!(doc.starts_with("<!DOCTYPE html>"));
        assert!(doc.contains("<main class=\"markdown-body\">\n<p>x</p></main>"));
    }

    #[test]
    fn csp_has_no_quotes_that_break_the_attribute() {
        assert!(!CONTENT_SECURITY_POLICY.contains('"'));
    }
}
