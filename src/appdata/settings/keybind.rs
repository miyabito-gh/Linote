//! キー割り当ての編集（V4-2）。設定画面の「キー」ページの中核で、GUI に依存しない。
//!
//! - 設定の `keys` は「コマンド名 → キー表記」。ここにないコマンドは定義の既定値、空文字は
//!   割り当てなし。**既定と同じ値は書かない**（[`set_chord`] が項目を消す）。
//! - 一覧の行（[`key_rows`]）は、分類・名前・今のキー・既定と異なるかを持つ。絞り込みは
//!   [`row_matches`]。
//! - 重複（[`find_conflict`]）は、実際に効くキー（誤った表記は既定値に読み替える。`KeyMap` と同じ）で
//!   調べる。付け替えでは相手の割り当てを外す（[`assign_chord`]）。
//! - 打鍵として受け付けないキー（文字入力を奪うもの、グローバルショートカットに使えないもの）は
//!   [`chord_problem`] が理由を返す。

use crate::command::{CommandDef, Key, KeyChord};

use super::Settings;

/// グローバルショートカット（OS に登録するもの）のコマンド名。修飾キーが要る。
pub const GLOBAL_COMMANDS: &[&str] = &["clipboard.new_tab"];

/// 一覧の行 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    pub id: &'static str,
    /// 分類の表示名（コマンド名の前半から決める）。
    pub category: &'static str,
    pub title: &'static str,
    /// 実際に効いているキー（割り当てなしは `None`）。
    pub chord: Option<KeyChord>,
    pub default: Option<KeyChord>,
    /// 既定と異なるか。
    pub custom: bool,
    /// グローバルショートカットか。
    pub global: bool,
}

impl KeyRow {
    /// 一覧に出すキーの文字（なしは空）。
    pub fn chord_text(&self) -> String {
        self.chord.map(|c| c.to_string()).unwrap_or_default()
    }
}

/// 分類の並び順と表示名（コマンド名の前半）。ここにないものは最後に、前半の名前で並ぶ。
const CATEGORIES: &[(&str, &str)] = &[
    ("file", "ファイル"),
    ("edit", "編集"),
    ("search", "検索"),
    ("view", "表示"),
    ("memo", "メモ"),
    ("print", "印刷"),
    ("clipboard", "クリップボード"),
    ("app", "アプリ"),
];

/// コマンド名の分類。
pub fn category_of(id: &str) -> &'static str {
    let prefix = id.split('.').next().unwrap_or("");
    CATEGORIES
        .iter()
        .find(|(p, _)| *p == prefix)
        .map(|(_, label)| *label)
        .unwrap_or("その他")
}

fn category_rank(id: &str) -> usize {
    let prefix = id.split('.').next().unwrap_or("");
    CATEGORIES
        .iter()
        .position(|(p, _)| *p == prefix)
        .unwrap_or(CATEGORIES.len())
}

/// 実際に効くキー。設定の表記が誤っていれば既定値（`KeyMap` と同じ読み替え）。
pub fn effective_chord(settings: &Settings, def: &CommandDef) -> Option<KeyChord> {
    match settings.key_for(def) {
        Ok(chord) => chord,
        Err(_) => default_chord(def),
    }
}

fn default_chord(def: &CommandDef) -> Option<KeyChord> {
    def.default_key.and_then(|k| KeyChord::parse(k).ok())
}

/// 一覧の行（分類の順、同じ分類の中はコマンド名の順）。
pub fn key_rows(defs: &[CommandDef], settings: &Settings) -> Vec<KeyRow> {
    let mut rows: Vec<KeyRow> = defs
        .iter()
        .map(|def| {
            let chord = effective_chord(settings, def);
            let default = default_chord(def);
            KeyRow {
                id: def.id.as_str(),
                category: category_of(def.id.as_str()),
                title: def.title,
                chord,
                default,
                custom: chord != default,
                global: GLOBAL_COMMANDS.contains(&def.id.as_str()),
            }
        })
        .collect();
    rows.sort_by(|a, b| (category_rank(a.id), a.id).cmp(&(category_rank(b.id), b.id)));
    rows
}

