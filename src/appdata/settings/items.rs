//! 設定画面の項目の定義（V4-1）。GUI に依存しない表にして、画面（`ui::settings_dialog`）は
//! この表から部品を作る。ページや項目を足すには [`PAGES`] に書き足す（V5・V7）。
//!
//! - 項目は種類（[`ItemKind`]）と、設定からの読み出し・設定への書き込みの関数を持つ。
//! - 保存では、画面を開いた時点の設定（基準）と画面で編集した設定を比べ、**変えた項目だけ**を
//!   最新の設定に差し替える（[`apply_edits`]）。ほかのウィンドウが書いた別の項目を失わない。
//! - 数値は範囲を確かめる（[`Item::parse_number`]）。範囲外・数字でない入力は、設定に入れずに
//!   画面で知らせる。

use crate::text::Eol;

use super::{
    new_file_encoding_name, parse_new_file_encoding, ExternalChangeMode, PreviewWidth, Settings,
    ThemeMode, WrapMode, FONT_SIZE_MAX, FONT_SIZE_MIN, PREVIEW_DELAYS_MS, PREVIEW_FONT_SIZE_MAX,
    PREVIEW_FONT_SIZE_MIN, WRAP_COLUMN_MAX, WRAP_COLUMN_MIN,
};

/// 選択肢 1 つ（設定ファイルでの値と、画面の表示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

/// 項目の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// チェックボックス（[`ItemValue::Bool`]）。
    Check,
    /// 選択肢から 1 つ選ぶ（[`ItemValue::Text`] に選択肢の `value`）。
    Choice(&'static [Choice]),
    /// 整数（[`ItemValue::Number`]。範囲は両端を含む）。
    Number { min: i64, max: i64 },
    /// 書体の選択（[`ItemValue::Font`]。標準のフォント選択ダイアログを呼ぶボタン）。
    Font,
}

/// 項目の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemValue {
    Bool(bool),
    Text(String),
    Number(i64),
    /// 書体の名前（空は既定）と文字サイズ（pt）。
    Font(String, u32),
}

/// 数値の入力の誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseNumberError {
    /// 数字でない。
    NotANumber,
    /// 範囲外（最小, 最大）。
    OutOfRange(i64, i64),
}

impl ParseNumberError {
    /// 利用者に見せる文。
    pub fn message(&self, label: &str) -> String {
        match self {
            ParseNumberError::NotANumber => format!("「{label}」は数字で入力してください"),
            ParseNumberError::OutOfRange(min, max) => {
                format!("「{label}」は {min} から {max} の間で入力してください")
            }
        }
    }
}

/// 設定画面の項目 1 つ。
pub struct Item {
    /// 項目の識別子（`view.wrap` のように設定の場所に合わせる。試験からの操作にも使う）。
    pub id: &'static str,
    /// 画面の表示名。
    pub label: &'static str,
    pub kind: ItemKind,
    get: fn(&Settings) -> ItemValue,
    set: fn(&mut Settings, &ItemValue),
}

impl Item {
    /// 設定からの読み出し。
    pub fn get(&self, settings: &Settings) -> ItemValue {
        (self.get)(settings)
    }

    /// 設定への書き込み（種類に合わない値は無視する）。
    pub fn set(&self, settings: &mut Settings, value: &ItemValue) {
        (self.set)(settings, value);
    }

    /// 数値の入力を確かめる（前後の空白は無視する）。数値の項目以外は範囲なしの整数として読む。
    pub fn parse_number(&self, text: &str) -> Result<i64, ParseNumberError> {
        let (min, max) = match self.kind {
            ItemKind::Number { min, max } => (min, max),
            _ => (i64::MIN, i64::MAX),
        };
        let n: i64 = text
            .trim()
            .parse()
            .map_err(|_| ParseNumberError::NotANumber)?;
        if (min..=max).contains(&n) {
            Ok(n)
        } else {
            Err(ParseNumberError::OutOfRange(min, max))
        }
    }

    /// 選択肢の `value` から、選択肢の中の位置を引く（選択肢の項目だけ）。
    pub fn choice_index(&self, value: &str) -> Option<usize> {
        match self.kind {
            ItemKind::Choice(choices) => choices.iter().position(|c| c.value == value),
            _ => None,
        }
    }
}

