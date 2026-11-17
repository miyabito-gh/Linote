//! 設定の形式。
//!
//! `settings.json`（UTF-8 の JSON）:
//!
//! ```json
//! {
//!   "version": 1,
//!   "theme": "system",
//!   "keys": { "clipboard.new_tab": "Ctrl+Alt+V", "file.save": "" },
//!   "recent_files": ["C:\\work\\a.txt"],
//!   "view": { "wrap": "word", "wrap_column": 80, "zoom": 0, "line_numbers": true },
//!   "print": { "margin_left": 2000, "landscape": false, "color": true },
//!   "restore_session": true,
//!   "open_in_existing_window": true,
//!   "window": { "x": 100, "y": 80, "width": 1100, "height": 720, "maximized": false },
//!   "mcp": { "enabled": false }
//! }
//! ```
//!
//! - `keys`: コマンド名 → キー表記（[`KeyChord`]）。ここにないコマンドは定義の既定値
//!   （[`CommandDef::default_key`]）を使う。空文字列は「割り当てなし」。
//!   グローバルショートカット（クリップボードの展開）も同じ表で、そのコマンドの
//!   割り当てをエディタ内とグローバルの両方に使う。
//! - `recent_files`: 最近使ったファイル（新しい順、`recent_files_max` 件まで。既定10、0〜30）。
//!   存在確認は読み込みでは行わない（選んだときに確かめる）。
//! - `view`: 表示の設定（[`ViewSettings`]）。値が不正な項目は既定値にして読む
//!   （`view` がオブジェクトでなければ壊れているとみなす）。全タブに適用する。
//! - `print`: 印刷の設定（[`PrintSettings`]。余白は 0.01 mm、用紙の種類と向き、色、行番号、
//!   ヘッダー、フッター）。値が不正な項目は既定値にして読む。
//! - `restore_session`: 起動時に前回のタブを開き直す（既定 `true`。記録は `session.json`）。
//! - `open_in_existing_window`: 起動したとき、既存の Linote のウィンドウがあればそこで開く
//!   （既定 `true`。`--new-window` で個別に避けられる）。
//! - `window`: 前回のウィンドウの位置と大きさ（通常状態の矩形と最大化の有無。V1）。値が
//!   不正なら無視して既定の位置で開く。
//! - `search_history`: 検索欄・置換欄・Grep の起点欄の履歴（`find`／`replace`／`grep_dir`。
//!   各欄は新しい順、重複なし、最大 [`MAX_SEARCH_HISTORY`] 件。V3）。壊れた値は空として読む。
//! - 知らない項目は読み込んだまま保持し、書き戻すときに失わない
//!   （新しい版が書いた設定を古い版が消さないため）。
//! - ファイルがなければ既定値。ファイルは設定を変更して保存するときに初めて作る。
//! - 読めない（壊れている）場合は [`Error::Corrupt`]。呼び出し側は既定値で起動して
//!   知らせ、利用者が設定を変更するまで**ファイルを上書きしない**。
//! - 保存は読み直してから書く: 保存の直前にファイルを読み直し、変更する項目だけを
//!   差し替えて、`file` モジュールの原子的な書き込みで置換する（別ウィンドウの変更を
//!   失わないため）。読み書きの処理自体はここに持たない（統合時に `file` を使って書く）。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::command::keys::KeyParseError;
use crate::command::{CommandDef, KeyChord};
use crate::core::{Error, Result};
use crate::printing::{Margins, MARGIN_MAX};
use crate::text::{Encoding, Eol, TextFormat};

mod items;
pub mod keybind;
pub mod store;

pub use items::{
    apply_edits, edited_items, pages, Choice, Item, ItemKind, ItemValue, Page, PageKind,
    ParseNumberError,
};

/// 現在の設定の形式の版。
pub const SETTINGS_VERSION: u64 = 1;

/// 最近使ったファイルの既定件数。
pub const MAX_RECENT_FILES: usize = 10;
/// 設定できる最近使ったファイルの件数の上限。
pub const RECENT_FILES_LIMIT: usize = 30;
/// 検索・置換・Grep の起点の履歴の、欄ごとの最大件数。
pub const MAX_SEARCH_HISTORY: usize = 20;

/// 明暗テーマの選び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Windows の設定に連動する（既定）。
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const fn name(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark]
            .into_iter()
            .find(|m| m.name() == name)
    }
}

/// 折り返しの方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// 折り返さない（既定）。
    #[default]
    None,
    /// ウィンドウ幅で、単語単位に折り返す。
    Word,
    /// ウィンドウ幅で、文字単位に折り返す。
    Char,
    /// 指定した桁（[`ViewSettings::wrap_column`]）で折り返す。
    Column,
}

impl WrapMode {
    pub const ALL: [WrapMode; 4] = [
        WrapMode::None,
        WrapMode::Word,
        WrapMode::Char,
        WrapMode::Column,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            WrapMode::None => "none",
            WrapMode::Word => "word",
            WrapMode::Char => "char",
            WrapMode::Column => "column",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// 折り返し桁の範囲と既定値。
pub const WRAP_COLUMN_MIN: u32 = 10;
pub const WRAP_COLUMN_MAX: u32 = 500;
pub const WRAP_COLUMN_DEFAULT: u32 = 80;
/// 文字サイズ（ポイント）の範囲と既定値。
pub const FONT_SIZE_MIN: u32 = 6;
pub const FONT_SIZE_MAX: u32 = 72;
pub const FONT_SIZE_DEFAULT: u32 = 11;
/// 拡大率（Scintilla の `SCI_SETZOOM` と同じ。文字サイズに足すポイント数）の範囲。
pub const ZOOM_MIN: i32 = -10;
pub const ZOOM_MAX: i32 = 20;

/// 表示の設定（全タブに適用する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSettings {
    pub wrap: WrapMode,
    /// `wrap` が [`WrapMode::Column`] のときの桁数。
    pub wrap_column: u32,
    /// 書体の名前。空は既定（Cascadia Mono → Consolas → MS Gothic の順）。
    pub font: String,
    /// 文字サイズ（ポイント）。
    pub font_size: u32,
    /// 拡大・縮小（Ctrl+ホイールなど）。
    pub zoom: i32,
    pub show_whitespace: bool,
    pub show_eol: bool,
    pub line_numbers: bool,
    /// ステータスバーの表示（非表示中も情報・通知を保持する）。
    pub status_bar: bool,
    pub highlight_current_line: bool,
    pub highlight_braces: bool,
    /// 折り返し桁のガイド線（`wrap` が桁指定のときだけ出す）。
    pub edge_guide: bool,
    /// タブ幅（2／4／8）。
    pub tab_width: u32,
    /// Tab キーでスペースを入力する。
    pub use_spaces: bool,
    pub auto_indent: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            wrap: WrapMode::None,
            wrap_column: WRAP_COLUMN_DEFAULT,
            font: String::new(),
            font_size: FONT_SIZE_DEFAULT,
            zoom: 0,
            show_whitespace: false,
            show_eol: false,
            line_numbers: true,
            status_bar: true,
            highlight_current_line: true,
            highlight_braces: true,
            edge_guide: true,
            tab_width: 4,
            use_spaces: false,
            auto_indent: true,
        }
    }
}