/// 絞り込み。空白で区切った語がすべて、分類・名前・コマンド名・キーのどれかに含まれる
/// （大文字小文字を区別しない）。空なら全部。
pub fn row_matches(row: &KeyRow, filter: &str) -> bool {
    let hay = format!(
        "{} {} {} {}",
        row.category,
        row.title,
        row.id,
        row.chord_text()
    )
    .to_lowercase();
    filter
        .split_whitespace()
        .all(|term| hay.contains(&term.to_lowercase()))
}

/// 割り当ての誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignError {
    /// 知らないコマンド。
    Unknown,
    /// 別のコマンドが使っている（付け替えなら外す相手の、コマンド名と表示名）。
    Conflict(&'static str, &'static str),
    /// このキーは使えない（理由）。
    Rejected(String),
}

/// `chord` を `command` に割り当てられない理由（割り当ててよければ `None`）。
/// 文字入力を奪うキー（修飾キーなしの文字・記号、Enter・Tab・Space・Backspace）と、
/// グローバルショートカットの修飾キーなしを断る。
pub fn chord_problem(command: &str, chord: &KeyChord) -> Option<String> {
    let m = chord.modifiers;
    let types_text = match chord.key {
        Key::Char(_) => true,
        Key::F(_) => false,
        Key::Named(name) => matches!(
            name,
            "Enter"
                | "Tab"
                | "Space"
                | "Backspace"
                | "Plus"
                | "Minus"
                | "Comma"
                | "Period"
                | "Slash"
                | "Semicolon"
                | "Backquote"
                | "BracketRight"
        ),
    };
    if types_text && !(m.ctrl || m.alt || m.win) {
        return Some(format!(
            "{chord} は文字の入力に使うため割り当てられません（Ctrl か Alt を組み合わせてください）"
        ));
    }
    if GLOBAL_COMMANDS.contains(&command) && m.is_empty() {
        return Some(format!(
            "グローバルショートカットには、修飾キー（Ctrl／Alt／Shift／Win）が要ります: {chord}"
        ));
    }
    None
}

/// `command` 以外で、実際に `chord` を使っているコマンド。
pub fn find_conflict(
    defs: &[CommandDef],
    settings: &Settings,
    command: &str,
    chord: &KeyChord,
) -> Option<CommandDef> {
    defs.iter()
        .filter(|d| d.id.as_str() != command)
        .find(|d| effective_chord(settings, d) == Some(*chord))
        .copied()
}

/// `command` のキーを `chord`（なしは `None`）にする。既定と同じなら `keys` の項目を消し、
/// 違えば書く（なしは空文字）。
pub fn set_chord(settings: &mut Settings, def: &CommandDef, chord: Option<KeyChord>) {
    let id = def.id.as_str();
    if chord == default_chord(def) {
        settings.keys.remove(id);
    } else {
        settings.keys.insert(
            id.to_owned(),
            chord.map(|c| c.to_string()).unwrap_or_default(),
        );
    }
}

/// `command` に `chord` を割り当てる。別のコマンドが使っていれば、`swap` が偽なら
/// [`AssignError::Conflict`] を返して何も変えず、真なら相手を割り当てなしにして付け替える。
pub fn assign_chord(
    settings: &mut Settings,
    defs: &[CommandDef],
    command: &str,
    chord: KeyChord,
    swap: bool,
) -> Result<(), AssignError> {
    let def = defs
        .iter()
        .find(|d| d.id.as_str() == command)
        .ok_or(AssignError::Unknown)?;
    if let Some(why) = chord_problem(command, &chord) {
        return Err(AssignError::Rejected(why));
    }
    if let Some(other) = find_conflict(defs, settings, command, &chord) {
        if !swap {
            return Err(AssignError::Conflict(other.id.as_str(), other.title));
        }
        // 相手がグローバルショートカットなら、外す（割り当てなし）ことは常にできる。
        set_chord(settings, &other, None);
    }
    set_chord(settings, def, Some(chord));
    Ok(())
}

