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
//!   "mcp": { "enabled": false }
//! }
//! ```
//!
//! - `keys`: コマンド名 → キー表記（[`KeyChord`]）。ここにないコマンドは定義の既定値
//!   （[`CommandDef::default_key`]）を使う。空文字列は「割り当てなし」。
//!   グローバルショートカット（クリップボードの展開）も同じ表で、そのコマンドの
//!   割り当てをエディタ内とグローバルの両方に使う。
//! - `recent_files`: 最近使ったファイル（新しい順、最大 [`MAX_RECENT_FILES`] 件）。
//!   存在確認は読み込みでは行わない（選んだときに確かめる）。
//! - `view`: 表示の設定（[`ViewSettings`]）。値が不正な項目は既定値にして読む
//!   （`view` がオブジェクトでなければ壊れているとみなす）。全タブに適用する。
//! - 知らない項目は読み込んだまま保持し、書き戻すときに失わない
//!   （新しい版が書いた設定を古い版が消さないため）。
//! - ファイルがなければ既定値。ファイルは設定を変更して保存するときに初めて作る。
//! - 読めない（壊れている）場合は [`Error::Corrupt`]。呼び出し側は既定値で起動して
//!   知らせ、利用者が設定を変更するまで**ファイルを上書きしない**。
//! - 保存は読み直してから書く: 保存の直前にファイルを読み直し、変更する項目だけを
//!   差し替えて、`file` モジュールの原子的な書き込みで置換する（別ウィンドウの変更を
//!   失わないため）。読み書きの処理自体はここに持たない（統合時に `file` を使って書く）。

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::command::keys::KeyParseError;
use crate::command::{CommandDef, KeyChord};
use crate::core::{Error, Result};

/// 現在の設定の形式の版。
pub const SETTINGS_VERSION: u64 = 1;

/// 最近使ったファイルの最大件数。
pub const MAX_RECENT_FILES: usize = 10;

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

/// 設定。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub theme: ThemeMode,
    /// コマンド名 → キー表記。空文字列は「割り当てなし」。
    pub keys: BTreeMap<String, String>,
    /// MCP 連携を有効にするか（既定は無効。次の版で使う）。
    pub mcp_enabled: bool,
    /// 最近使ったファイル（新しい順）。
    pub recent_files: Vec<String>,
    /// 表示の設定。
    pub view: ViewSettings,
    /// このプログラムが知らない項目。読み込んだまま書き戻す。
    extra: Map<String, Value>,
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
                    && settings.recent_files.len() < MAX_RECENT_FILES
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
                    .map(|p| Value::from(p.as_str()))
                    .collect(),
            ),
        );
        map.insert("view".into(), Value::Object(self.view.to_value()));
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

    /// 最近使ったファイルの先頭にパスを置く（同じパスは大文字小文字を区別せず 1 つにする）。
    pub fn push_recent(&mut self, path: String) {
        if path.is_empty() {
            return;
        }
        self.recent_files
            .retain(|p| !p.eq_ignore_ascii_case(&path) && p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(MAX_RECENT_FILES);
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
}
