//! 明暗テーマと配色。Windows の設定に連動する
//! （[`crate::appdata::ThemeMode`] で固定もできる）。編集欄とプレビューで同じ配色を使う。
//!
//! - [`Theme`] が配色の一式。[`Theme::resolve`] で設定と Windows の明暗から選ぶ。
//! - 構文着色は意味の分類（[`Token`]）ごとに色を決め、字句解析器ごとの対応は
//!   [`crate::ui::editor`] が持つ。
//! - プレビュー（タスク H）は [`Theme::css_variables`] で同じ配色を CSS 変数として受け取る。
//!
//! ここは Win32 を呼ばない純粋な計算だけ（単体テストで配色の見分けやすさを確かめる）。
//!
//! 担当: タスク B。

use crate::appdata::ThemeMode;

/// sRGB の色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `0xRRGGBB` から。
    pub const fn hex(v: u32) -> Rgb {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// Win32 の `COLORREF`（`0x00BBGGRR`）。Scintilla の色の引数も同じ形式。
    pub const fn colorref(self) -> u32 {
        self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16
    }

    /// Scintilla の `SCI_SETELEMENTCOLOUR` 用（不透明の `0xAABBGGRR`）。
    pub const fn colour_alpha(self) -> u32 {
        self.colorref() | 0xFF00_0000
    }

    /// CSS の `#rrggbb`。
    pub fn css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// `self` から `other` へ `t`（0〜1）だけ寄せた色。
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    /// 相対輝度（WCAG 2）。
    pub fn luminance(self) -> f32 {
        fn channel(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    /// コントラスト比（WCAG 2。1〜21）。
    pub fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

/// 構文着色の意味の分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Default,
    Comment,
    Keyword,
    /// 型名、第 2 キーワード群、組み込みの名前。
    Type,
    String,
    Number,
    Operator,
    Preprocessor,
    /// HTML/XML のタグ、Markdown の見出し以外の印、YAML のキー、INI のセクション。
    Tag,
    /// 属性名、プロパティ名、変数。
    Attribute,
    /// Markdown の見出し。
    Heading,
    /// 強調（Markdown の太字・斜体）。
    Emphasis,
    /// リンク、URL。
    Link,
    /// コード（Markdown のコード、正規表現の値など）。
    Code,
    /// 字句の誤り（閉じていない文字列など）。
    Error,
}

/// 1 つの分類の表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenStyle {
    pub fore: Rgb,
    pub bold: bool,
    pub italic: bool,
}

/// 画面部品（タブバー、ステータスバー）の配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiColors {
    /// 上部のタブバーの背景。
    pub bar_bg: Rgb,
    /// 選ばれていないタブの文字。
    pub tab_fg: Rgb,
    /// 選ばれているタブの背景（本文の背景と同じにして、本文とつながって見せる）。
    pub tab_active_bg: Rgb,
    pub tab_active_fg: Rgb,
    /// マウスが乗ったタブの背景。
    pub tab_hover_bg: Rgb,
    /// タブの区切り線。
    pub tab_separator: Rgb,
    /// アクセント（選択中のタブの下線、フォーカスの印）。
    pub accent: Rgb,
    /// 未保存の丸。
    pub dirty: Rgb,
    /// AI の変更の印。
    pub ai_mark: Rgb,
    /// 閉じるボタンにマウスが乗ったときの背景。
    pub close_hover_bg: Rgb,
    pub status_bg: Rgb,
    pub status_fg: Rgb,
    /// ステータスバーの補助的な文字（区切り、読み取り専用以外の項目）。
    pub status_dim: Rgb,
    /// ステータスバーの警告（読み取り専用、外部変更）。
    pub status_warn: Rgb,
    /// 右ペインとの区切り線。
    pub splitter: Rgb,
}

/// 編集欄の配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorColors {
    pub bg: Rgb,
    pub fg: Rgb,
    pub caret: Rgb,
    /// カーソル行の背景。
    pub caret_line_bg: Rgb,
    /// フォーカスがあるときの選択範囲の背景。
    pub selection_bg: Rgb,
    /// フォーカスがないときの選択範囲の背景（控えめにして、フォーカスの位置を見分ける）。
    pub inactive_selection_bg: Rgb,
    pub line_number_fg: Rgb,
    pub line_number_bg: Rgb,
    /// 空白の表示、インデントの線。
    pub whitespace: Rgb,
}