/// 割り当てを外す（空文字。既定がなしなら項目を消す）。
pub fn clear_chord(settings: &mut Settings, defs: &[CommandDef], command: &str) {
    if let Some(def) = defs.iter().find(|d| d.id.as_str() == command) {
        set_chord(settings, def, None);
    }
}

/// 既定に戻す（`keys` から項目を消す）。
pub fn reset_chord(settings: &mut Settings, command: &str) {
    settings.keys.remove(command);
}

/// すべて既定に戻す。知らないコマンド名の項目（将来の版のもの）は残す。
pub fn reset_all(settings: &mut Settings, defs: &[CommandDef]) {
    for def in defs {
        settings.keys.remove(def.id.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandId;

    fn defs() -> Vec<CommandDef> {
        vec![
            CommandDef::new(CommandId::new("file.save"), "保存", "x").default_key("Ctrl+S"),
            CommandDef::new(CommandId::new("file.open"), "開く", "x").default_key("Ctrl+O"),
            CommandDef::new(CommandId::new("edit.undo"), "元に戻す", "x").default_key("Ctrl+Z"),
            CommandDef::new(CommandId::new("view.zoom_in"), "拡大", "x"),
            CommandDef::new(CommandId::new("clipboard.new_tab"), "クリップボード", "x")
                .default_key("Ctrl+Alt+V"),
        ]
    }

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn rows_are_grouped_and_show_custom_state() {
        let mut s = Settings::default();
        s.keys.insert("file.save".into(), "F2".into());
        let rows = key_rows(&defs(), &s);
        let ids: Vec<_> = rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [
                "file.open",
                "file.save",
                "edit.undo",
                "view.zoom_in",
                "clipboard.new_tab"
            ]
        );
        let save = &rows[1];
        assert_eq!(save.category, "ファイル");
        assert_eq!(save.chord_text(), "F2");
        assert!(save.custom);
        assert!(!rows[0].custom);
        assert_eq!(rows[3].chord_text(), "");
        assert!(rows[4].global && !rows[0].global);
        assert_eq!(category_of("zzz.x"), "その他");
    }

    #[test]
    fn invalid_setting_reads_as_the_default() {
        let mut s = Settings::default();
        s.keys.insert("file.save".into(), "Ctrl+Nope".into());
        let rows = key_rows(&defs(), &s);
        assert_eq!(rows[1].chord_text(), "Ctrl+S");
        assert!(!rows[1].custom);
    }

    #[test]
    fn filter_matches_every_term_in_any_column() {
        let rows = key_rows(&defs(), &Settings::default());
        let hit = |f: &str| {
            rows.iter()
                .filter(|r| row_matches(r, f))
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(hit("").len(), 5);
        assert_eq!(hit("保存"), ["file.save"]);
        assert_eq!(hit("ctrl+s"), ["file.save"]);
        assert_eq!(hit("ファイル 開く"), ["file.open"]);
        assert_eq!(hit("UNDO"), ["edit.undo"]);
        assert!(hit("存在しない").is_empty());
    }

    #[test]
    fn assignments_normalize_and_omit_defaults() {
        let d = defs();
        let mut s = Settings::default();
        assign_chord(&mut s, &d, "file.save", chord("ctrl+shift+s"), false).unwrap();
        assert_eq!(s.keys["file.save"], "Ctrl+Shift+S");
        // 既定と同じ値に戻せば、項目は消える。
        assign_chord(&mut s, &d, "file.save", chord("Ctrl+S"), false).unwrap();
        assert!(s.keys.is_empty());
        // 既定がないコマンドに付ける。
        assign_chord(&mut s, &d, "view.zoom_in", chord("Ctrl+Plus"), false).unwrap();
        assert_eq!(s.keys["view.zoom_in"], "Ctrl+Plus");
        // 外す: 既定がある → 空文字、既定がない → 項目を消す。
        clear_chord(&mut s, &d, "file.save");
        assert_eq!(s.keys["file.save"], "");
        clear_chord(&mut s, &d, "view.zoom_in");
        assert!(!s.keys.contains_key("view.zoom_in"));
        // 既定に戻す。
        reset_chord(&mut s, "file.save");
        assert!(s.keys.is_empty());
        assert_eq!(
            assign_chord(&mut s, &d, "nope.x", chord("F2"), false),
            Err(AssignError::Unknown)
        );
    }

    #[test]
    fn conflicts_are_detected_and_swapped() {
        let d = defs();
        let mut s = Settings::default();
        let err = assign_chord(&mut s, &d, "file.open", chord("Ctrl+S"), false).unwrap_err();
        let AssignError::Conflict(other, title) = err else {
            panic!("衝突のはず")
        };
        assert_eq!((other, title), ("file.save", "保存"));
        assert!(s.keys.is_empty(), "やめたら何も変えない");
        // 付け替え: 相手は割り当てなしになる。
        assign_chord(&mut s, &d, "file.open", chord("Ctrl+S"), true).unwrap();
        assert_eq!(s.keys["file.open"], "Ctrl+S");
        assert_eq!(s.keys["file.save"], "");
        let rows = key_rows(&d, &s);
        assert_eq!(
            rows.iter().find(|r| r.id == "file.save").unwrap().chord,
            None
        );
        // 付け替えのあとに重複はない。
        assert!(find_conflict(&d, &s, "file.open", &chord("Ctrl+S")).is_none());
        // 既定のキーを持つコマンドの既定のキーを、別のコマンドが取る（持ち主は空になる）。
        // 空の持ち主の既定を取り戻すと、また重複する。
        let err = assign_chord(&mut s, &d, "file.save", chord("Ctrl+S"), false).unwrap_err();
        assert!(matches!(err, AssignError::Conflict("file.open", _)));
    }

    #[test]
    fn typing_keys_and_bare_global_keys_are_rejected() {
        let d = defs();
        let mut s = Settings::default();
        for bad in [
            "A",
            "Shift+A",
            "5",
            "Space",
            "Enter",
            "Tab",
            "Comma",
            "Shift+Plus",
        ] {
            let r = assign_chord(&mut s, &d, "view.zoom_in", chord(bad), false);
            assert!(matches!(r, Err(AssignError::Rejected(_))), "{bad}");
        }
        for ok in [
            "F5",
            "Delete",
            "Shift+Left",
            "Ctrl+A",
            "Alt+Comma",
            "Escape",
        ] {
            assert!(chord_problem("view.zoom_in", &chord(ok)).is_none(), "{ok}");
        }
        // グローバルショートカットは修飾キーが要る（Shift 単独は不可でない: F キーは文字でない）。
        assert!(chord_problem("clipboard.new_tab", &chord("F5")).is_some());
        assert!(chord_problem("clipboard.new_tab", &chord("Shift+F5")).is_none());
        assert!(chord_problem("clipboard.new_tab", &chord("Ctrl+Alt+V")).is_none());
        assert!(s.keys.is_empty());
    }

    #[test]
    fn reset_all_keeps_unknown_commands() {
        let d = defs();
        let mut s = Settings::default();
        s.keys.insert("file.save".into(), "F2".into());
        s.keys.insert("edit.undo".into(), String::new());
        s.keys.insert("future.cmd".into(), "F9".into());
        reset_all(&mut s, &d);
        assert_eq!(s.keys.len(), 1);
        assert_eq!(s.keys["future.cmd"], "F9");
    }

    #[test]
    fn round_trip_through_json_keeps_only_changes() {
        let d = defs();
        let mut s = Settings::default();
        assign_chord(&mut s, &d, "file.open", chord("Ctrl+Alt+O"), false).unwrap();
        clear_chord(&mut s, &d, "edit.undo");
        let back = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(back.keys, s.keys);
        assert_eq!(back.keys.len(), 2);
    }
}
