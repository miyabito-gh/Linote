//! メニューとショートカットからコマンドへの対応。
//! どちらも [`crate::command::Invocation::user`] を作って登録表に渡す。キーは
//! [`crate::appdata::Settings::key_for`] で決める。有効・無効の表示は
//! [`crate::command::Registry::check_available`]。
//!
//! - `KeyMap`: キー（[`KeyChord`]）とコマンドの対応。設定の割り当てが定義の既定値に
//!   優先する。メニューの表示とキー入力の解釈の両方がこれを使う。
//! - `MenuModel`: メニューバー（ファイル／編集／検索／表示／メモ）と、メニュー項目 ID
//!   からコマンドへの対応。
//!
//! 担当: タスク I1（統括）。

use std::collections::HashMap;
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, DestroyMenu, DrawMenuBar, EnableMenuItem,
    ModifyMenuW, SetMenu, HMENU, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING,
};

use crate::appdata::Settings;
use crate::command::{CommandDef, CommandId, Key, KeyChord, Modifiers, Registry, Value};
use crate::core::{Error, Result};
use crate::platform::hotkey::virtual_key;
use crate::platform::system::to_wide;
use crate::ui::app::App;

// ------------------------------------------------------------------ キーの対応

/// キーとコマンドの対応。
#[derive(Debug, Default, Clone)]
pub struct KeyMap {
    by_chord: HashMap<KeyChord, CommandId>,
    by_command: HashMap<&'static str, KeyChord>,
}

impl KeyMap {
    /// 定義と設定から作る。表記の誤りや重複は、利用者に知らせる文として返す
    /// （誤った割り当ては無視して既定値を使う。重複は先の定義を優先する）。
    pub fn build<'a>(
        defs: impl Iterator<Item = &'a CommandDef>,
        settings: &Settings,
    ) -> (KeyMap, Vec<String>) {
        let mut map = KeyMap::default();
        let mut problems = Vec::new();
        for def in defs {
            let chord = match settings.key_for(def) {
                Ok(chord) => chord,
                Err(e) => {
                    problems.push(format!("{}: {e}", def.id));
                    // 設定の誤りは既定値で補う。
                    def.default_key.and_then(|k| KeyChord::parse(k).ok())
                }
            };
            let Some(chord) = chord else { continue };
            if let Some(other) = map.by_chord.get(&chord) {
                problems.push(format!("{chord} は {other} と {} で重複しています", def.id));
                continue;
            }
            map.by_chord.insert(chord, def.id);
            map.by_command.insert(def.id.as_str(), chord);
        }
        (map, problems)
    }

    pub fn command_for(&self, chord: &KeyChord) -> Option<CommandId> {
        self.by_chord.get(chord).copied()
    }

    pub fn chord_for(&self, command: CommandId) -> Option<KeyChord> {
        self.by_command.get(command.as_str()).copied()
    }
}

/// 仮想キーコード → キー。
fn key_for_vk(vk: u32) -> Option<Key> {
    static TABLE: OnceLock<HashMap<u32, Key>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = HashMap::new();
        let mut add = |key: Key| {
            if let Some(vk) = virtual_key(key) {
                t.entry(vk).or_insert(key);
            }
        };
        for c in ('A'..='Z').chain('0'..='9') {
            add(Key::Char(c));
        }
        for n in 1..=24 {
            add(Key::F(n));
        }
        for name in Key::NAMED {
            add(Key::Named(name));
        }
        t
    });
    table.get(&vk).copied()
}

/// キー入力（仮想キーと修飾キーの状態）をキー割り当ての表記にする。修飾キー単独や、
/// 対応しないキーは `None`。
pub fn chord_from_input(vk: u32, modifiers: Modifiers) -> Option<KeyChord> {
    Some(KeyChord {
        modifiers,
        key: key_for_vk(vk)?,
    })
}