/// 配色の一式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub dark: bool,
    pub ui: UiColors,
    pub editor: EditorColors,
    syntax: [TokenStyle; TOKEN_COUNT],
}

const TOKEN_COUNT: usize = 15;

impl Token {
    pub const ALL: [Token; TOKEN_COUNT] = [
        Token::Default,
        Token::Comment,
        Token::Keyword,
        Token::Type,
        Token::String,
        Token::Number,
        Token::Operator,
        Token::Preprocessor,
        Token::Tag,
        Token::Attribute,
        Token::Heading,
        Token::Emphasis,
        Token::Link,
        Token::Code,
        Token::Error,
    ];

    /// CSS 変数などで使う名前。
    pub const fn name(self) -> &'static str {
        match self {
            Token::Default => "default",
            Token::Comment => "comment",
            Token::Keyword => "keyword",
            Token::Type => "type",
            Token::String => "string",
            Token::Number => "number",
            Token::Operator => "operator",
            Token::Preprocessor => "preprocessor",
            Token::Tag => "tag",
            Token::Attribute => "attribute",
            Token::Heading => "heading",
            Token::Emphasis => "emphasis",
            Token::Link => "link",
            Token::Code => "code",
            Token::Error => "error",
        }
    }
}

const fn plain(v: u32) -> TokenStyle {
    TokenStyle {
        fore: Rgb::hex(v),
        bold: false,
        italic: false,
    }
}

const fn bold(v: u32) -> TokenStyle {
    TokenStyle {
        fore: Rgb::hex(v),
        bold: true,
        italic: false,
    }
}

const fn italic(v: u32) -> TokenStyle {
    TokenStyle {
        fore: Rgb::hex(v),
        bold: false,
        italic: true,
    }
}

impl Theme {
    /// 暗いテーマ。本文背景 `#191D24`、上部バー `#212731`、本文 `#E3E8EF`、
    /// アクセント `#91ABD0`。
    pub fn dark() -> Theme {
        let bg = Rgb::hex(0x191D24);
        let fg = Rgb::hex(0xE3E8EF);
        let bar = Rgb::hex(0x212731);
        let accent = Rgb::hex(0x91ABD0);
        Theme {
            dark: true,
            ui: UiColors {
                bar_bg: bar,
                tab_fg: Rgb::hex(0x9AA5B4),
                tab_active_bg: bg,
                tab_active_fg: fg,
                tab_hover_bg: bar.mix(fg, 0.07),
                tab_separator: bar.mix(fg, 0.12),
                accent,
                dirty: Rgb::hex(0xE6B673),
                ai_mark: Rgb::hex(0xC49BE0),
                close_hover_bg: bar.mix(fg, 0.18),
                status_bg: bar,
                status_fg: Rgb::hex(0xC5CDD8),
                status_dim: Rgb::hex(0x8994A3),
                status_warn: Rgb::hex(0xE6B673),
                splitter: bar.mix(fg, 0.10),
            },
            editor: EditorColors {
                bg,
                fg,
                caret: Rgb::hex(0xD5E2F5),
                caret_line_bg: bg.mix(fg, 0.04),
                selection_bg: bg.mix(accent, 0.27),
                inactive_selection_bg: bg.mix(fg, 0.12),
                line_number_fg: Rgb::hex(0x6B7686),
                line_number_bg: bg,
                whitespace: Rgb::hex(0x3A4250),
            },
            syntax: [
                plain(0xE3E8EF),  // Default
                italic(0x95A1B3), // Comment
                plain(0x9DB6DC),  // Keyword
                plain(0x86CBD6),  // Type
                plain(0xA9D49C),  // String
                plain(0xE8BB8A),  // Number
                plain(0xB9C3D0),  // Operator
                plain(0xD3A6D8),  // Preprocessor
                plain(0x8FB8EC),  // Tag
                plain(0xE5C890),  // Attribute
                bold(0x9DB6DC),   // Heading
                bold(0xEDF1F6),   // Emphasis
                plain(0x86CBD6),  // Link
                plain(0xA9D49C),  // Code
                plain(0xF2958C),  // Error
            ],
        }
    }

