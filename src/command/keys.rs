//! キー割り当ての表記。設定ファイル、コマンドの既定値、グローバルショートカットで共通。
//!
//! 表記は `Ctrl+Shift+V` のように、修飾キーとキー名を `+` でつなぐ。
//! 修飾キーは `Ctrl`、`Alt`、`Shift`、`Win`（この順に正規化）。キー名は
//! `A`〜`Z`、`0`〜`9`、`F1`〜`F24`、および [`Key::NAMED`] の名前。大文字小文字は
//! 区別しない。仮想キーコードとの対応は `ui`／`platform` が持つ（ここは OS に依存しない）。

use std::fmt;

/// 修飾キー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Modifiers {
    pub const fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }
}

/// 修飾キー以外のキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// `A`〜`Z`（大文字で保持）または `0`〜`9`。
    Char(char),
    /// ファンクションキー（1〜24）。
    F(u8),
    /// 名前付きのキー（[`Key::NAMED`] のいずれか。正規の表記で保持）。
    Named(&'static str),
}

impl Key {
    /// 名前付きのキーの一覧（正規の表記）。
    pub const NAMED: [&'static str; 23] = [
        "Enter",
        "Escape",
        "Tab",
        "Space",
        "Backspace",
        "Delete",
        "Insert",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Left",
        "Right",
        "Up",
        "Down",
        "Plus",
        "Minus",
        "Comma",
        "Period",
        "Slash",
        "Semicolon",
        "Backquote",
        "BracketRight",
    ];

    fn parse(s: &str) -> Option<Key> {
        if s == "]" {
            return Some(Key::Named("BracketRight"));
        }
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return (c.is_ascii_alphanumeric()).then(|| Key::Char(c.to_ascii_uppercase()));
        }
        if let Some(n) = s
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u8>().ok())
        {
            return (1..=24).contains(&n).then_some(Key::F(n));
        }
        Self::NAMED
            .into_iter()
            .find(|name| name.eq_ignore_ascii_case(s))
            .map(Key::Named)
    }
}

/// キー割り当て 1 つ（修飾キー＋キー）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub modifiers: Modifiers,
    pub key: Key,
}

/// キー表記の誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParseError(pub String);

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "キーの表記を解釈できません: {}", self.0)
    }
}

impl std::error::Error for KeyParseError {}

impl KeyChord {
    /// 表記を解釈する。最後の要素がキー、それ以外が修飾キー。
    pub fn parse(s: &str) -> Result<Self, KeyParseError> {
        let err = || KeyParseError(s.to_owned());
        let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let key = parts.pop().and_then(Key::parse).ok_or_else(err)?;
        let mut modifiers = Modifiers::default();
        for part in parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                "win" => &mut modifiers.win,
                _ => return Err(err()),
            };
            if std::mem::replace(flag, true) {
                return Err(err());
            }
        }
        Ok(Self { modifiers, key })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Named(name) => f.write_str(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes_and_round_trips() {
        for (input, normalized) in [
            ("Ctrl+S", "Ctrl+S"),
            ("shift + ctrl + v", "Ctrl+Shift+V"),
            ("CONTROL+ALT+f12", "Ctrl+Alt+F12"),
            ("Win+Shift+pagedown", "Shift+Win+PageDown"),
            ("F3", "F3"),
            ("Ctrl+0", "Ctrl+0"),
            ("Ctrl+Plus", "Ctrl+Plus"),
            ("ctrl+]", "Ctrl+BracketRight"),
            ("Ctrl+BracketRight", "Ctrl+BracketRight"),
            ("escape", "Escape"),
        ] {
            let chord = KeyChord::parse(input).unwrap();
            assert_eq!(chord.to_string(), normalized, "{input}");
            assert_eq!(KeyChord::parse(normalized).unwrap(), chord);
        }
        assert!(KeyChord::parse("F3").unwrap().modifiers.is_empty());
        assert_eq!(KeyChord::parse("Ctrl+f").unwrap().key, Key::Char('F'));
    }

    #[test]
    fn parse_rejects_bad_notation() {
        for bad in [
            "",
            "Ctrl",
            "Ctrl+",
            "+S",
            "Ctrl+Ctrl+S",
            "Hyper+S",
            "Ctrl+SS",
            "F0",
            "F25",
            "Ctrl+あ",
            "Ctrl++",
        ] {
            assert!(KeyChord::parse(bad).is_err(), "{bad:?}");
        }
    }
}