/// いまの修飾キーの状態。
pub fn current_modifiers() -> Modifiers {
    let down = |vk: i32| {
        // SAFETY: キーの状態を読むだけ。
        unsafe { GetKeyState(vk) < 0 }
    };
    Modifiers {
        ctrl: down(VK_CONTROL.0 as i32),
        alt: down(VK_MENU.0 as i32),
        shift: down(VK_SHIFT.0 as i32),
        win: down(VK_LWIN.0 as i32) || down(VK_RWIN.0 as i32),
    }
}

// ------------------------------------------------------------------ メニューの構成

/// メニュー項目。
enum Item {
    /// 引数なしのコマンド。
    Cmd(CommandId),
    /// 引数つきのコマンド（題名、引数名、値）。
    CmdArg(CommandId, &'static str, &'static str, &'static str),
    Separator,
    Sub(&'static str, &'static [Item]),
    /// 最近使ったファイルの枠（1 始まりの番号）。表示名はメニューを開くときに付ける。
    Recent(u8),
}

struct MenuDef {
    title: &'static str,
    items: &'static [Item],
}

const ID_FILE_NEW: CommandId = CommandId::new("file.new");

/// 最近使ったファイルのコマンド。
pub const ID_FILE_OPEN_RECENT: CommandId = CommandId::new("file.open_recent");

static MENUS: &[MenuDef] = &[
    MenuDef {
        title: "ファイル(&F)",
        items: &[
            Item::Cmd(ID_FILE_NEW),
            Item::Cmd(CommandId::new("file.open")),
            Item::Sub(
                "最近使ったファイル(&R)",
                &[
                    Item::Recent(1),
                    Item::Recent(2),
                    Item::Recent(3),
                    Item::Recent(4),
                    Item::Recent(5),
                    Item::Recent(6),
                    Item::Recent(7),
                    Item::Recent(8),
                    Item::Recent(9),
                    Item::Recent(10),
                ],
            ),
            Item::Separator,
            Item::Cmd(CommandId::new("file.save")),
            Item::Cmd(CommandId::new("file.save_as")),
            Item::Separator,
            Item::Cmd(CommandId::new("file.reload")),
            Item::Sub(
                "文字コードを指定して開き直す(&E)",
                &[
                    Item::CmdArg(
                        CommandId::new("file.reopen_with_encoding"),
                        "UTF-8",
                        "encoding",
                        "utf-8",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.reopen_with_encoding"),
                        "UTF-8 (BOM 付き)",
                        "encoding",
                        "utf-8-bom",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.reopen_with_encoding"),
                        "UTF-16 LE",
                        "encoding",
                        "utf-16le",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.reopen_with_encoding"),
                        "UTF-16 BE",
                        "encoding",
                        "utf-16be",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.reopen_with_encoding"),
                        "Shift_JIS (CP932)",
                        "encoding",
                        "cp932",
                    ),
                ],
            ),
            Item::Sub(
                "保存時の文字コード(&C)",
                &[
                    Item::CmdArg(
                        CommandId::new("file.save_encoding"),
                        "UTF-8",
                        "encoding",
                        "utf-8",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.save_encoding"),
                        "UTF-8 (BOM 付き)",
                        "encoding",
                        "utf-8-bom",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.save_encoding"),
                        "UTF-16 LE",
                        "encoding",
                        "utf-16le",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.save_encoding"),
                        "UTF-16 BE",
                        "encoding",
                        "utf-16be",
                    ),
                    Item::CmdArg(
                        CommandId::new("file.save_encoding"),
                        "Shift_JIS (CP932)",
                        "encoding",
                        "cp932",
                    ),
                ],
            ),
            Item::Sub(
                "保存時の改行(&L)",
                &[
                    Item::CmdArg(
                        CommandId::new("file.save_eol"),
                        "CRLF (Windows)",
                        "eol",
                        "crlf",
                    ),
                    Item::CmdArg(CommandId::new("file.save_eol"), "LF (Unix)", "eol", "lf"),
                    Item::CmdArg(CommandId::new("file.save_eol"), "CR", "eol", "cr"),
                ],
            ),
            Item::Separator,
            Item::Cmd(CommandId::new("file.copy_path")),
            Item::Cmd(CommandId::new("file.reveal_in_explorer")),
            Item::Separator,
            Item::Cmd(CommandId::new("file.new_window")),
            Item::Cmd(CommandId::new("file.close")),
            Item::Cmd(CommandId::new("file.close_others")),
            Item::Cmd(CommandId::new("file.close_all")),
            Item::Separator,
            Item::Cmd(CommandId::new("file.exit")),
        ],
    },
    MenuDef {
        title: "編集(&E)",
        items: &[
            Item::Cmd(CommandId::new("edit.undo")),
            Item::Cmd(CommandId::new("edit.redo")),
            Item::Separator,
            Item::Cmd(CommandId::new("edit.cut")),
            Item::Cmd(CommandId::new("edit.copy")),
            Item::Cmd(CommandId::new("edit.paste")),
            Item::Cmd(CommandId::new("edit.select_all")),
        ],
    },
    MenuDef {
        title: "検索(&S)",
        items: &[
            Item::Cmd(CommandId::new("search.find")),
            Item::Cmd(CommandId::new("search.replace")),
            Item::Cmd(CommandId::new("search.find_next")),
            Item::Cmd(CommandId::new("search.find_prev")),
            Item::Cmd(CommandId::new("search.grep")),
            Item::Separator,
            Item::Cmd(CommandId::new("edit.goto_line")),
        ],
    },
    MenuDef {
        title: "表示(&V)",
        items: &[
            Item::Cmd(CommandId::new("view.next_tab")),
            Item::Cmd(CommandId::new("view.prev_tab")),
            Item::Separator,
            Item::Cmd(CommandId::new("view.toggle_theme")),
            Item::Separator,
            Item::Cmd(CommandId::new("view.preview_pane")),
            Item::Cmd(CommandId::new("view.preview_window")),
        ],
    },
    MenuDef {
        title: "メモ(&M)",
        items: &[
            Item::Cmd(CommandId::new("memo.open_panel")),
            Item::Cmd(CommandId::new("memo.rename")),
            Item::Separator,
            Item::Cmd(CommandId::new("clipboard.new_tab")),
        ],
    },
];

/// メニュー項目 ID の最初の値（メインウィンドウの `WM_COMMAND` で、ほかの部品の ID と
/// 区別する）。
const FIRST_ITEM_ID: u16 = 0x2000;

/// 作ったメニュー項目 1 つ。
#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub id: u16,
    pub command: CommandId,
    pub args: Value,
    popup: isize,
}