    /// 明るいテーマ。
    pub fn light() -> Theme {
        let bg = Rgb::hex(0xFCFCFD);
        let fg = Rgb::hex(0x1F2328);
        let bar = Rgb::hex(0xECEFF3);
        let accent = Rgb::hex(0x3F6CA8);
        Theme {
            dark: false,
            ui: UiColors {
                bar_bg: bar,
                tab_fg: Rgb::hex(0x56606C),
                tab_active_bg: bg,
                tab_active_fg: fg,
                tab_hover_bg: bar.mix(fg, 0.06),
                tab_separator: bar.mix(fg, 0.14),
                accent,
                dirty: Rgb::hex(0xA8680A),
                ai_mark: Rgb::hex(0x8A4FB8),
                close_hover_bg: bar.mix(fg, 0.14),
                status_bg: bar,
                status_fg: Rgb::hex(0x323A44),
                status_dim: Rgb::hex(0x5F6874),
                status_warn: Rgb::hex(0x9A5B00),
                splitter: bar.mix(fg, 0.12),
            },
            editor: EditorColors {
                bg,
                fg,
                caret: Rgb::hex(0x1F2328),
                caret_line_bg: bg.mix(accent, 0.05),
                selection_bg: bg.mix(accent, 0.28),
                inactive_selection_bg: bg.mix(fg, 0.10),
                line_number_fg: Rgb::hex(0x7D8691),
                line_number_bg: bg,
                whitespace: Rgb::hex(0xC5CBD3),
            },
            syntax: [
                plain(0x1F2328),  // Default
                italic(0x6A737D), // Comment
                plain(0x3B4BA0),  // Keyword
                plain(0x0A6E7C),  // Type
                plain(0x2C6E2F),  // String
                plain(0xA34E00),  // Number
                plain(0x4A5562),  // Operator
                plain(0x8A3F9E),  // Preprocessor
                plain(0x1F5C9A),  // Tag
                plain(0x84520A),  // Attribute
                bold(0x3B4BA0),   // Heading
                bold(0x111418),   // Emphasis
                plain(0x0A6E7C),  // Link
                plain(0x2C6E2F),  // Code
                plain(0xC0261D),  // Error
            ],
        }
    }

    /// 設定と Windows の明暗から配色を選ぶ。`system_light` は Windows の
    /// 「アプリのモード」（読めなければ `None`。そのときは明るいテーマ）。
    pub fn resolve(mode: ThemeMode, system_light: Option<bool>) -> Theme {
        let dark = match mode {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => system_light == Some(false),
        };
        if dark {
            Theme::dark()
        } else {
            Theme::light()
        }
    }

    pub fn token(&self, token: Token) -> TokenStyle {
        self.syntax[token as usize]
    }