/// ページの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// 項目の表（[`Page::items`]）から部品を作る。
    Items,
    /// キー割り当て（V4-2）。コマンドの一覧と割り当ての操作を持つ専用のページ。
    /// 編集は設定の `keys` を直接変える（[`super::keybind`]）ので、項目の表は空。
    Keys,
}

/// 設定画面のページ 1 つ。
pub struct Page {
    pub id: &'static str,
    /// 左の一覧の表示名。
    pub title: &'static str,
    pub kind: PageKind,
    pub items: &'static [Item],
}

const THEME_CHOICES: &[Choice] = &[
    Choice {
        value: "system",
        label: "システムに合わせる",
    },
    Choice {
        value: "light",
        label: "ライト",
    },
    Choice {
        value: "dark",
        label: "ダーク",
    },
];

const EXTERNAL_CHANGE_CHOICES: &[Choice] = &[
    Choice {
        value: "notify",
        label: "変更を通知する",
    },
    Choice {
        value: "reload_if_clean",
        label: "未保存の変更がなければ自動で反映",
    },
];

const WRAP_CHOICES: &[Choice] = &[
    Choice {
        value: "none",
        label: "折り返さない",
    },
    Choice {
        value: "word",
        label: "ウィンドウ幅（単語単位）",
    },
    Choice {
        value: "char",
        label: "ウィンドウ幅（文字単位）",
    },
    Choice {
        value: "column",
        label: "指定した桁",
    },
];

const TAB_WIDTH_CHOICES: &[Choice] = &[
    Choice {
        value: "2",
        label: "2",
    },
    Choice {
        value: "4",
        label: "4",
    },
    Choice {
        value: "8",
        label: "8",
    },
];

fn text(value: &ItemValue) -> Option<&str> {
    match value {
        ItemValue::Text(t) => Some(t),
        _ => None,
    }
}

/// 真偽値の項目を作る（設定の中の場所を、読み出しと書き込みの 2 つの関数で表す）。
macro_rules! check_item {
    ($id:literal, $label:literal, $($path:ident).+) => {
        Item {
            id: $id,
            label: $label,
            kind: ItemKind::Check,
            get: |s| ItemValue::Bool(s.$($path).+),
            set: |s, v| {
                if let ItemValue::Bool(b) = v {
                    s.$($path).+ = *b;
                }
            },
        }
    };
}

static GENERAL_ITEMS: &[Item] = &[
    Item {
        id: "external_change",
        label: "ファイルが外部で変更されたとき",
        kind: ItemKind::Choice(EXTERNAL_CHANGE_CHOICES),
        get: |s| ItemValue::Text(s.external_change.name().into()),
        set: |s, v| {
            if let Some(mode) = text(v).and_then(ExternalChangeMode::from_name) {
                s.external_change = mode;
            }
        },
    },
    Item {
        id: "recent_files_max",
        label: "最近使ったファイルの件数（0 は記録しない）",
        kind: ItemKind::Number { min: 0, max: 30 },
        get: |s| ItemValue::Number(s.recent_files_max as i64),
        set: |s, v| {
            if let ItemValue::Number(n) = v {
                if (0..=30).contains(n) {
                    s.recent_files_max = *n as usize;
                }
            }
        },
    },
    Item {
        id: "theme",
        label: "テーマ",
        kind: ItemKind::Choice(THEME_CHOICES),
        get: |s| ItemValue::Text(s.theme.name().to_owned()),
        set: |s, v| {
            if let Some(mode) = text(v).and_then(ThemeMode::from_name) {
                s.theme = mode;
            }
        },
    },
    check_item!(
        "restore_session",
        "起動時に前回のタブを開き直す",
        restore_session
    ),
    check_item!(
        "open_in_existing_window",
        "起動したとき、開いている Linote のウィンドウがあればそこで開く",
        open_in_existing_window
    ),
];