/// 作ったメニューバー。
#[derive(Debug)]
pub struct MenuModel {
    bar: isize,
    entries: Vec<MenuEntry>,
}

impl MenuModel {
    /// メニューバーを作り、`hwnd` に付ける。登録されていないコマンドの項目は作らない。
    pub fn install(hwnd: HWND, registry: &Registry<App>, keys: &KeyMap) -> Result<MenuModel> {
        let unavailable = |e: windows::core::Error| {
            Error::unavailable("メニュー", format!("作れませんでした: {e}"))
        };
        let mut entries = Vec::new();
        let mut next_id = FIRST_ITEM_ID;
        // SAFETY: 作ったメニューを自分のウィンドウに付ける。
        let bar = unsafe { CreateMenu() }.map_err(unavailable)?;
        for menu in MENUS {
            // SAFETY: 作ったメニューに項目を足す。
            let popup = unsafe { CreatePopupMenu() }.map_err(unavailable)?;
            fill(
                popup,
                menu.items,
                registry,
                keys,
                &mut entries,
                &mut next_id,
            )?;
            let title = to_wide(menu.title);
            // SAFETY: popup は作ったばかり。所有はメニューバーに移る。
            unsafe {
                AppendMenuW(
                    bar,
                    MF_POPUP | MF_STRING,
                    popup.0 as usize,
                    PCWSTR(title.as_ptr()),
                )
            }
            .map_err(unavailable)?;
        }
        // SAFETY: 自分のウィンドウにメニューバーを付ける。
        unsafe {
            SetMenu(hwnd, Some(bar)).map_err(unavailable)?;
            let _ = DrawMenuBar(hwnd);
        }
        Ok(MenuModel {
            bar: bar.0 as isize,
            entries,
        })
    }