    /// プレビューなどの HTML 用の CSS 変数（`:root { --bg: #…; … }`）。
    ///
    /// 変数: `--bg`、`--fg`、`--accent`、`--selection`、`--bar`、`--muted`、
    /// `--border`、`--code-bg`、`--tok-<分類名>`（[`Token::name`]）。
    pub fn css_variables(&self) -> String {
        let e = &self.editor;
        let u = &self.ui;
        let mut out = String::from(":root {\n");
        let mut var = |name: &str, c: Rgb| {
            out.push_str(&format!("  --{name}: {};\n", c.css()));
        };
        var("bg", e.bg);
        var("fg", e.fg);
        var("accent", u.accent);
        var("selection", e.selection_bg);
        var("bar", u.bar_bg);
        var("muted", self.token(Token::Comment).fore);
        var("border", u.tab_separator);
        var(
            "code-bg",
            e.bg.mix(e.fg, if self.dark { 0.06 } else { 0.04 }),
        );
        for t in Token::ALL {
            var(&format!("tok-{}", t.name()), self.token(t).fore);
        }
        out.push_str(&format!(
            "  color-scheme: {};\n}}\n",
            if self.dark { "dark" } else { "light" }
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_conversions() {
        let c = Rgb::hex(0x191D24);
        assert_eq!(c, Rgb(0x19, 0x1D, 0x24));
        assert_eq!(c.colorref(), 0x00241D19);
        assert_eq!(c.colour_alpha(), 0xFF241D19);
        assert_eq!(c.css(), "#191d24");
        assert_eq!(
            Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 0.5),
            Rgb(128, 128, 128)
        );
        assert_eq!(c.mix(Rgb(255, 0, 0), 0.0), c);
        assert!((Rgb(0, 0, 0).contrast(Rgb(255, 255, 255)) - 21.0).abs() < 0.01);
    }

    #[test]
    fn dark_theme_uses_the_agreed_base_colours() {
        let t = Theme::dark();
        assert_eq!(t.editor.bg, Rgb::hex(0x191D24));
        assert_eq!(t.ui.bar_bg, Rgb::hex(0x212731));
        assert_eq!(t.editor.fg, Rgb::hex(0xE3E8EF));
        assert_eq!(t.ui.accent, Rgb::hex(0x91ABD0));
    }

    #[test]
    fn text_is_readable_in_both_themes() {
        for t in [Theme::dark(), Theme::light()] {
            let bg = t.editor.bg;
            assert!(
                t.editor.fg.contrast(bg) >= 7.0,
                "body text, dark={}",
                t.dark
            );
            for tok in Token::ALL {
                let need = if tok == Token::Comment { 4.0 } else { 4.5 };
                let c = t.token(tok).fore.contrast(bg);
                assert!(c >= need, "{tok:?} contrast {c} (dark={})", t.dark);
                // 選択範囲の上でも読める。
                let c = t.token(tok).fore.contrast(t.editor.selection_bg);
                assert!(c >= 3.0, "{tok:?} on selection {c} (dark={})", t.dark);
            }
            assert!(t.editor.fg.contrast(t.editor.selection_bg) >= 4.5);
            assert!(t.ui.tab_active_fg.contrast(t.ui.tab_active_bg) >= 7.0);
            assert!(t.ui.tab_fg.contrast(t.ui.bar_bg) >= 4.5);
            assert!(t.ui.status_fg.contrast(t.ui.status_bg) >= 7.0);
            assert!(t.ui.status_dim.contrast(t.ui.status_bg) >= 4.5);
            assert!(t.ui.status_warn.contrast(t.ui.status_bg) >= 4.5);
            assert!(t.editor.line_number_fg.contrast(t.editor.line_number_bg) >= 3.0);
            assert!(t.editor.caret.contrast(bg) >= 4.5);
        }
    }

    #[test]
    fn selection_focus_and_caret_line_are_distinguishable() {
        for t in [Theme::dark(), Theme::light()] {
            let e = &t.editor;
            // 選択範囲は背景・カーソル行からはっきり区別できる。
            assert!(e.selection_bg.contrast(e.bg) >= 1.35, "dark={}", t.dark);
            assert!(e.selection_bg.contrast(e.caret_line_bg) >= 1.3);
            // フォーカスのない選択は、ある選択より控えめだが背景とは区別できる。
            assert!(e.inactive_selection_bg.contrast(e.bg) >= 1.15);
            assert!(e.selection_bg.contrast(e.bg) > e.inactive_selection_bg.contrast(e.bg) + 0.1);
            // カーソル行は控えめ。
            let cl = e.caret_line_bg.contrast(e.bg);
            assert!(cl > 1.01 && cl < 1.15, "caret line {cl}");
            // 選択中のタブはバーから区別でき、アクセントは見える。
            assert!(t.ui.tab_active_bg.contrast(t.ui.bar_bg) >= 1.1);
            assert!(t.ui.accent.contrast(t.ui.bar_bg) >= 3.0);
            assert!(t.ui.dirty.contrast(t.ui.bar_bg) >= 3.0);
        }
    }

    #[test]
    fn resolve_follows_the_setting_and_windows() {
        assert!(Theme::resolve(ThemeMode::System, Some(false)).dark);
        assert!(!Theme::resolve(ThemeMode::System, Some(true)).dark);
        assert!(!Theme::resolve(ThemeMode::System, None).dark);
        assert!(Theme::resolve(ThemeMode::Dark, Some(true)).dark);
        assert!(!Theme::resolve(ThemeMode::Light, Some(false)).dark);
    }

    #[test]
    fn css_variables_cover_every_token() {
        let css = Theme::dark().css_variables();
        assert!(css.starts_with(":root {"));
        assert!(css.contains("--bg: #191d24;"));
        assert!(css.contains("color-scheme: dark;"));
        for t in Token::ALL {
            assert!(css.contains(&format!("--tok-{}:", t.name())), "{t:?}");
        }
    }
}