impl ViewSettings {
    /// 範囲外の値を範囲内に直す。
    pub fn clamped(mut self) -> Self {
        self.wrap_column = self.wrap_column.clamp(WRAP_COLUMN_MIN, WRAP_COLUMN_MAX);
        self.font_size = self.font_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        self.zoom = self.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if ![2, 4, 8].contains(&self.tab_width) {
            self.tab_width = 4;
        }
        self
    }

    fn from_value(map: &Map<String, Value>) -> Self {
        let d = ViewSettings::default();
        let flag = |k: &str, default: bool| map.get(k).and_then(Value::as_bool).unwrap_or(default);
        let num = |k: &str| map.get(k).and_then(Value::as_i64);
        ViewSettings {
            wrap: map
                .get("wrap")
                .and_then(Value::as_str)
                .and_then(WrapMode::from_name)
                .unwrap_or(d.wrap),
            wrap_column: num("wrap_column").map_or(d.wrap_column, |n| n.clamp(0, 10_000) as u32),
            font: map
                .get("font")
                .and_then(Value::as_str)
                .map_or(d.font.clone(), str::to_owned),
            font_size: num("font_size").map_or(d.font_size, |n| n.clamp(0, 10_000) as u32),
            zoom: num("zoom").map_or(d.zoom, |n| n.clamp(-1000, 1000) as i32),
            show_whitespace: flag("show_whitespace", d.show_whitespace),
            show_eol: flag("show_eol", d.show_eol),
            line_numbers: flag("line_numbers", d.line_numbers),
            status_bar: flag("status_bar", d.status_bar),
            highlight_current_line: flag("highlight_current_line", d.highlight_current_line),
            highlight_braces: flag("highlight_braces", d.highlight_braces),
            edge_guide: flag("edge_guide", d.edge_guide),
            tab_width: num("tab_width").map_or(d.tab_width, |n| n.clamp(0, 1000) as u32),
            use_spaces: flag("use_spaces", d.use_spaces),
            auto_indent: flag("auto_indent", d.auto_indent),
        }
        .clamped()
    }

    fn to_value(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("wrap".into(), self.wrap.name().into());
        m.insert("wrap_column".into(), self.wrap_column.into());
        m.insert("font".into(), self.font.as_str().into());
        m.insert("font_size".into(), self.font_size.into());
        m.insert("zoom".into(), self.zoom.into());
        m.insert("show_whitespace".into(), self.show_whitespace.into());
        m.insert("show_eol".into(), self.show_eol.into());
        m.insert("line_numbers".into(), self.line_numbers.into());
        m.insert("status_bar".into(), self.status_bar.into());
        m.insert(
            "highlight_current_line".into(),
            self.highlight_current_line.into(),
        );
        m.insert("highlight_braces".into(), self.highlight_braces.into());
        m.insert("edge_guide".into(), self.edge_guide.into());
        m.insert("tab_width".into(), self.tab_width.into());
        m.insert("use_spaces".into(), self.use_spaces.into());
        m.insert("auto_indent".into(), self.auto_indent.into());
        m
    }
}

/// 印刷の設定（ページ設定と印刷の選択肢。`settings.json` の `print`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintSettings {
    /// 余白（0.01 mm）。
    pub margins: Margins,
    /// 用紙の向きが横。
    pub landscape: bool,
    /// 用紙の種類（Windows の `DMPAPER_*`。0 はプリンターの既定）。
    pub paper_size: u32,
    /// 色分けして印刷する（偽なら白黒）。
    pub color: bool,
    pub line_numbers: bool,
    /// ヘッダー（ファイル名またはタブ名と印刷日時）。
    pub header: bool,
    /// フッター（ページ番号）。
    pub footer: bool,
}

impl Default for PrintSettings {
    fn default() -> Self {
        PrintSettings {
            margins: Margins::default(),
            landscape: false,
            paper_size: 0,
            color: true,
            line_numbers: false,
            header: true,
            footer: true,
        }
    }
}

impl PrintSettings {
    fn from_value(map: &Map<String, Value>) -> Self {
        let d = PrintSettings::default();
        let flag = |k: &str, default: bool| map.get(k).and_then(Value::as_bool).unwrap_or(default);
        let mm = |k: &str, default: i32| {
            map.get(k)
                .and_then(Value::as_i64)
                .map_or(default, |n| n.clamp(0, i64::from(MARGIN_MAX)) as i32)
        };
        PrintSettings {
            margins: Margins {
                left: mm("margin_left", d.margins.left),
                top: mm("margin_top", d.margins.top),
                right: mm("margin_right", d.margins.right),
                bottom: mm("margin_bottom", d.margins.bottom),
            },
            landscape: flag("landscape", d.landscape),
            paper_size: map
                .get("paper_size")
                .and_then(Value::as_i64)
                .map_or(d.paper_size, |n| n.clamp(0, 1000) as u32),
            color: flag("color", d.color),
            line_numbers: flag("line_numbers", d.line_numbers),
            header: flag("header", d.header),
            footer: flag("footer", d.footer),
        }
    }

    fn to_value(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("margin_left".into(), self.margins.left.into());
        m.insert("margin_top".into(), self.margins.top.into());
        m.insert("margin_right".into(), self.margins.right.into());
        m.insert("margin_bottom".into(), self.margins.bottom.into());
        m.insert("landscape".into(), self.landscape.into());
        m.insert("paper_size".into(), self.paper_size.into());
        m.insert("color".into(), self.color.into());
        m.insert("line_numbers".into(), self.line_numbers.into());
        m.insert("header".into(), self.header.into());
        m.insert("footer".into(), self.footer.into());
        m
    }
}

/// 新規文書（無題・クリップボード由来）の既定の形式（`settings.json` の `new_file`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewFileSettings {
    pub format: TextFormat,
}

impl Default for NewFileSettings {
    fn default() -> Self {
        NewFileSettings {
            format: TextFormat::DEFAULT,
        }
    }
}

/// 設定ファイルの文字コード名（BOM の有無を含む）から（文字コード, BOM）。
/// `cp932` と `shift-jis` も受け付ける。
pub fn parse_new_file_encoding(name: &str) -> Option<(Encoding, bool)> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => (Encoding::Utf8, false),
        "utf-8-bom" => (Encoding::Utf8, true),
        // UTF-16 は Windows の慣例どおり BOM を付ける。
        "utf-16le" => (Encoding::Utf16Le, true),
        "utf-16be" => (Encoding::Utf16Be, true),
        "shift_jis" | "shift-jis" | "sjis" | "cp932" => (Encoding::Cp932, false),
        _ => return None,
    })
}

/// [`parse_new_file_encoding`] の逆（設定ファイルに書く名前）。
pub fn new_file_encoding_name(format: &TextFormat) -> &'static str {
    match (format.encoding, format.bom) {
        (Encoding::Utf8, false) => "utf-8",
        (Encoding::Utf8, true) => "utf-8-bom",
        (Encoding::Utf16Le, _) => "utf-16le",
        (Encoding::Utf16Be, _) => "utf-16be",
        (Encoding::Cp932, _) => "shift_jis",
    }
}