    /// 項目 ID に対応するコマンドと引数。
    pub fn command_for_id(&self, id: u16) -> Option<(CommandId, Value)> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.command, e.args.clone()))
    }

    /// 作った項目の一覧。
    pub fn entries(&self) -> &[MenuEntry] {
        &self.entries
    }

    /// 開こうとしているメニュー（`WM_INITMENUPOPUP` の `wParam`）の項目の有効・無効を
    /// 直す。`available` は項目のコマンドを今実行できるか。
    pub fn update_enabled(&self, popup: HMENU, available: impl Fn(&MenuEntry) -> bool) {
        for e in self.entries.iter().filter(|e| e.popup == popup.0 as isize) {
            let flag = if available(e) { MF_ENABLED } else { MF_GRAYED };
            // SAFETY: 自分のメニューの項目。
            unsafe {
                let _ = EnableMenuItem(popup, e.id as u32, MF_BYCOMMAND | flag);
            }
        }
    }

    /// 開こうとしているメニューのうち、最近使ったファイルの枠の表示名を付け直す。
    /// `label` は番号（1 始まり）に対する表示名で、なければ「(なし)」にする。
    pub fn relabel_recent(&self, popup: HMENU, label: impl Fn(usize) -> Option<String>) {
        for e in self
            .entries
            .iter()
            .filter(|e| e.popup == popup.0 as isize && e.command == ID_FILE_OPEN_RECENT)
        {
            let n = e.args["index"].as_u64().unwrap_or(0) as usize;
            let text = label(n).unwrap_or_else(|| "(なし)".to_owned());
            let wide = to_wide(&text);
            // SAFETY: 自分のメニューの項目。
            unsafe {
                let _ = ModifyMenuW(
                    popup,
                    e.id as u32,
                    MF_BYCOMMAND | MF_STRING,
                    e.id as usize,
                    PCWSTR(wide.as_ptr()),
                );
            }
        }
    }

    /// 項目の有効・無効を全部直す（試験用）。
    pub fn popups(&self) -> Vec<HMENU> {
        let mut v: Vec<isize> = self.entries.iter().map(|e| e.popup).collect();
        v.sort_unstable();
        v.dedup();
        v.into_iter().map(|p| HMENU(p as *mut _)).collect()
    }
}

impl Drop for MenuModel {
    fn drop(&mut self) {
        // ウィンドウが先に破棄された場合は、メニューも一緒に破棄されている（失敗は無視）。
        // SAFETY: 自分が作ったメニュー。
        unsafe {
            let _ = DestroyMenu(HMENU(self.bar as *mut _));
        }
    }
}