const NEW_FILE_ENCODING_CHOICES: &[Choice] = &[
    Choice {
        value: "utf-8",
        label: "UTF-8",
    },
    Choice {
        value: "utf-8-bom",
        label: "UTF-8（BOM 付き）",
    },
    Choice {
        value: "shift_jis",
        label: "Shift_JIS (CP932)",
    },
    Choice {
        value: "utf-16le",
        label: "UTF-16 LE（BOM 付き）",
    },
    Choice {
        value: "utf-16be",
        label: "UTF-16 BE（BOM 付き）",
    },
];

const NEW_FILE_EOL_CHOICES: &[Choice] = &[
    Choice {
        value: "crlf",
        label: "CRLF（Windows）",
    },
    Choice {
        value: "lf",
        label: "LF",
    },
];

static FILE_ITEMS: &[Item] = &[
    Item {
        id: "new_file.encoding",
        label: "新規文書の文字コード",
        kind: ItemKind::Choice(NEW_FILE_ENCODING_CHOICES),
        get: |s| ItemValue::Text(new_file_encoding_name(&s.new_file.format).to_owned()),
        set: |s, v| {
            if let Some((encoding, bom)) = text(v).and_then(parse_new_file_encoding) {
                s.new_file.format.encoding = encoding;
                s.new_file.format.bom = bom;
            }
        },
    },
    Item {
        id: "new_file.eol",
        label: "新規文書の改行",
        kind: ItemKind::Choice(NEW_FILE_EOL_CHOICES),
        get: |s| {
            ItemValue::Text(
                match s.new_file.format.eol {
                    Eol::Lf => "lf",
                    // 設定画面にない CR は、設定ファイルで指定されていても CRLF として見せる。
                    _ => "crlf",
                }
                .to_owned(),
            )
        },
        set: |s, v| match text(v) {
            Some("lf") => s.new_file.format.eol = Eol::Lf,
            Some("crlf") => s.new_file.format.eol = Eol::CrLf,
            _ => {}
        },
    },
    check_item!(
        "save.trim_trailing_whitespace",
        "保存時に行末の空白を削除（Markdown を除く）",
        save.trim_trailing_whitespace
    ),
    check_item!(
        "save.ensure_final_newline",
        "保存時に最終行の改行を補う",
        save.ensure_final_newline
    ),
    check_item!(
        "save.untitled_to_memo",
        "無題の文書の保存先をメモにする（Ctrl+S でファイルを選ばずメモに保存）",
        save.untitled_to_memo
    ),
];

static VIEW_ITEMS: &[Item] = &[
    Item {
        id: "view.wrap",
        label: "折り返し",
        kind: ItemKind::Choice(WRAP_CHOICES),
        get: |s| ItemValue::Text(s.view.wrap.name().to_owned()),
        set: |s, v| {
            if let Some(mode) = text(v).and_then(WrapMode::from_name) {
                s.view.wrap = mode;
            }
        },
    },
    Item {
        id: "view.wrap_column",
        label: "折り返す桁",
        kind: ItemKind::Number {
            min: WRAP_COLUMN_MIN as i64,
            max: WRAP_COLUMN_MAX as i64,
        },
        get: |s| ItemValue::Number(i64::from(s.view.wrap_column)),
        set: |s, v| {
            if let ItemValue::Number(n) = v {
                s.view.wrap_column =
                    (*n).clamp(WRAP_COLUMN_MIN as i64, WRAP_COLUMN_MAX as i64) as u32;
            }
        },
    },
    check_item!("view.edge_guide", "折り返し桁のガイド線", view.edge_guide),
    Item {
        id: "view.font",
        label: "本文フォント",
        kind: ItemKind::Font,
        get: |s| ItemValue::Font(s.view.font.clone(), s.view.font_size),
        set: |s, v| {
            if let ItemValue::Font(name, size) = v {
                s.view.font = name.clone();
                s.view.font_size = (*size).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
            }
        },
    },
    Item {
        id: "view.font_size",
        label: "文字サイズ（pt）",
        kind: ItemKind::Number {
            min: FONT_SIZE_MIN as i64,
            max: FONT_SIZE_MAX as i64,
        },
        get: |s| ItemValue::Number(i64::from(s.view.font_size)),
        set: |s, v| {
            if let ItemValue::Number(n) = v {
                s.view.font_size = (*n).clamp(FONT_SIZE_MIN as i64, FONT_SIZE_MAX as i64) as u32;
            }
        },
    },
    check_item!(
        "view.show_whitespace",
        "空白とタブを表示",
        view.show_whitespace
    ),
    check_item!("view.show_eol", "改行記号を表示", view.show_eol),
    check_item!("view.line_numbers", "行番号を表示", view.line_numbers),
    check_item!("view.status_bar", "ステータスバーを表示", view.status_bar),
    check_item!(
        "view.highlight_current_line",
        "現在行を強調",
        view.highlight_current_line
    ),
    check_item!(
        "view.highlight_braces",
        "対応する括弧を強調",
        view.highlight_braces
    ),
    Item {
        id: "view.tab_width",
        label: "タブ幅",
        kind: ItemKind::Choice(TAB_WIDTH_CHOICES),
        get: |s| ItemValue::Text(s.view.tab_width.to_string()),
        set: |s, v| {
            if let Some(w) = text(v).and_then(|t| t.parse::<u32>().ok()) {
                if [2, 4, 8].contains(&w) {
                    s.view.tab_width = w;
                }
            }
        },
    },
    check_item!(
        "view.use_spaces",
        "Tab キーでスペースを入力",
        view.use_spaces
    ),
    check_item!("view.auto_indent", "自動インデント", view.auto_indent),
];