fn eol_name(eol: Eol) -> &'static str {
    match eol {
        Eol::CrLf => "crlf",
        Eol::Lf => "lf",
        Eol::Cr => "cr",
    }
}

fn parse_eol_name(name: &str) -> Option<Eol> {
    match name.trim().to_ascii_lowercase().as_str() {
        "crlf" => Some(Eol::CrLf),
        "lf" => Some(Eol::Lf),
        "cr" => Some(Eol::Cr),
        _ => None,
    }
}

impl NewFileSettings {
    /// 不正な値は既定として読む。
    fn from_value(map: &Map<String, Value>) -> Self {
        let mut format = TextFormat::DEFAULT;
        if let Some((encoding, bom)) = map
            .get("encoding")
            .and_then(Value::as_str)
            .and_then(parse_new_file_encoding)
        {
            format.encoding = encoding;
            format.bom = bom;
        }
        if let Some(eol) = map
            .get("eol")
            .and_then(Value::as_str)
            .and_then(parse_eol_name)
        {
            format.eol = eol;
        }
        NewFileSettings { format }
    }

    fn to_value(self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "encoding".into(),
            new_file_encoding_name(&self.format).into(),
        );
        m.insert("eol".into(), eol_name(self.format.eol).into());
        m
    }
}

/// 保存時の整形（`settings.json` の `save`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveSettings {
    /// 行末の空白を除く（Markdown の文書は除く）。
    pub trim_trailing_whitespace: bool,
    /// 最終行に改行を補う。
    pub ensure_final_newline: bool,
    /// 無題の文書の保存先をメモにする（`Ctrl+S` でファイルの保存先を尋ねず、メモにする）。
    pub untitled_to_memo: bool,
}

impl SaveSettings {
    fn from_value(map: &Map<String, Value>) -> Self {
        let flag = |k: &str| map.get(k).and_then(Value::as_bool).unwrap_or(false);
        SaveSettings {
            trim_trailing_whitespace: flag("trim_trailing_whitespace"),
            ensure_final_newline: flag("ensure_final_newline"),
            untitled_to_memo: flag("untitled_to_memo"),
        }
    }

    fn to_value(self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "trim_trailing_whitespace".into(),
            self.trim_trailing_whitespace.into(),
        );
        m.insert(
            "ensure_final_newline".into(),
            self.ensure_final_newline.into(),
        );
        m.insert("untitled_to_memo".into(), self.untitled_to_memo.into());
        m
    }
}

/// ファイルの外部変更を見つけたときの動作。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExternalChangeMode {
    #[default]
    Notify,
    ReloadIfClean,
}

impl ExternalChangeMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::ReloadIfClean => "reload_if_clean",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "notify" => Some(Self::Notify),
            "reload_if_clean" => Some(Self::ReloadIfClean),
            _ => None,
        }
    }
}

/// Grep の入力欄の設定。入力の解釈は検索モジュールが行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepSettings {
    pub extensions: String,
    pub exclude_dirs: String,
    pub include_hidden: bool,
}

impl Default for GrepSettings {
    fn default() -> Self {
        Self {
            extensions: String::new(),
            exclude_dirs: ".git;.hg;.svn;node_modules;target".into(),
            include_hidden: false,
        }
    }
}

impl GrepSettings {
    fn from_value(map: &Map<String, Value>) -> Self {
        let defaults = Self::default();
        Self {
            extensions: map
                .get("extensions")
                .and_then(Value::as_str)
                .unwrap_or(&defaults.extensions)
                .to_owned(),
            exclude_dirs: map
                .get("exclude_dirs")
                .and_then(Value::as_str)
                .unwrap_or(&defaults.exclude_dirs)
                .to_owned(),
            include_hidden: map
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    fn to_value(&self, base: Option<&Value>) -> Value {
        let mut map = base.and_then(Value::as_object).cloned().unwrap_or_default();
        map.insert("extensions".into(), self.extensions.clone().into());
        map.insert("exclude_dirs".into(), self.exclude_dirs.clone().into());
        map.insert("include_hidden".into(), self.include_hidden.into());
        Value::Object(map)
    }
}

/// メモの一覧（小窓）の設定（`settings.json` の `memo_panel`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoPanelSettings {
    /// 各メモの内容を見せる（広い窓は右の本文、狭い窓は 2 行目の冒頭）。隠すと名前と日時だけ。
    pub show_content: bool,
    /// 前回の小窓の位置と大きさ（画面座標。`maximized` は使わない）。
    pub rect: Option<WindowSettings>,
}

impl Default for MemoPanelSettings {
    fn default() -> Self {
        MemoPanelSettings {
            show_content: true,
            rect: None,
        }
    }
}

impl MemoPanelSettings {
    fn from_value(v: &Value) -> Self {
        let mut s = MemoPanelSettings::default();
        if let Value::Object(map) = v {
            if let Some(b) = map.get("show_content").and_then(Value::as_bool) {
                s.show_content = b;
            }
            s.rect = WindowSettings::from_value(map);
        }
        s
    }

    fn to_value(self, base: Option<&Value>) -> Value {
        let mut map = base.and_then(Value::as_object).cloned().unwrap_or_default();
        map.insert("show_content".into(), self.show_content.into());
        for k in ["x", "y", "width", "height", "maximized"] {
            map.remove(k);
        }
        if let Some(r) = self.rect {
            let mut r = r.to_value();
            r.remove("maximized");
            map.extend(r);
        }
        Value::Object(map)
    }
}

/// 前回のウィンドウの位置と大きさ（画面座標。最大化していても通常状態の矩形を持つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSettings {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

/// 保存したウィンドウの幅・高さの下限（これより小さい値は壊れているとみなす）。
pub const WINDOW_MIN_SIZE: i32 = 200;

impl WindowSettings {
    fn from_value(map: &Map<String, Value>) -> Option<Self> {
        let num = |k: &str| {
            map.get(k)
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
        };
        let w = WindowSettings {
            x: num("x")?,
            y: num("y")?,
            width: num("width")?,
            height: num("height")?,
            maximized: map
                .get("maximized")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        (w.width >= WINDOW_MIN_SIZE && w.height >= WINDOW_MIN_SIZE).then_some(w)
    }

    fn to_value(self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("x".into(), self.x.into());
        m.insert("y".into(), self.y.into());
        m.insert("width".into(), self.width.into());
        m.insert("height".into(), self.height.into());
        m.insert("maximized".into(), self.maximized.into());
        m
    }

    /// 矩形を、モニターの作業領域 `work`（左, 上, 右, 下）の内側に収める。作業領域より大きければ
    /// 作業領域に合わせて縮め、はみ出していれば寄せる（画面外に出た窓を取り戻せるように）。
    pub fn fitted(self, work: (i32, i32, i32, i32)) -> Self {
        let fit = |pos: i32, len: i32, lo: i32, hi: i32| {
            let len = len.min((hi - lo).max(1));
            (pos.clamp(lo, (hi - len).max(lo)), len)
        };
        let (x, width) = fit(self.x, self.width, work.0, work.2);
        let (y, height) = fit(self.y, self.height, work.1, work.3);
        WindowSettings {
            x,
            y,
            width,
            height,
            maximized: self.maximized,
        }
    }
}

/// 検索欄・置換欄・Grep の起点欄の履歴（各欄、新しい順、最大 [`MAX_SEARCH_HISTORY`] 件）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchHistory {
    pub find: Vec<String>,
    pub replace: Vec<String>,
    pub grep_dir: Vec<String>,
}

/// 履歴の先頭に `text` を置く（重複は先頭へ移す。空文字列は無視）。変化したら `true`。
fn push_history(list: &mut Vec<String>, text: &str) -> bool {
    if text.is_empty() || list.first().is_some_and(|f| f == text) {
        return false;
    }
    list.retain(|p| p != text);
    list.insert(0, text.to_owned());
    list.truncate(MAX_SEARCH_HISTORY);
    true
}

impl SearchHistory {
    pub fn push_find(&mut self, text: &str) -> bool {
        push_history(&mut self.find, text)
    }