fn fill(
    popup: HMENU,
    items: &[Item],
    registry: &Registry<App>,
    keys: &KeyMap,
    entries: &mut Vec<MenuEntry>,
    next_id: &mut u16,
) -> Result<()> {
    let unavailable =
        |e: windows::core::Error| Error::unavailable("メニュー", format!("作れませんでした: {e}"));
    for item in items {
        match item {
            Item::Separator => {
                // SAFETY: 自分のメニューへの追加。
                unsafe { AppendMenuW(popup, MF_SEPARATOR, 0, PCWSTR::null()) }
                    .map_err(unavailable)?;
            }
            Item::Sub(title, children) => {
                // SAFETY: 作ったメニューに項目を足す。
                let sub = unsafe { CreatePopupMenu() }.map_err(unavailable)?;
                fill(sub, children, registry, keys, entries, next_id)?;
                let title = to_wide(title);
                // SAFETY: sub は作ったばかり。
                unsafe {
                    AppendMenuW(
                        popup,
                        MF_POPUP | MF_STRING,
                        sub.0 as usize,
                        PCWSTR(title.as_ptr()),
                    )
                }
                .map_err(unavailable)?;
            }
            Item::Recent(n) => {
                let item_id = *next_id;
                *next_id += 1;
                let wide = to_wide("(なし)");
                // SAFETY: 自分のメニューへの追加。表示名はメニューを開くときに付け直す。
                unsafe {
                    AppendMenuW(
                        popup,
                        MF_STRING | MF_GRAYED,
                        item_id as usize,
                        PCWSTR(wide.as_ptr()),
                    )
                }
                .map_err(unavailable)?;
                entries.push(MenuEntry {
                    id: item_id,
                    command: ID_FILE_OPEN_RECENT,
                    args: serde_json::json!({ "index": *n }),
                    popup: popup.0 as isize,
                });
            }
            Item::Cmd(id) | Item::CmdArg(id, ..) => {
                let Some(def) = registry.find(id.as_str()) else {
                    continue;
                };
                let (title, args) = match item {
                    Item::CmdArg(_, title, name, value) => {
                        ((*title).to_owned(), serde_json::json!({ *name: *value }))
                    }
                    _ => (def.title.to_owned(), Value::Null),
                };
                let text = match keys.chord_for(*id).filter(|_| args.is_null()) {
                    Some(chord) => format!("{title}\t{chord}"),
                    None => title,
                };
                let wide = to_wide(&text);
                let item_id = *next_id;
                *next_id += 1;
                // SAFETY: 自分のメニューへの追加。
                unsafe { AppendMenuW(popup, MF_STRING, item_id as usize, PCWSTR(wide.as_ptr())) }
                    .map_err(unavailable)?;
                entries.push(MenuEntry {
                    id: item_id,
                    command: *id,
                    args,
                    popup: popup.0 as isize,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandId;

    fn def(id: &'static str, key: Option<&'static str>) -> CommandDef {
        let d = CommandDef::new(CommandId::new(id), "t", "d");
        match key {
            Some(k) => d.default_key(k),
            None => d,
        }
    }

    #[test]
    fn key_map_prefers_settings_and_reports_problems() {
        let defs = [
            def("file.save", Some("Ctrl+S")),
            def("file.open", Some("Ctrl+O")),
            def("edit.undo", Some("Ctrl+S")),
            def("edit.redo", None),
        ];
        let mut settings = Settings::default();
        settings.keys.insert("edit.redo".into(), "Ctrl+Y".into());
        settings.keys.insert("file.open".into(), "Ctrl+Nope".into());
        let (map, problems) = KeyMap::build(defs.iter(), &settings);
        let ctrl_s = KeyChord::parse("Ctrl+S").unwrap();
        assert_eq!(map.command_for(&ctrl_s), Some(CommandId::new("file.save")));
        assert_eq!(
            map.chord_for(CommandId::new("edit.redo"))
                .unwrap()
                .to_string(),
            "Ctrl+Y"
        );
        // 誤った割り当ては既定値で補う。
        assert_eq!(
            map.chord_for(CommandId::new("file.open"))
                .unwrap()
                .to_string(),
            "Ctrl+O"
        );
        // 誤りと重複の両方を知らせる。
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn virtual_keys_map_back_to_keys() {
        let m = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(chord_from_input(0x53, m).unwrap().to_string(), "Ctrl+S");
        assert_eq!(chord_from_input(0x09, m).unwrap().to_string(), "Ctrl+Tab");
        assert_eq!(
            chord_from_input(0x7B, Modifiers::default())
                .unwrap()
                .to_string(),
            "F12"
        );
        // 修飾キー単独は対応しない。
        assert!(chord_from_input(0x11, m).is_none());
    }
}