const PREVIEW_WIDTH_CHOICES: &[Choice] = &[
    Choice {
        value: "standard",
        label: "標準（読みやすい幅）",
    },
    Choice {
        value: "full",
        label: "ウィンドウ幅いっぱい",
    },
];

/// 更新までの待ち時間の選択肢（[`PREVIEW_DELAYS_MS`] と同じ順）。
const PREVIEW_DELAY_CHOICES: &[Choice] = &[
    Choice {
        value: "100",
        label: "0.1 秒",
    },
    Choice {
        value: "250",
        label: "0.25 秒",
    },
    Choice {
        value: "500",
        label: "0.5 秒",
    },
    Choice {
        value: "1000",
        label: "1 秒",
    },
];

static PREVIEW_ITEMS: &[Item] = &[
    check_item!(
        "preview.line_breaks",
        "改行をそのまま改行として表示（行末の空白 2 つが不要）",
        preview.line_breaks
    ),
    Item {
        id: "preview.body_font",
        label: "本文のフォント",
        kind: ItemKind::Font,
        get: |s| ItemValue::Font(s.preview.body_font.clone(), s.preview.font_size),
        set: |s, v| {
            if let ItemValue::Font(name, size) = v {
                s.preview.body_font = name.clone();
                s.preview.font_size = (*size).clamp(PREVIEW_FONT_SIZE_MIN, PREVIEW_FONT_SIZE_MAX);
            }
        },
    },
    // 書体の項目の後に置く（数値欄の編集中の値を優先する）。
    Item {
        id: "preview.font_size",
        label: "文字サイズ（pt）",
        kind: ItemKind::Number {
            min: PREVIEW_FONT_SIZE_MIN as i64,
            max: PREVIEW_FONT_SIZE_MAX as i64,
        },
        get: |s| ItemValue::Number(i64::from(s.preview.font_size)),
        set: |s, v| {
            if let ItemValue::Number(n) = v {
                s.preview.font_size =
                    (*n).clamp(PREVIEW_FONT_SIZE_MIN as i64, PREVIEW_FONT_SIZE_MAX as i64) as u32;
            }
        },
    },
    Item {
        id: "preview.code_font",
        label: "コードのフォント（大きさは本文に合わせる）",
        kind: ItemKind::Font,
        get: |s| ItemValue::Font(s.preview.code_font.clone(), s.preview.font_size),
        set: |s, v| {
            // 大きさは本文に合わせるので、選んだ大きさは使わない。
            if let ItemValue::Font(name, _) = v {
                s.preview.code_font = name.clone();
            }
        },
    },
    Item {
        id: "preview.width",
        label: "本文の幅",
        kind: ItemKind::Choice(PREVIEW_WIDTH_CHOICES),
        get: |s| ItemValue::Text(s.preview.width.name().to_owned()),
        set: |s, v| {
            if let Some(w) = text(v).and_then(PreviewWidth::from_name) {
                s.preview.width = w;
            }
        },
    },
    Item {
        id: "preview.delay_ms",
        label: "更新までの待ち時間",
        kind: ItemKind::Choice(PREVIEW_DELAY_CHOICES),
        get: |s| {
            // 設定ファイルの任意の値は、近い選択肢として見せる。
            let nearest = PREVIEW_DELAYS_MS
                .iter()
                .min_by_key(|d| d.abs_diff(s.preview.delay_ms))
                .copied()
                .unwrap_or(250);
            ItemValue::Text(nearest.to_string())
        },
        set: |s, v| {
            if let Some(n) = text(v).and_then(|t| t.parse::<u32>().ok()) {
                if PREVIEW_DELAYS_MS.contains(&n) {
                    s.preview.delay_ms = n;
                }
            }
        },
    },
    check_item!("preview.toc", "見出しの目次を先頭に表示", preview.toc),
    check_item!(
        "preview.follow_cursor",
        "編集位置に合わせてスクロール",
        preview.follow_cursor
    ),
];