    pub fn push_replace(&mut self, text: &str) -> bool {
        push_history(&mut self.replace, text)
    }

    pub fn push_grep_dir(&mut self, text: &str) -> bool {
        push_history(&mut self.grep_dir, text)
    }

    fn list_from(value: Option<&Value>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(Value::Array(items)) = value {
            for item in items {
                if let Value::String(s) = item {
                    if !s.is_empty() && out.len() < MAX_SEARCH_HISTORY && !out.contains(s) {
                        out.push(s.clone());
                    }
                }
            }
        }
        out
    }

    /// 壊れた値（オブジェクトでない、配列でない要素）は読み飛ばす。
    fn from_value(value: &Value) -> Self {
        let Value::Object(map) = value else {
            return Self::default();
        };
        SearchHistory {
            find: Self::list_from(map.get("find")),
            replace: Self::list_from(map.get("replace")),
            grep_dir: Self::list_from(map.get("grep_dir")),
        }
    }

    fn to_value(&self, base: Option<&Value>) -> Value {
        // 知らない項目は残し、この 3 項目だけ差し替える。
        let mut m = match base {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        for (k, v) in [
            ("find", &self.find),
            ("replace", &self.replace),
            ("grep_dir", &self.grep_dir),
        ] {
            m.insert(
                k.into(),
                Value::Array(v.iter().map(|s| Value::from(s.as_str())).collect()),
            );
        }
        Value::Object(m)
    }
}

/// 設定ファイルの更新時刻と長さ（再読み込みが要るかを調べるための印。V4-1）。
/// ファイルがなければ [`SettingsStamp::of`] は `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

impl SettingsStamp {
    /// ファイルの印。ファイルがない・調べられないときは `None`。
    pub fn of(path: &Path) -> Option<SettingsStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(SettingsStamp {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

/// 前回読んだときの印 `last` と今の印 `now` を比べ、ファイルが変わっている（作られた・消えた・
/// 更新時刻か長さが違う）なら `true`。
pub fn stamp_changed(last: Option<SettingsStamp>, now: Option<SettingsStamp>) -> bool {
    last != now
}

/// 2 つの設定の違い（どの項目が変わったか）。再読み込みで、変わった部分だけを反映するのに使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsChanges {
    pub theme: bool,
    pub keys: bool,
    pub view: bool,
    pub print: bool,
    pub restore_session: bool,
    pub open_in_existing_window: bool,
    pub new_file: bool,
    pub save: bool,
    pub recent_files: bool,
    pub recent_files_max: bool,
    pub external_change: bool,
    pub grep: bool,
    pub search_history: bool,
    pub window: bool,
    pub memo_panel: bool,
    pub mcp: bool,
    /// 知らない項目（`version` を含む）。
    pub other: bool,
}

impl SettingsChanges {
    /// 何も変わっていない。
    pub fn is_empty(&self) -> bool {
        *self == SettingsChanges::default()
    }

    /// 利用者に見せる、変わった項目の名前（`window` と知らない項目は含めない）。
    pub fn labels(&self) -> Vec<&'static str> {
        [
            (self.theme, "テーマ"),
            (self.view, "表示"),
            (self.keys, "キー割り当て"),
            (self.print, "印刷"),
            (self.restore_session, "前回のタブの復元"),
            (self.open_in_existing_window, "既存のウィンドウで開く"),
            (self.new_file, "新規文書の既定"),
            (self.save, "保存時の整形"),
            (
                self.recent_files || self.recent_files_max,
                "最近使ったファイル",
            ),
            (self.external_change, "外部変更時の動作"),
            (self.grep, "Grep の絞り込み"),
            (self.search_history, "検索履歴"),
            (self.mcp, "MCP"),
        ]
        .into_iter()
        .filter_map(|(changed, label)| changed.then_some(label))
        .collect()
    }
}

/// 設定。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: ThemeMode,
    /// コマンド名 → キー表記。空文字列は「割り当てなし」。
    pub keys: BTreeMap<String, String>,
    /// MCP 連携を有効にするか（既定は無効。次の版で使う）。
    pub mcp_enabled: bool,
    /// 最近使ったファイル（新しい順）。
    pub recent_files: Vec<String>,
    /// 記録する最近使ったファイルの件数（0〜30、既定10）。
    pub recent_files_max: usize,
    pub external_change: ExternalChangeMode,
    pub grep: GrepSettings,
    /// 表示の設定。
    pub view: ViewSettings,
    /// 印刷の設定。
    pub print: PrintSettings,
    /// 起動時に前回のタブを開き直す。
    pub restore_session: bool,
    /// 起動時に、既存のウィンドウがあればそこで開く。
    pub open_in_existing_window: bool,
    /// 新規文書の既定の形式。
    pub new_file: NewFileSettings,
    /// 保存時の整形。
    pub save: SaveSettings,
    /// 前回のウィンドウの位置と大きさ。
    pub window: Option<WindowSettings>,
    /// メモの一覧（小窓）の設定。
    pub memo_panel: MemoPanelSettings,
    /// 検索・置換・Grep の履歴。
    pub search_history: SearchHistory,
    /// このプログラムが知らない項目。読み込んだまま書き戻す。
    extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeMode::default(),
            keys: BTreeMap::new(),
            mcp_enabled: false,
            recent_files: Vec::new(),
            recent_files_max: MAX_RECENT_FILES,
            external_change: ExternalChangeMode::default(),
            grep: GrepSettings::default(),
            view: ViewSettings::default(),
            print: PrintSettings::default(),
            restore_session: true,
            open_in_existing_window: true,
            new_file: NewFileSettings::default(),
            save: SaveSettings::default(),
            window: None,
            memo_panel: MemoPanelSettings::default(),
            search_history: SearchHistory::default(),
            extra: Map::new(),
        }
    }
}

fn corrupt(reason: impl Into<String>) -> Error {
    Error::corrupt("設定ファイル", reason)
}

