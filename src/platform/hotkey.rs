//! グローバルショートカット（`RegisterHotKey`）。
//!
//! - キーは [`crate::command::KeyChord`] で受け取る（設定の表記と共通）。
//! - 登録に失敗（他のアプリと競合）したら、その状態を返して画面で伝え、再設定できるようにする。
//! - 押されたら、エディタ内のショートカットと同じコマンドを
//!   [`crate::command::Registry::execute`] で実行する（専用の経路を作らない）。
//!
//! 担当: タスク F。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, IsIconic, SetForegroundWindow, ShowWindow,
    SW_RESTORE, SW_SHOW,
};

use crate::command::{Key, KeyChord};

/// `clipboard.new_tab` の既定の割り当て。
pub const DEFAULT_CLIPBOARD_CHORD: &str = "Ctrl+Alt+V";
/// グローバルショートカットの ID（`WM_HOTKEY` の `wParam`）。
pub const CLIPBOARD_HOTKEY_ID: i32 = 0x4C01;

/// 登録の状態。画面の状態表示に使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyStatus {
    /// 割り当てなし（設定が空）。
    Unset,
    Registered(KeyChord),
    /// 他のアプリが使用中などで登録できなかった。
    Conflict(KeyChord),
    /// 表記を解釈できない、または単独キー・対応しないキー。
    Invalid(String),
}

impl HotkeyStatus {
    /// 利用者に見せる短い文。
    pub fn message(&self) -> String {
        match self {
            Self::Unset => "クリップボード展開のショートカットは未設定です".to_owned(),
            Self::Registered(c) => format!("クリップボード展開: {c}"),
            Self::Conflict(c) => format!(
                "ショートカット {c} は他のアプリが使用中のため登録できません。設定で変更してください"
            ),
            Self::Invalid(s) => format!("ショートカットの表記が正しくありません: {s}"),
        }
    }
}

/// 仮想キーコード。対応しないキーは `None`。
pub fn virtual_key(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u32,
        Key::Char(_) => return None,
        Key::F(n) if (1..=24).contains(&n) => 0x70 + (n as u32 - 1),
        Key::F(_) => return None,
        Key::Named(name) => match name {
            "Enter" => 0x0D,
            "Escape" => 0x1B,
            "Tab" => 0x09,
            "Space" => 0x20,
            "Backspace" => 0x08,
            "Delete" => 0x2E,
            "Insert" => 0x2D,
            "Home" => 0x24,
            "End" => 0x23,
            "PageUp" => 0x21,
            "PageDown" => 0x22,
            "Left" => 0x25,
            "Up" => 0x26,
            "Right" => 0x27,
            "Down" => 0x28,
            "Plus" => 0xBB,
            "Comma" => 0xBC,
            "Minus" => 0xBD,
            "Period" => 0xBE,
            "Slash" => 0xBF,
            "Semicolon" => 0xBA,
            "Backquote" => 0xC0,
            "BracketRight" => 0xDD,
            _ => return None,
        },
    })
}

/// `RegisterHotKey` の修飾キー（連打の繰り返しは無効）。
pub fn modifier_flags(chord: &KeyChord) -> HOT_KEY_MODIFIERS {
    let m = chord.modifiers;
    let mut f = MOD_NOREPEAT;
    if m.ctrl {
        f |= MOD_CONTROL;
    }
    if m.alt {
        f |= MOD_ALT;
    }
    if m.shift {
        f |= MOD_SHIFT;
    }
    if m.win {
        f |= MOD_WIN;
    }
    f
}

/// 1 つのグローバルショートカット。`WM_HOTKEY` は登録した HWND に届く
/// （`wParam == id`）。破棄時に解除する。
pub struct GlobalHotkey {
    hwnd: HWND,
    id: i32,
    status: HotkeyStatus,
    active: bool,
}

impl GlobalHotkey {
    pub fn new(hwnd: HWND, id: i32) -> Self {
        Self {
            hwnd,
            id,
            status: HotkeyStatus::Unset,
            active: false,
        }
    }

    pub fn status(&self) -> &HotkeyStatus {
        &self.status
    }

    /// 設定の表記（空なら解除）で登録し直す。失敗しても状態を返す（再設定できる）。
    pub fn set_from_text(&mut self, text: &str) -> &HotkeyStatus {
        self.unregister();
        let text = text.trim();
        if text.is_empty() {
            self.status = HotkeyStatus::Unset;
            return &self.status;
        }
        match KeyChord::parse(text) {
            Ok(chord) => self.register(chord),
            Err(_) => {
                self.status = HotkeyStatus::Invalid(text.to_owned());
                &self.status
            }
        }
    }

    /// 登録する（既存の登録は解除する）。
    pub fn register(&mut self, chord: KeyChord) -> &HotkeyStatus {
        self.unregister();
        // 修飾キーなしの単独キーはグローバルでは危険なので受け付けない。
        let vk = match virtual_key(chord.key) {
            Some(vk) if !chord.modifiers.is_empty() => vk,
            _ => {
                self.status = HotkeyStatus::Invalid(chord.to_string());
                return &self.status;
            }
        };
        // SAFETY: hwnd は自分のスレッドのウィンドウ。
        let ok = unsafe { RegisterHotKey(Some(self.hwnd), self.id, modifier_flags(&chord), vk) };
        self.active = ok.is_ok();
        self.status = if self.active {
            HotkeyStatus::Registered(chord)
        } else {
            HotkeyStatus::Conflict(chord)
        };
        &self.status
    }

    pub fn unregister(&mut self) {
        if self.active {
            // SAFETY: 登録済みの ID だけ解除する。
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), self.id);
            }
            self.active = false;
        }
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// 別のアプリにフォーカスがあっても `hwnd` を前面に出す。最小化なら復元する。
/// `WM_HOTKEY` を処理している間のプロセスには前面化の権利があるので、通常は
/// `SetForegroundWindow` が通る。`AttachThreadInput` は使わず、失敗は `false` で返す
/// （呼び出し側が状態表示で伝える）。Windows 実機での確認項目。
pub fn bring_to_front(hwnd: HWND) -> bool {
    // SAFETY: hwnd は自分のウィンドウ。
    unsafe {
        let _ = AllowSetForegroundWindow(std::process::id());
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd).as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_keys() {
        let c = KeyChord::parse("Ctrl+Alt+V").unwrap();
        assert_eq!(virtual_key(c.key), Some(0x56));
        assert_eq!(virtual_key(KeyChord::parse("F12").unwrap().key), Some(0x7B));
        assert_eq!(
            virtual_key(KeyChord::parse("Win+Plus").unwrap().key),
            Some(0xBB)
        );
        for name in Key::NAMED {
            assert!(virtual_key(Key::Named(name)).is_some(), "{name}");
        }
    }

    #[test]
    fn default_chord_parses() {
        let c = KeyChord::parse(DEFAULT_CLIPBOARD_CHORD).unwrap();
        assert_eq!(c.to_string(), DEFAULT_CLIPBOARD_CHORD);
        let f = modifier_flags(&c);
        assert!(f.contains(MOD_CONTROL) && f.contains(MOD_ALT) && !f.contains(MOD_SHIFT));
    }
}