/// 設定画面のページ。左の一覧はこの順に並ぶ。ページや項目を足すにはここに書き足す。
static PAGES: &[Page] = &[
    Page {
        id: "general",
        title: "一般",
        kind: PageKind::Items,
        items: GENERAL_ITEMS,
    },
    Page {
        id: "file",
        title: "ファイル",
        kind: PageKind::Items,
        items: FILE_ITEMS,
    },
    Page {
        id: "view",
        title: "表示",
        kind: PageKind::Items,
        items: VIEW_ITEMS,
    },
    Page {
        id: "preview",
        title: "プレビュー",
        kind: PageKind::Items,
        items: PREVIEW_ITEMS,
    },
    Page {
        id: "keys",
        title: "キー",
        kind: PageKind::Keys,
        items: &[],
    },
];

/// 設定画面のページの一覧。
pub fn pages() -> &'static [Page] {
    PAGES
}

/// 基準 `initial` から `edited` で値が変わっている項目。
pub fn edited_items(initial: &Settings, edited: &Settings) -> Vec<&'static Item> {
    PAGES
        .iter()
        .flat_map(|p| p.items.iter())
        .filter(|item| item.get(initial) != item.get(edited))
        .collect()
}

/// 画面で変えた項目（`initial` と `edited` で値が違うもの）だけを `target` に書き込む。
/// 変えなかった項目は `target` の値のまま（最新の設定ファイルの内容を保つ）。
/// 書き込んだ項目の識別子を返す。
pub fn apply_edits(
    initial: &Settings,
    edited: &Settings,
    target: &mut Settings,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    for item in edited_items(initial, edited) {
        item.set(target, &item.get(edited));
        changed.push(item.id);
    }
    // キー割り当ては、コマンドごとに比べる（別のウィンドウが変えた別のコマンドを失わない）。
    let mut keys_changed = false;
    let ids: std::collections::BTreeSet<&String> =
        initial.keys.keys().chain(edited.keys.keys()).collect();
    for id in ids {
        if initial.keys.get(id) == edited.keys.get(id) {
            continue;
        }
        keys_changed = true;
        match edited.keys.get(id) {
            Some(v) => {
                target.keys.insert(id.clone(), v.clone());
            }
            None => {
                target.keys.remove(id);
            }
        }
    }
    if keys_changed {
        changed.push("keys");
    }
    target.view = target.view.clone().clamped();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> &'static Item {
        PAGES
            .iter()
            .flat_map(|p| p.items.iter())
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("項目 {id} がありません"))
    }

    #[test]
    fn item_ids_are_unique_and_values_round_trip() {
        let mut seen = std::collections::HashSet::new();
        let s = Settings::default();
        for page in PAGES {
            assert!(page.kind == PageKind::Keys || !page.items.is_empty());
            for it in page.items {
                assert!(seen.insert(it.id), "重複: {}", it.id);
                // 読んだ値をそのまま書いても変わらない。
                let mut t = s.clone();
                it.set(&mut t, &it.get(&s));
                assert_eq!(t, s, "{}", it.id);
                if let ItemKind::Choice(choices) = it.kind {
                    // 既定値は選択肢のどれかにある。
                    let ItemValue::Text(v) = it.get(&s) else {
                        panic!("{} は文字列", it.id)
                    };
                    assert!(choices.iter().any(|c| c.value == v), "{}", it.id);
                }
            }
        }
    }

    #[test]
    fn items_cover_every_view_setting_except_zoom() {
        // 設定の view の全項目（拡大率は対象外）が表にある。
        let mut s = Settings::default();
        let before = s.view.clone();
        for (id, value) in [
            ("view.wrap", ItemValue::Text("column".into())),
            ("view.wrap_column", ItemValue::Number(100)),
            ("view.edge_guide", ItemValue::Bool(false)),
            ("view.font", ItemValue::Font("Consolas".into(), 14)),
            ("view.show_whitespace", ItemValue::Bool(true)),
            ("view.show_eol", ItemValue::Bool(true)),
            ("view.line_numbers", ItemValue::Bool(false)),
            ("view.status_bar", ItemValue::Bool(false)),
            ("view.highlight_current_line", ItemValue::Bool(false)),
            ("view.highlight_braces", ItemValue::Bool(false)),
            ("view.tab_width", ItemValue::Text("8".into())),
            ("view.use_spaces", ItemValue::Bool(true)),
            ("view.auto_indent", ItemValue::Bool(false)),
        ] {
            item(id).set(&mut s, &value);
            assert_eq!(item(id).get(&s), value, "{id}");
        }
        item("view.font_size").set(&mut s, &ItemValue::Number(20));
        assert_eq!(s.view.font_size, 20);
        let v = &s.view;
        assert_eq!(v.wrap, WrapMode::Column);
        assert_eq!(v.wrap_column, 100);
        assert_eq!(v.font, "Consolas");
        assert_eq!(v.tab_width, 8);
        assert_eq!(v.zoom, before.zoom, "拡大率は対象外");
        assert_ne!(*v, before);
    }

    #[test]
    fn number_input_is_validated() {
        let col = item("view.wrap_column");
        assert_eq!(col.parse_number(" 80 "), Ok(80));
        assert_eq!(col.parse_number("10"), Ok(10));
        assert_eq!(col.parse_number("500"), Ok(500));
        assert_eq!(
            col.parse_number("9"),
            Err(ParseNumberError::OutOfRange(10, 500))
        );
        assert_eq!(
            col.parse_number("501"),
            Err(ParseNumberError::OutOfRange(10, 500))
        );
        assert_eq!(col.parse_number(""), Err(ParseNumberError::NotANumber));
        assert_eq!(col.parse_number("八十"), Err(ParseNumberError::NotANumber));
        assert_eq!(col.parse_number("1.5"), Err(ParseNumberError::NotANumber));
        assert!(col
            .parse_number("x")
            .unwrap_err()
            .message("折り返す桁")
            .contains("数字"));
        assert!(col
            .parse_number("0")
            .unwrap_err()
            .message("折り返す桁")
            .contains("10 から 500"));
        let size = item("view.font_size");
        assert_eq!(size.parse_number("72"), Ok(72));
        assert!(size.parse_number("5").is_err());
    }

    #[test]
    fn external_change_and_recent_limit_are_editable() {
        let baseline = Settings::default();
        let mut edited = baseline.clone();
        item("external_change").set(&mut edited, &ItemValue::Text("reload_if_clean".into()));
        item("recent_files_max").set(&mut edited, &ItemValue::Number(30));
        assert_eq!(edited.external_change, ExternalChangeMode::ReloadIfClean);
        assert_eq!(edited.recent_files_max, 30);
        assert!(item("recent_files_max").parse_number("31").is_err());
        assert_eq!(item("recent_files_max").parse_number("0"), Ok(0));
        let mut latest = baseline.clone();
        latest.grep.extensions = "*.rs".into();
        apply_edits(&baseline, &edited, &mut latest);
        assert_eq!(latest.external_change, ExternalChangeMode::ReloadIfClean);
        assert_eq!(latest.recent_files_max, 30);
        assert_eq!(latest.grep.extensions, "*.rs");
    }

    #[test]
    fn choice_index_finds_the_value() {
        let theme = item("theme");
        assert_eq!(theme.choice_index("system"), Some(0));
        assert_eq!(theme.choice_index("dark"), Some(2));
        assert_eq!(theme.choice_index("nope"), None);
        assert_eq!(item("view.show_eol").choice_index("x"), None);
    }

    #[test]
    fn only_edited_items_are_applied_to_the_latest_settings() {
        let initial = Settings::default();
        // 画面で テーマ と 折り返し だけを変えた。
        let mut edited = initial.clone();
        edited.theme = ThemeMode::Dark;
        edited.view.wrap = WrapMode::Word;
        // そのあいだに、別のウィンドウが タブ幅 と 最近使ったファイル を書いた。
        let mut latest = initial.clone();
        latest.view.tab_width = 8;
        latest.recent_files = vec!["C:\\a.txt".into()];
        latest.view.show_eol = true;
        let changed = apply_edits(&initial, &edited, &mut latest);
        assert_eq!(changed, vec!["theme", "view.wrap"]);
        assert_eq!(latest.theme, ThemeMode::Dark);
        assert_eq!(latest.view.wrap, WrapMode::Word);
        // 画面で触らなかった項目は最新のまま。
        assert_eq!(latest.view.tab_width, 8);
        assert!(latest.view.show_eol);
        assert_eq!(latest.recent_files, vec!["C:\\a.txt".to_owned()]);
        // 変更がなければ何も書かない。
        let mut same = latest.clone();
        assert!(apply_edits(&initial, &initial.clone(), &mut same).is_empty());
        assert_eq!(same, latest);
    }

    #[test]
    fn key_edits_are_applied_per_command() {
        let mut initial = Settings::default();
        initial.keys.insert("edit.undo".into(), "F2".into());
        initial.keys.insert("file.open".into(), String::new());
        // 画面で file.save を変え、edit.undo を既定に戻した（file.open は触らない）。
        let mut edited = initial.clone();
        edited.keys.insert("file.save".into(), "Ctrl+Alt+S".into());
        edited.keys.remove("edit.undo");
        // そのあいだに別のウィンドウが file.open と view.zoom_in を変えた。
        let mut latest = initial.clone();
        latest.keys.insert("file.open".into(), "F4".into());
        latest
            .keys
            .insert("view.zoom_in".into(), "Ctrl+Plus".into());
        let changed = apply_edits(&initial, &edited, &mut latest);
        assert_eq!(changed, vec!["keys"]);
        assert_eq!(latest.keys["file.save"], "Ctrl+Alt+S");
        assert!(!latest.keys.contains_key("edit.undo"));
        assert_eq!(latest.keys["file.open"], "F4");
        assert_eq!(latest.keys["view.zoom_in"], "Ctrl+Plus");
    }

    #[test]
    fn font_edit_writes_name_and_size_together() {
        let initial = Settings::default();
        let mut edited = initial.clone();
        edited.view.font = "MS Gothic".into();
        edited.view.font_size = 16;
        let mut target = initial.clone();
        let changed = apply_edits(&initial, &edited, &mut target);
        assert!(changed.contains(&"view.font"));
        assert!(changed.contains(&"view.font_size"));
        assert_eq!(target.view.font, "MS Gothic");
        assert_eq!(target.view.font_size, 16);
    }

    #[test]
    fn unknown_settings_survive_an_edit() {
        let text = r#"{ "future": {"a": 1}, "theme": "light" }"#;
        let initial = Settings::from_json(text).unwrap();
        let mut edited = initial.clone();
        edited.view.show_eol = true;
        let mut target = initial.clone();
        apply_edits(&initial, &edited, &mut target);
        assert!(target.to_json().contains("\"future\""));
        assert_eq!(target.theme, ThemeMode::Light);
    }
}