impl Settings {
    /// JSON から読む。
    pub fn from_json(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).map_err(|e| corrupt(e.to_string()))?;
        let Value::Object(mut map) = value else {
            return Err(corrupt("最上位がオブジェクトではありません"));
        };
        let mut settings = Settings::default();
        match map.remove("version") {
            None => {}
            Some(v) if v.as_u64().is_some_and(|v| v >= 1) => {
                // 新しい版の設定も、知っている項目だけ読む。版の値は書き戻すときに保つ。
                if v.as_u64() != Some(SETTINGS_VERSION) {
                    settings.extra.insert("version".into(), v);
                }
            }
            Some(_) => return Err(corrupt("version が不正です")),
        }
        if let Some(theme) = map.remove("theme") {
            settings.theme = theme
                .as_str()
                .and_then(ThemeMode::from_name)
                .ok_or_else(|| corrupt("theme が不正です"))?;
        }
        if let Some(keys) = map.remove("keys") {
            let Value::Object(keys) = keys else {
                return Err(corrupt("keys がオブジェクトではありません"));
            };
            for (command, key) in keys {
                let Value::String(key) = key else {
                    return Err(corrupt(format!("keys.{command} が文字列ではありません")));
                };
                settings.keys.insert(command, key);
            }
        }
        if let Some(max) = map.remove("recent_files_max") {
            settings.recent_files_max = max
                .as_u64()
                .filter(|&n| n <= RECENT_FILES_LIMIT as u64)
                .map_or(MAX_RECENT_FILES, |n| n as usize);
        }
        if let Some(mode) = map.remove("external_change") {
            settings.external_change = mode
                .as_str()
                .and_then(ExternalChangeMode::from_name)
                .unwrap_or_default();
        }
        if let Some(Value::Object(mut grep)) = map.remove("grep") {
            settings.grep = GrepSettings::from_value(&grep);
            for key in ["extensions", "exclude_dirs", "include_hidden"] {
                grep.remove(key);
            }
            if !grep.is_empty() {
                settings.extra.insert("grep".into(), Value::Object(grep));
            }
        }
        if let Some(recent) = map.remove("recent_files") {
            let Value::Array(items) = recent else {
                return Err(corrupt("recent_files が配列ではありません"));
            };
            for item in items {
                let Value::String(path) = item else {
                    return Err(corrupt("recent_files に文字列でない要素があります"));
                };
                // 新しい順に並んでいる前提で、重複と上限だけ整える。
                if !path.is_empty()
                    && settings.recent_files.len() < settings.recent_files_limit()
                    && !settings
                        .recent_files
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&path))
                {
                    settings.recent_files.push(path);
                }
            }
        }
        if let Some(view) = map.remove("view") {
            let Value::Object(view) = view else {
                return Err(corrupt("view がオブジェクトではありません"));
            };
            settings.view = ViewSettings::from_value(&view);
            // Keep fields added by a newer application version when editing
            // a known visibility flag in this version.
            let mut unknown = view;
            for name in settings.view.to_value().keys() {
                unknown.remove(name);
            }
            if !unknown.is_empty() {
                settings.extra.insert("view".into(), Value::Object(unknown));
            }
        }
        if let Some(print) = map.remove("print") {
            let Value::Object(print) = print else {
                return Err(corrupt("print がオブジェクトではありません"));
            };
            settings.print = PrintSettings::from_value(&print);
        }
        // オブジェクトでなければ既定のまま。
        if let Some(Value::Object(nf)) = map.remove("new_file") {
            settings.new_file = NewFileSettings::from_value(&nf);
        }
        if let Some(Value::Object(sv)) = map.remove("save") {
            settings.save = SaveSettings::from_value(&sv);
        }
        if let Some(v) = map.remove("restore_session") {
            // 真偽値でなければ既定（有効）のまま。
            settings.restore_session = v.as_bool().unwrap_or(true);
        }
        if let Some(v) = map.remove("open_in_existing_window") {
            settings.open_in_existing_window = v.as_bool().unwrap_or(true);
        }
        if let Some(Value::Object(w)) = map.remove("window") {
            settings.window = WindowSettings::from_value(&w);
        }
        if let Some(v) = map.remove("memo_panel") {
            settings.memo_panel = MemoPanelSettings::from_value(&v);
        }
        if let Some(h) = map.remove("search_history") {
            settings.search_history = SearchHistory::from_value(&h);
            // 壊れた値でも、知らない項目は書き戻しで保つ。
            if let Value::Object(mut m) = h {
                for k in ["find", "replace", "grep_dir"] {
                    m.remove(k);
                }
                if !m.is_empty() {
                    settings
                        .extra
                        .insert("search_history".into(), Value::Object(m));
                }
            }
        }
        if let Some(mcp) = map.remove("mcp") {
            let Value::Object(mut mcp) = mcp else {
                return Err(corrupt("mcp がオブジェクトではありません"));
            };
            if let Some(enabled) = mcp.remove("enabled") {
                settings.mcp_enabled = enabled
                    .as_bool()
                    .ok_or_else(|| corrupt("mcp.enabled が不正です"))?;
            }
            if !mcp.is_empty() {
                settings.extra.insert("mcp".into(), Value::Object(mcp));
            }
        }
        settings.extra.extend(map);
        Ok(settings)
    }

    /// JSON にする（整形済み、末尾に改行）。
    pub fn to_json(&self) -> String {
        let mut map = self.extra.clone();
        map.entry("version")
            .or_insert_with(|| SETTINGS_VERSION.into());
        map.insert("theme".into(), self.theme.name().into());
        map.insert(
            "keys".into(),
            Value::Object(
                self.keys
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
                    .collect(),
            ),
        );
        map.insert(
            "recent_files".into(),
            Value::Array(
                self.recent_files
                    .iter()
                    .take(self.recent_files_limit())
                    .map(|p| Value::from(p.as_str()))
                    .collect(),
            ),
        );
        map.insert("recent_files_max".into(), self.recent_files_limit().into());
        map.insert("external_change".into(), self.external_change.name().into());
        let grep = self.grep.to_value(map.get("grep"));
        map.insert("grep".into(), grep);
        let mut view = map
            .remove("view")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        view.extend(self.view.to_value());
        map.insert("view".into(), Value::Object(view));
        map.insert("print".into(), Value::Object(self.print.to_value()));
        map.insert("restore_session".into(), self.restore_session.into());
        map.insert(
            "open_in_existing_window".into(),
            self.open_in_existing_window.into(),
        );
        map.insert("new_file".into(), Value::Object(self.new_file.to_value()));
        map.insert("save".into(), Value::Object(self.save.to_value()));
        match self.window {
            Some(w) => map.insert("window".into(), Value::Object(w.to_value())),
            None => map.remove("window"),
        };
        let panel = self.memo_panel.to_value(map.get("memo_panel"));
        map.insert("memo_panel".into(), panel);
        let history = self.search_history.to_value(map.get("search_history"));
        map.insert("search_history".into(), history);
        let mcp = map
            .entry("mcp")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(mcp) = mcp {
            mcp.insert("enabled".into(), self.mcp_enabled.into());
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(map))
            .expect("JSON の値は常に文字列にできる");
        text.push('\n');
        text
    }

    /// `other` との違い（`self` から `other` に変わったときに何が変わるか）。
    pub fn diff(&self, other: &Settings) -> SettingsChanges {
        SettingsChanges {
            theme: self.theme != other.theme,
            keys: self.keys != other.keys,
            view: self.view != other.view,
            print: self.print != other.print,
            restore_session: self.restore_session != other.restore_session,
            open_in_existing_window: self.open_in_existing_window != other.open_in_existing_window,
            new_file: self.new_file != other.new_file,
            save: self.save != other.save,
            recent_files: self.recent_files != other.recent_files,
            recent_files_max: self.recent_files_max != other.recent_files_max,
            external_change: self.external_change != other.external_change,
            grep: self.grep != other.grep,
            search_history: self.search_history != other.search_history,
            window: self.window != other.window,
            memo_panel: self.memo_panel != other.memo_panel,
            mcp: self.mcp_enabled != other.mcp_enabled,
            other: self.extra != other.extra,
        }
    }

    /// 不正な値を直接設定した場合も、上限を超えて記録しない。
    pub fn recent_files_limit(&self) -> usize {
        self.recent_files_max.min(RECENT_FILES_LIMIT)
    }

    /// 件数の設定を減らしたとき、保存前に一覧にも反映する。
    pub fn truncate_recent_files(&mut self) {
        self.recent_files.truncate(self.recent_files_limit());
    }

    /// 最近使ったファイルの先頭にパスを置く（同じパスは大文字小文字を区別せず 1 つにする）。
    pub fn push_recent(&mut self, path: String) {
        if path.is_empty() {
            return;
        }
        self.recent_files
            .retain(|p| !p.eq_ignore_ascii_case(&path) && p != &path);
        self.recent_files.insert(0, path);
        self.truncate_recent_files();
    }

    /// 最近使ったファイルから外す。
    pub fn remove_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| !p.eq_ignore_ascii_case(path));
    }

    /// コマンドに実際に割り当てるキー。設定の割り当てが定義の既定値より優先する。
    /// 割り当てなしなら `Ok(None)`。設定の表記が誤っていれば `Err`
    /// （呼び出し側は知らせたうえで既定値を使う）。
    pub fn key_for(
        &self,
        def: &CommandDef,
    ) -> std::result::Result<Option<KeyChord>, KeyParseError> {
        match self.keys.get(def.id.as_str()).map(String::as_str) {
            Some("") => Ok(None),
            Some(key) => KeyChord::parse(key).map(Some),
            None => def.default_key.map(KeyChord::parse).transpose(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandId;

    #[test]
    fn empty_object_gives_defaults() {
        let s = Settings::from_json("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.theme, ThemeMode::System);
        assert!(!s.mcp_enabled);
    }

    #[test]
    fn known_fields_round_trip() {
        let mut s = Settings {
            theme: ThemeMode::Dark,
            mcp_enabled: true,
            ..Settings::default()
        };
        s.keys
            .insert("clipboard.new_tab".into(), "Ctrl+Alt+V".into());
        s.keys.insert("file.save".into(), String::new());
        let text = s.to_json();
        assert!(text.ends_with("}\n"));
        assert_eq!(Settings::from_json(&text).unwrap(), s);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["mcp"]["enabled"], true);
    }

    #[test]
    fn open_in_existing_window_defaults_to_true_and_round_trips() {
        let mut s = Settings::default();
        assert!(s.open_in_existing_window);
        s.open_in_existing_window = false;
        let back = Settings::from_json(&s.to_json()).unwrap();
        assert!(!back.open_in_existing_window);
        assert!(
            Settings::from_json(r#"{"open_in_existing_window":"x"}"#)
                .unwrap()
                .open_in_existing_window
        );
        assert!(Settings::default().diff(&s).open_in_existing_window);
    }

    #[test]
    fn restore_session_and_window_round_trip() {
        let mut s = Settings::default();
        assert!(s.restore_session);
        assert_eq!(s.window, None);
        s.restore_session = false;
        s.window = Some(WindowSettings {
            x: -1200,
            y: 40,
            width: 900,
            height: 700,
            maximized: true,
        });
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
        // 不正な値は既定に戻す（設定全体は壊れていない）。
        for bad in [
            r#"{"window":{"x":1,"y":2,"width":10,"height":500}}"#,
            r#"{"window":{"x":"a","y":2,"width":500,"height":500}}"#,
            r#"{"window":{"y":2,"width":500,"height":500}}"#,
            r#"{"window":5}"#,
        ] {
            assert_eq!(Settings::from_json(bad).unwrap().window, None, "{bad}");
        }
        assert!(
            Settings::from_json(r#"{"restore_session":"x"}"#)
                .unwrap()
                .restore_session
        );
        let ok = Settings::from_json(r#"{"window":{"x":1,"y":2,"width":500,"height":400}}"#)
            .unwrap()
            .window
            .unwrap();
        assert!(!ok.maximized);
    }

    #[test]
    fn status_bar_defaults_on_and_round_trips_with_unknown_view_fields() {
        for text in ["{}", r#"{"view":{"status_bar":"off"}}"#] {
            assert!(Settings::from_json(text).unwrap().view.status_bar);
        }
        let s = Settings::from_json(r#"{"view":{"status_bar":false,"future":123}}"#).unwrap();
        assert!(!s.view.status_bar);
        let json = s.to_json();
        assert_eq!(Settings::from_json(&json).unwrap(), s);
        assert!(json.contains("future"));
    }

    #[test]
    fn window_is_fitted_into_the_work_area() {
        let w = |x, y, width, height| WindowSettings {
            x,
            y,
            width,
            height,
            maximized: false,
        };
        let work = (0, 0, 1920, 1040);
        // 収まっていれば変えない。
        assert_eq!(w(100, 50, 800, 600).fitted(work), w(100, 50, 800, 600));
        // 画面外（取り外したモニター）は内側へ寄せる。
        assert_eq!(w(3000, 50, 800, 600).fitted(work), w(1120, 50, 800, 600));
        assert_eq!(w(-2000, -500, 800, 600).fitted(work), w(0, 0, 800, 600));
        assert_eq!(w(100, 900, 800, 600).fitted(work), w(100, 440, 800, 600));
        // 作業領域より大きければ縮める。
        assert_eq!(w(-50, -50, 4000, 3000).fitted(work), w(0, 0, 1920, 1040));
        // 作業領域が原点でないモニター。
        assert_eq!(
            w(100, 100, 500, 400).fitted((1920, 0, 2920, 700)),
            w(1920, 100, 500, 400)
        );
    }

    #[test]
    fn print_settings_round_trip_and_clamp() {
        let mut s = Settings::default();
        s.print.landscape = true;
        s.print.paper_size = 9;
        s.print.color = false;
        s.print.line_numbers = true;
        s.print.margins.left = 1500;
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
        let bad = Settings::from_json(
            r#"{"print":{"margin_left":-4,"margin_top":99999999,"color":"x","paper_size":-1}}"#,
        )
        .unwrap();
        assert_eq!(bad.print.margins.left, 0);
        assert_eq!(bad.print.margins.top, crate::printing::MARGIN_MAX);
        assert!(bad.print.color);
        assert_eq!(bad.print.paper_size, 0);
        assert!(Settings::from_json(r#"{"print":[]}"#).is_err());
    }

    #[test]
    fn new_file_and_save_round_trip_with_defaults_for_bad_values() {
        let d = Settings::from_json("{}").unwrap();
        assert_eq!(d.new_file.format, TextFormat::DEFAULT);
        assert_eq!(d.save, SaveSettings::default());
        let s = Settings::from_json(
            r#"{"new_file":{"encoding":"Shift_JIS","eol":"lf"},"save":{"trim_trailing_whitespace":true,"ensure_final_newline":true}}"#,
        )
        .unwrap();
        assert_eq!(s.new_file.format.encoding, Encoding::Cp932);
        assert!(!s.new_file.format.bom);
        assert_eq!(s.new_file.format.eol, Eol::Lf);
        assert!(s.save.trim_trailing_whitespace && s.save.ensure_final_newline);
        let again = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(again, s);
        assert!(s.to_json().contains("\"shift_jis\""));
        // 不正な値は既定として読み、他の項目は読む。
        let bad = Settings::from_json(
            r#"{"new_file":{"encoding":"latin1","eol":"x"},"save":{"trim_trailing_whitespace":"yes","ensure_final_newline":true}}"#,
        )
        .unwrap();
        assert_eq!(bad.new_file.format, TextFormat::DEFAULT);
        assert!(!bad.save.trim_trailing_whitespace && bad.save.ensure_final_newline);
        let bom = Settings::from_json(r#"{"new_file":{"encoding":"utf-8-bom"}}"#).unwrap();
        assert!(bom.new_file.format.bom);
        assert_eq!(
            parse_new_file_encoding("utf-16le"),
            Some((Encoding::Utf16Le, true))
        );
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let text = r#"{
            "version": 2,
            "theme": "light",
            "future": { "a": [1, 2] },
            "mcp": { "enabled": true, "folders": ["C:\\work"] }
        }"#;
        let mut s = Settings::from_json(text).unwrap();
        s.theme = ThemeMode::Dark;
        let rewritten: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(rewritten["version"], 2);
        assert_eq!(rewritten["theme"], "dark");
        assert_eq!(rewritten["future"]["a"][1], 2);
        assert_eq!(rewritten["mcp"]["folders"][0], "C:\\work");
        assert_eq!(rewritten["mcp"]["enabled"], true);
    }

    #[test]
    fn broken_settings_are_reported_not_defaulted() {
        for bad in [
            "",
            "{",
            "[]",
            r#"{"theme":"blue"}"#,
            r#"{"keys":[]}"#,
            r#"{"keys":{"file.save":1}}"#,
            r#"{"mcp":{"enabled":"yes"}}"#,
            r#"{"version":"1"}"#,
            r#"{"version":0}"#,
            r#"{"recent_files":"a"}"#,
            r#"{"recent_files":[1]}"#,
            r#"{"view":[]}"#,
        ] {
            assert!(
                matches!(Settings::from_json(bad), Err(Error::Corrupt { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn v7_settings_round_trip_and_preserve_unknown_grep_fields() {
        let initial = r#"{"external_change":"reload_if_clean","recent_files_max":30,"grep":{"extensions":"*.rs;*.txt","exclude_dirs":".git;vendor","include_hidden":true,"future":{"a":1}}}"#;
        let settings = Settings::from_json(initial).unwrap();
        assert_eq!(settings.external_change, ExternalChangeMode::ReloadIfClean);
        assert_eq!(settings.recent_files_max, 30);
        assert_eq!(settings.grep.extensions, "*.rs;*.txt");
        assert_eq!(settings.grep.exclude_dirs, ".git;vendor");
        assert!(settings.grep.include_hidden);
        let saved: Value = serde_json::from_str(&settings.to_json()).unwrap();
        assert_eq!(saved["grep"]["future"]["a"], 1);
        assert_eq!(Settings::from_json(&settings.to_json()).unwrap(), settings);
        let changed = Settings::default().diff(&settings);
        assert!(changed.external_change && changed.recent_files_max && changed.grep);
    }

    #[test]
    fn v7_invalid_values_use_defaults() {
        for invalid in [
            r#"{"external_change":"other","recent_files_max":31,"grep":{"extensions":3,"exclude_dirs":null,"include_hidden":"true"}}"#,
            r#"{"external_change":null,"recent_files_max":-1,"grep":true}"#,
            r#"{"external_change":[],"recent_files_max":1.5}"#,
            r#"{"recent_files_max":"10"}"#,
        ] {
            let settings = Settings::from_json(invalid).unwrap();
            assert_eq!(settings.external_change, ExternalChangeMode::Notify);
            assert_eq!(settings.recent_files_max, MAX_RECENT_FILES);
            assert_eq!(settings.grep, GrepSettings::default());
        }
    }

    #[test]
    fn recent_limit_handles_zero_thirty_and_reduced_limit() {
        let mut settings = Settings::from_json(r#"{"recent_files_max":30}"#).unwrap();
        for n in 0..35 {
            settings.push_recent(format!("file-{n}.txt"));
        }
        assert_eq!(settings.recent_files.len(), 30);
        assert_eq!(settings.recent_files[0], "file-34.txt");
        settings.recent_files_max = 2;
        let saved = Settings::from_json(&settings.to_json()).unwrap();
        assert_eq!(saved.recent_files, ["file-34.txt", "file-33.txt"]);
        settings.truncate_recent_files();
        assert_eq!(settings.recent_files.len(), 2);
        settings.recent_files_max = 0;
        settings.push_recent("new.txt".into());
        assert!(settings.recent_files.is_empty());
        let settings =
            Settings::from_json(r#"{"recent_files_max":0,"recent_files":["a","b"]}"#).unwrap();
        assert!(settings.recent_files.is_empty());
    }

    #[test]
    fn recent_files_are_deduplicated_limited_and_round_trip() {
        let mut s = Settings::default();
        for i in 0..12 {
            s.push_recent(format!("C:\\work\\{i}.txt"));
        }
        s.push_recent("c:\\WORK\\5.TXT".into());
        assert_eq!(s.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(s.recent_files[0], "c:\\WORK\\5.TXT");
        assert_eq!(s.recent_files[1], "C:\\work\\11.txt");
        assert!(!s.recent_files.contains(&"C:\\work\\5.txt".to_owned()));
        s.remove_recent("C:\\work\\11.txt");
        assert_eq!(s.recent_files[1], "C:\\work\\10.txt");
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
    }

    #[test]
    fn memo_panel_settings_round_trip_and_tolerate_bad_values() {
        let d = Settings::default();
        assert!(d.memo_panel.show_content && d.memo_panel.rect.is_none());
        let mut s = Settings::default();
        s.memo_panel.show_content = false;
        s.memo_panel.rect = Some(WindowSettings {
            x: 10,
            y: 20,
            width: 760,
            height: 500,
            maximized: false,
        });
        let back = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(back.memo_panel, s.memo_panel);
        assert!(s.diff(&d).memo_panel);
        // 壊れた値は既定（内容を表示、位置なし）で読む。
        for bad in [
            r#"{"memo_panel":5}"#,
            r#"{"memo_panel":{"show_content":"no","x":1,"y":2,"width":10,"height":5}}"#,
        ] {
            let b = Settings::from_json(bad).unwrap();
            assert!(
                b.memo_panel.show_content && b.memo_panel.rect.is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn search_history_is_deduplicated_limited_and_round_trips() {
        let mut s = Settings::default();
        for i in 0..25 {
            assert!(s.search_history.push_find(&format!("w{i}")));
        }
        assert!(!s.search_history.push_find("w24"), "先頭と同じなら変化なし");
        assert!(!s.search_history.push_find(""));
        assert!(s.search_history.push_find("w20"));
        assert_eq!(s.search_history.find.len(), MAX_SEARCH_HISTORY);
        assert_eq!(s.search_history.find[0], "w20");
        assert_eq!(s.search_history.find[1], "w24");
        assert_eq!(
            s.search_history.find.iter().filter(|w| *w == "w20").count(),
            1
        );
        s.search_history.push_replace("x");
        s.search_history.push_grep_dir("C:\\src");
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
    }

    #[test]
    fn broken_search_history_is_ignored_and_unknown_keys_kept() {
        let s = Settings::from_json(r#"{"search_history": 5}"#).unwrap();
        assert_eq!(s.search_history, SearchHistory::default());
        let s = Settings::from_json(
            r#"{"search_history": {"find": ["a", 3, "a", "b"], "future": [1]}}"#,
        )
        .unwrap();
        assert_eq!(s.search_history.find, ["a", "b"]);
        assert!(s.to_json().contains("\"future\""));
    }

    #[test]
    fn view_settings_round_trip_and_clamp() {
        let mut s = Settings::default();
        assert_eq!(s.view, ViewSettings::default());
        s.view = ViewSettings {
            wrap: WrapMode::Column,
            wrap_column: 100,
            font: "Consolas".into(),
            font_size: 14,
            zoom: -3,
            show_whitespace: true,
            show_eol: true,
            line_numbers: false,
            status_bar: false,
            highlight_current_line: false,
            highlight_braces: false,
            edge_guide: false,
            tab_width: 8,
            use_spaces: true,
            auto_indent: false,
        };
        assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);

        // 不正な値は既定値や範囲内に直して読む（設定ファイル全体は壊れていない）。
        let bad = r#"{"view":{"wrap":"x","wrap_column":3,"font_size":999,"zoom":99,
            "tab_width":3,"line_numbers":"no","use_spaces":true}}"#;
        let v = Settings::from_json(bad).unwrap().view;
        assert_eq!(v.wrap, WrapMode::None);
        assert_eq!(v.wrap_column, WRAP_COLUMN_MIN);
        assert_eq!(v.font_size, FONT_SIZE_MAX);
        assert_eq!(v.zoom, ZOOM_MAX);
        assert_eq!(v.tab_width, 4);
        assert!(v.line_numbers);
        assert!(v.use_spaces);
    }

    #[test]
    fn key_override_beats_the_default() {
        let save = CommandDef::new(CommandId::new("file.save"), "保存", "").default_key("Ctrl+S");
        let other = CommandDef::new(CommandId::new("file.other"), "他", "");
        let mut s = Settings::default();
        assert_eq!(s.key_for(&save).unwrap().unwrap().to_string(), "Ctrl+S");
        assert_eq!(s.key_for(&other).unwrap(), None);
        s.keys.insert("file.save".into(), "ctrl+shift+s".into());
        assert_eq!(
            s.key_for(&save).unwrap().unwrap().to_string(),
            "Ctrl+Shift+S"
        );
        s.keys.insert("file.save".into(), String::new());
        assert_eq!(s.key_for(&save).unwrap(), None);
        s.keys.insert("file.save".into(), "Ctrl+Nope".into());
        assert!(s.key_for(&save).is_err());
    }

    // ---------------------------------------------------------------- V4-1: 差分と更新時刻

    #[test]
    fn diff_reports_what_changed() {
        let a = Settings::default();
        assert!(a.diff(&a.clone()).is_empty());
        let mut b = a.clone();
        b.theme = ThemeMode::Dark;
        b.view.wrap = WrapMode::Word;
        b.keys.insert("file.save".into(), "F2".into());
        b.recent_files.push("C:\\a.txt".into());
        b.search_history.push_find("foo");
        b.restore_session = false;
        b.print.landscape = true;
        b.mcp_enabled = true;
        b.window = Some(WindowSettings {
            x: 1,
            y: 2,
            width: 300,
            height: 300,
            maximized: false,
        });
        let d = a.diff(&b);
        assert!(d.theme && d.view && d.keys && d.recent_files && d.search_history);
        assert!(d.restore_session && d.print && d.mcp && d.window);
        assert!(!d.other);
        assert_eq!(
            d.labels(),
            vec![
                "テーマ",
                "表示",
                "キー割り当て",
                "印刷",
                "前回のタブの復元",
                "最近使ったファイル",
                "検索履歴",
                "MCP"
            ]
        );
        // 1 項目だけの違い。
        let mut c = a.clone();
        c.view.zoom = 3;
        let d = a.diff(&c);
        assert_eq!(d.labels(), vec!["表示"]);
        assert!(!d.theme && !d.keys);
        // 知らない項目の違い。
        let x = Settings::from_json(r#"{"future": 1}"#).unwrap();
        let d = a.diff(&x);
        assert!(d.other && d.labels().is_empty() && !d.is_empty());
    }

    #[test]
    fn diff_ignores_json_formatting() {
        let a = Settings::from_json(r#"{"theme":"dark","view":{"wrap":"word"}}"#).unwrap();
        let b = Settings::from_json(&a.to_json()).unwrap();
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn stamp_comparison_detects_changes() {
        let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000);
        let t1 = t0 + std::time::Duration::from_secs(5);
        let s = |m: SystemTime, len| {
            Some(SettingsStamp {
                modified: Some(m),
                len,
            })
        };
        assert!(!stamp_changed(s(t0, 10), s(t0, 10)));
        assert!(stamp_changed(s(t0, 10), s(t1, 10)), "更新時刻");
        assert!(stamp_changed(s(t0, 10), s(t0, 11)), "長さ");
        assert!(stamp_changed(None, s(t0, 10)), "作られた");
        assert!(stamp_changed(s(t0, 10), None), "消えた");
        assert!(!stamp_changed(None, None), "ずっとない");
    }

    #[test]
    fn stamp_of_a_real_file_follows_its_content() {
        let dir = std::env::temp_dir().join(format!("linote-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(SettingsStamp::of(&path), None);
        std::fs::write(&path, "{}").unwrap();
        let first = SettingsStamp::of(&path);
        assert_eq!(first.map(|s| s.len), Some(2));
        assert!(!stamp_changed(first, SettingsStamp::of(&path)));
        std::fs::write(&path, "{ \"theme\": \"dark\" }").unwrap();
        assert!(stamp_changed(first, SettingsStamp::of(&path)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
