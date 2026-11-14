//! 設定の形式。
//!
//! `settings.json`（UTF-8 の JSON）:
//!
//! ```json
//! {
//!   "version": 1,
//!   "theme": "system",
//!   "keys": { "clipboard.new_tab": "Ctrl+Alt+V", "file.save": "" },
//!   "mcp": { "enabled": false }
//! }
//! ```
//!
//! - `keys`: コマンド名 → キー表記（[`KeyChord`]）。ここにないコマンドは定義の既定値
//!   （[`CommandDef::default_key`]）を使う。空文字列は「割り当てなし」。
//!   グローバルショートカット（クリップボードの展開）も同じ表で、そのコマンドの
//!   割り当てをエディタ内とグローバルの両方に使う。
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

/// 設定。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub theme: ThemeMode,
    /// コマンド名 → キー表記。空文字列は「割り当てなし」。
    pub keys: BTreeMap<String, String>,
    /// MCP 連携を有効にするか（既定は無効。次の版で使う）。
    pub mcp_enabled: bool,
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
        ] {
            assert!(
                matches!(Settings::from_json(bad), Err(Error::Corrupt { .. })),
                "{bad}"
            );
        }
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
