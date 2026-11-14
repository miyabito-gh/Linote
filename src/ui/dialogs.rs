//! ファイルを開く・保存、確認、行ジャンプなどのダイアログ。
//!
//! アプリは [`Prompts`] だけを呼ぶ。実際の画面は [`Win32Prompts`]（共通のファイルダイアログ、
//! メッセージボックス、メモリ上に組み立てた入力ダイアログ）。自己診断では
//! [`ScriptedPrompts`] に差し替え、利用者の選択を台本どおりに返す。
//!
//! どのダイアログもモーダルで、メッセージループを回す。アプリの状態を借りたまま呼ばれる
//! ことがある（`ui::app` の冒頭の「再入と変更通知の扱い」を参照）。
//!
//! 担当: タスク I1（統括）。

use crate::command::CommandDef;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, LOGFONTW};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    ChooseFontW, GetOpenFileNameW, GetSaveFileNameW, CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT,
    CF_LIMITSIZE, CF_NOVERTFONTS, CF_SCREENFONTS, CHOOSEFONTW, OFN_ALLOWMULTISELECT, OFN_EXPLORER,
    OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DialogBoxIndirectParamW, EndDialog, GetDlgItem, GetDlgItemTextW, MessageBoxW, SendMessageW,
    SetDlgItemTextW, IDCANCEL, IDNO, IDOK, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_OK, MB_YESNO, MB_YESNOCANCEL, WM_COMMAND, WM_INITDIALOG,
};

use crate::appdata::settings::{FONT_SIZE_MAX, FONT_SIZE_MIN};
use crate::appdata::Settings;
use crate::platform::system::{from_wide, os_from_wide, to_wide};

/// 未保存の文書を閉じるときの利用者の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// 起動時に見つかった復元データへの利用者の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreChoice {
    /// 復元して開く。
    Restore,
    /// 復元データを破棄する。
    Discard,
    /// 今は何もしない（次の起動でもう一度聞く）。
    Later,
}

/// 通知の重さ（アイコンの違い）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
    Error,
}

/// 設定画面（V4-1）が、アプリの設定を読み書きするための窓口。画面は設定の中身を知らず、
/// この窓口を通して現在の設定を受け取り、「適用」で編集した内容を渡す。
pub trait SettingsSession {
    /// いま実行中の設定（画面の初期値。画面を開いた時点の基準にする）。
    fn current(&self) -> Settings;
    /// 保存できない事情（設定ファイルが壊れている、保存場所がない）があれば、画面の上部に
    /// 出す文。なければ `None`。
    fn warning(&self) -> Option<String>;
    /// 編集した設定 `edited` を反映して保存する。`baseline` は編集の前の設定で、これと違う
    /// 項目だけを、最新の設定に差し替える。反映後の（いま実行中の）設定を返す。
    fn apply(&mut self, baseline: &Settings, edited: &Settings) -> Settings;
    /// 登録されているコマンドの定義（「キー」ページの一覧）。
    fn command_defs(&self) -> Vec<CommandDef>;
    /// はい／いいえの確認（キーの付け替え、すべて既定に戻す）。
    fn confirm(&mut self, owner: HWND, caption: &str, text: &str) -> bool;
    /// OK だけの知らせ（割り当てられないキー、ショートカットの登録の失敗など）。
    fn notice(&mut self, owner: HWND, text: &str);
}

/// 利用者に尋ねる・知らせる操作の一式。
pub trait Prompts {
    /// ファイルを開くダイアログ（複数選択可）。取り消しは空。
    fn open_files(&self, owner: HWND, initial_dir: Option<&Path>) -> Vec<PathBuf>;
    /// 名前を付けて保存するダイアログ。既存ファイルへの上書きの確認はここで済ませる。
    fn save_file(
        &self,
        owner: HWND,
        suggested_name: &str,
        initial_dir: Option<&Path>,
    ) -> Option<PathBuf>;
    /// 未保存の文書を閉じる前の確認（保存／保存しない／取り消し）。
    fn close_choice(&self, owner: HWND, doc_title: &str) -> CloseChoice;
    /// はい／いいえの確認。
    fn confirm(&self, owner: HWND, caption: &str, text: &str) -> bool;
    /// OK だけの知らせ。
    fn notice(&self, owner: HWND, kind: NoticeKind, caption: &str, text: &str);
    /// 復元データの扱い。
    fn restore_choice(&self, owner: HWND, text: &str) -> RestoreChoice;
    /// 1 行の入力（行ジャンプ、名前の変更）。取り消しは `None`。
    fn input_text(&self, owner: HWND, caption: &str, label: &str, initial: &str) -> Option<String>;
    /// フォント選択ダイアログ。現在の書体とポイント数を初期値にし、選んだ（書体、ポイント数）を
    /// 返す。取り消しは `None`。
    fn choose_font(&self, owner: HWND, current: &str, points: u32) -> Option<(String, u32)>;
    /// 設定画面（モーダル）。OK・適用で `session.apply` を呼ぶ。
    fn settings_dialog(&self, owner: HWND, session: &mut dyn SettingsSession);
}

/// 台本の 1 手: 設定画面を開いた利用者の操作（`session` の `current`／`apply` を呼ぶ）。
pub type SettingsScript = Box<dyn FnOnce(&mut dyn SettingsSession)>;

// ------------------------------------------------------------------ 台本つきの実装

/// 利用者の選択を台本どおりに返す（自己診断用）。台本が尽きたら、安全な側
/// （開かない、保存しない、閉じない、確認は「いいえ」）を返す。見せた文面は
/// [`ScriptedPrompts::shown`] に残る。
#[derive(Default)]
pub struct ScriptedPrompts {
    pub open: RefCell<VecDeque<Vec<PathBuf>>>,
    pub save: RefCell<VecDeque<Option<PathBuf>>>,
    pub close: RefCell<VecDeque<CloseChoice>>,
    pub confirm: RefCell<VecDeque<bool>>,
    pub restore: RefCell<VecDeque<RestoreChoice>>,
    pub input: RefCell<VecDeque<Option<String>>>,
    pub font: RefCell<VecDeque<Option<(String, u32)>>>,
    /// 設定画面での操作（開くたびに 1 つ使う。尽きたら何もせず閉じる＝キャンセル）。
    pub settings: RefCell<VecDeque<SettingsScript>>,
    /// 見せた文面（種類と本文）。
    pub shown: RefCell<Vec<String>>,
}

impl ScriptedPrompts {
    pub fn new() -> Self {
        Self::default()
    }

    fn record(&self, kind: &str, text: &str) {
        self.shown.borrow_mut().push(format!("{kind}: {text}"));
    }
}

impl Prompts for ScriptedPrompts {
    fn open_files(&self, _: HWND, _: Option<&Path>) -> Vec<PathBuf> {
        self.open.borrow_mut().pop_front().unwrap_or_default()
    }

    fn save_file(&self, _: HWND, name: &str, _: Option<&Path>) -> Option<PathBuf> {
        self.record("save_file", name);
        self.save.borrow_mut().pop_front().flatten()
    }

    fn close_choice(&self, _: HWND, title: &str) -> CloseChoice {
        self.record("close", title);
        self.close
            .borrow_mut()
            .pop_front()
            .unwrap_or(CloseChoice::Cancel)
    }

    fn confirm(&self, _: HWND, _: &str, text: &str) -> bool {
        self.record("confirm", text);
        self.confirm.borrow_mut().pop_front().unwrap_or(false)
    }

    fn notice(&self, _: HWND, _: NoticeKind, _: &str, text: &str) {
        self.record("notice", text);
    }

    fn restore_choice(&self, _: HWND, text: &str) -> RestoreChoice {
        self.record("restore", text);
        self.restore
            .borrow_mut()
            .pop_front()
            .unwrap_or(RestoreChoice::Later)
    }

    fn input_text(&self, _: HWND, _: &str, label: &str, _: &str) -> Option<String> {
        self.record("input", label);
        self.input.borrow_mut().pop_front().flatten()
    }

    fn choose_font(&self, _: HWND, current: &str, points: u32) -> Option<(String, u32)> {
        self.record("font", &format!("{current} {points}"));
        self.font.borrow_mut().pop_front().flatten()
    }

    fn settings_dialog(&self, _: HWND, session: &mut dyn SettingsSession) {
        self.record("settings", &session.warning().unwrap_or_default());
        if let Some(script) = self.settings.borrow_mut().pop_front() {
            script(session);
        }
    }
}

// ------------------------------------------------------------------ Win32 の実装

/// 実際のダイアログ。
pub struct Win32Prompts;

const FILE_BUFFER: usize = 32 * 1024;

/// 「テキストファイル」の種類に含める拡張子。
pub const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "log",
    "md",
    "markdown",
    "csv",
    "tsv",
    "json",
    "xml",
    "html",
    "htm",
    "css",
    "js",
    "ts",
    "rs",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "java",
    "py",
    "ps1",
    "bat",
    "cmd",
    "sql",
    "ini",
    "toml",
    "yaml",
    "yml",
    "cfg",
    "conf",
    "properties",
];

/// ファイルダイアログの種類の指定（「表示名\0パターン\0」の並びを、空の項目で終える）。
/// 先頭（既定）は「すべてのファイル」。
fn filter_wide() -> Vec<u16> {
    let patterns = TEXT_EXTENSIONS
        .iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(";");
    let mut v = Vec::new();
    for part in [
        "すべてのファイル (*.*)",
        "*.*",
        "テキストファイル",
        patterns.as_str(),
    ] {
        v.extend(part.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

fn message_box(owner: HWND, caption: &str, text: &str, style: u32) -> i32 {
    let text = to_wide(text);
    let caption = to_wide(caption);
    // SAFETY: NUL 終端の文字列。モーダルで、戻るまで有効。
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(style),
        )
        .0
    }
}

impl Prompts for Win32Prompts {
    fn open_files(&self, owner: HWND, initial_dir: Option<&Path>) -> Vec<PathBuf> {
        let mut buffer = vec![0u16; FILE_BUFFER];
        let filter = filter_wide();
        let dir = initial_dir.map(|d| to_wide(&d.to_string_lossy()));
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: owner,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            nFilterIndex: 1,
            lpstrFile: PWSTR(buffer.as_mut_ptr()),
            nMaxFile: buffer.len() as u32,
            lpstrInitialDir: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            Flags: OFN_ALLOWMULTISELECT
                | OFN_EXPLORER
                | OFN_FILEMUSTEXIST
                | OFN_PATHMUSTEXIST
                | OFN_HIDEREADONLY,
            ..Default::default()
        };
        // SAFETY: 構造体と、それが指すバッファーは呼び出しの間有効。
        let ok = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
        if !ok {
            return Vec::new();
        }
        parse_multi_select(&buffer)
    }

    fn save_file(
        &self,
        owner: HWND,
        suggested_name: &str,
        initial_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 4096];
        let name = to_wide(suggested_name);
        // 末尾の NUL を除いて写す（残りは 0 のまま）。
        let n = (name.len() - 1).min(buffer.len() - 1);
        buffer[..n].copy_from_slice(&name[..n]);
        let filter = filter_wide();
        let ext = to_wide("txt");
        let dir = initial_dir.map(|d| to_wide(&d.to_string_lossy()));
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: owner,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            nFilterIndex: 1,
            lpstrFile: PWSTR(buffer.as_mut_ptr()),
            nMaxFile: buffer.len() as u32,
            lpstrDefExt: PCWSTR(ext.as_ptr()),
            lpstrInitialDir: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_HIDEREADONLY,
            ..Default::default()
        };
        // SAFETY: 構造体と、それが指すバッファーは呼び出しの間有効。
        let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
        if !ok {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(PathBuf::from(os_from_wide(&buffer[..end])))
    }

    fn close_choice(&self, owner: HWND, doc_title: &str) -> CloseChoice {
        let text = format!(
            "「{doc_title}」には保存していない変更があります。\n\n\
             はい: 保存してから閉じる\nいいえ: 保存せずに閉じる\nキャンセル: 閉じない"
        );
        match message_box(owner, "Linote", &text, MB_YESNOCANCEL.0 | MB_ICONWARNING.0) {
            r if r == IDYES.0 => CloseChoice::Save,
            r if r == IDNO.0 => CloseChoice::Discard,
            _ => CloseChoice::Cancel,
        }
    }

    fn confirm(&self, owner: HWND, caption: &str, text: &str) -> bool {
        message_box(owner, caption, text, MB_YESNO.0 | MB_ICONWARNING.0) == IDYES.0
    }

    fn notice(&self, owner: HWND, kind: NoticeKind, caption: &str, text: &str) {
        let icon = match kind {
            NoticeKind::Info => MB_ICONINFORMATION,
            NoticeKind::Warning => MB_ICONWARNING,
            NoticeKind::Error => MB_ICONERROR,
        };
        message_box(owner, caption, text, MB_OK.0 | icon.0);
    }

    fn restore_choice(&self, owner: HWND, text: &str) -> RestoreChoice {
        let text = format!(
            "{text}\n\nはい: 復元して開く\nいいえ: 破棄する\nキャンセル: 今は何もしない（次回の起動でもう一度確認します）"
        );
        match message_box(
            owner,
            "Linote - 未保存の内容の復元",
            &text,
            MB_YESNOCANCEL.0 | MB_ICONINFORMATION.0,
        ) {
            r if r == IDYES.0 => RestoreChoice::Restore,
            r if r == IDNO.0 => RestoreChoice::Discard,
            _ => RestoreChoice::Later,
        }
    }

    fn input_text(&self, owner: HWND, caption: &str, label: &str, initial: &str) -> Option<String> {
        input_dialog(owner, caption, label, initial)
    }

    fn settings_dialog(&self, owner: HWND, session: &mut dyn SettingsSession) {
        crate::ui::settings_dialog::run(owner, session, None);
    }

    fn choose_font(&self, owner: HWND, current: &str, points: u32) -> Option<(String, u32)> {
        let mut lf = LOGFONTW {
            // 96 DPI での高さ（負の値は文字の高さ）。ダイアログが DPI に合わせて読み替える。
            lfHeight: -((points as i32 * 96 + 36) / 72),
            lfCharSet: DEFAULT_CHARSET,
            ..Default::default()
        };
        for (dst, src) in lf.lfFaceName[..31].iter_mut().zip(current.encode_utf16()) {
            *dst = src;
        }
        let mut cf = CHOOSEFONTW {
            lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
            hwndOwner: owner,
            lpLogFont: &mut lf,
            iPointSize: points as i32 * 10,
            Flags: CF_SCREENFONTS
                | CF_INITTOLOGFONTSTRUCT
                | CF_FORCEFONTEXIST
                | CF_NOVERTFONTS
                | CF_LIMITSIZE,
            nSizeMin: FONT_SIZE_MIN as i32,
            nSizeMax: FONT_SIZE_MAX as i32,
            ..Default::default()
        };
        // SAFETY: 構造体と、それが指す LOGFONTW は呼び出しの間有効。
        if !unsafe { ChooseFontW(&mut cf) }.as_bool() {
            return None;
        }
        let name = from_wide(&lf.lfFaceName);
        let points = ((cf.iPointSize + 5) / 10).max(1) as u32;
        (!name.is_empty()).then_some((name, points))
    }
}

/// `OFN_ALLOWMULTISELECT | OFN_EXPLORER` の結果を読む。1 件なら完全なパス、複数なら
/// 「フォルダー\0名前\0名前\0\0」。
fn parse_multi_select(buffer: &[u16]) -> Vec<PathBuf> {
    let mut parts: Vec<&[u16]> = Vec::new();
    let mut start = 0;
    for (i, &c) in buffer.iter().enumerate() {
        if c == 0 {
            if i == start {
                break;
            }
            parts.push(&buffer[start..i]);
            start = i + 1;
        }
    }
    match parts.as_slice() {
        [] => Vec::new(),
        [single] => vec![PathBuf::from(os_from_wide(single))],
        [dir, names @ ..] => {
            let dir = PathBuf::from(os_from_wide(dir));
            names
                .iter()
                .map(|n| dir.join(os_from_wide(n)))
                .collect::<Vec<_>>()
        }
    }
}

// ------------------------------------------------------------------ 入力ダイアログ

const ID_INPUT_EDIT: i32 = 1001;
const ID_INPUT_LABEL: i32 = 1002;

struct InputState {
    caption: Vec<u16>,
    label: Vec<u16>,
    initial: Vec<u16>,
    result: Option<String>,
}

fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn push_i16(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn push_wstr(v: &mut Vec<u8>, s: &str) {
    for c in s.encode_utf16() {
        push_u16(v, c);
    }
    push_u16(v, 0);
}

fn align4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// 1 つの部品（`DLGITEMTEMPLATE`）を足す。`class` は 0x0080 ボタン、0x0081 編集、0x0082 静的。
#[allow(clippy::too_many_arguments)]
fn push_item(
    v: &mut Vec<u8>,
    style: u32,
    (x, y, cx, cy): (i16, i16, i16, i16),
    id: u16,
    class: u16,
    text: &str,
) {
    align4(v);
    push_u32(v, style);
    push_u32(v, 0);
    push_i16(v, x);
    push_i16(v, y);
    push_i16(v, cx);
    push_i16(v, cy);
    push_u16(v, id);
    push_u16(v, 0xFFFF);
    push_u16(v, class);
    push_wstr(v, text);
    push_u16(v, 0); // 作成データなし
}

/// 部品を持たないダイアログのひな形（題名なし。大きさはダイアログ単位。部品と大きさは
/// 作成後に呼び出し側が決める）。`DLGTEMPLATE` は DWORD 境界に置く必要があるので、
/// `u32` の配列で返す。
pub(crate) fn blank_dialog_template(cx: i16, cy: i16) -> Vec<u32> {
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_SYSMENU: u32 = 0x0008_0000;
    const DS_MODALFRAME: u32 = 0x80;
    const DS_SETFONT: u32 = 0x40;
    const WS_VISIBLE: u32 = 0x1000_0000;
    let mut v = Vec::new();
    push_u32(
        &mut v,
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE | DS_MODALFRAME | DS_SETFONT,
    );
    push_u32(&mut v, 0);
    push_u16(&mut v, 0); // 部品の数
    for x in [0i16, 0, cx, cy] {
        push_i16(&mut v, x);
    }
    push_u16(&mut v, 0); // メニューなし
    push_u16(&mut v, 0); // 既定のクラス
    push_wstr(&mut v, ""); // 題名（後で設定）
    push_u16(&mut v, 9);
    push_wstr(&mut v, "Yu Gothic UI");
    align4(&mut v);
    let mut aligned = vec![0u32; v.len().div_ceil(4)];
    // SAFETY: aligned は v 以上の長さ。
    unsafe {
        std::ptr::copy_nonoverlapping(v.as_ptr(), aligned.as_mut_ptr() as *mut u8, v.len());
    }
    aligned
}

/// 入力ダイアログのひな形（ダイアログ単位。題名は実行時に `WM_INITDIALOG` で設定する）。
fn input_template() -> Vec<u8> {
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_SYSMENU: u32 = 0x0008_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_TABSTOP: u32 = 0x0001_0000;
    const WS_BORDER: u32 = 0x0080_0000;
    const DS_MODALFRAME: u32 = 0x80;
    const DS_SETFONT: u32 = 0x40;
    const DS_CENTER: u32 = 0x0800;
    const ES_AUTOHSCROLL: u32 = 0x80;
    const BS_DEFPUSHBUTTON: u32 = 0x1;
    const BS_PUSHBUTTON: u32 = 0x0;

    let mut v = Vec::new();
    push_u32(
        &mut v,
        WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME | DS_SETFONT | DS_CENTER,
    );
    push_u32(&mut v, 0);
    push_u16(&mut v, 4); // 部品の数
    for x in [0i16, 0, 210, 72] {
        push_i16(&mut v, x);
    }
    push_u16(&mut v, 0); // メニューなし
    push_u16(&mut v, 0); // 既定のクラス
    push_wstr(&mut v, ""); // 題名（後で設定）
    push_u16(&mut v, 9); // フォントの大きさ
    push_wstr(&mut v, "Yu Gothic UI");
    push_item(
        &mut v,
        WS_CHILD | WS_VISIBLE,
        (10, 8, 190, 10),
        ID_INPUT_LABEL as u16,
        0x0082,
        "",
    );
    push_item(
        &mut v,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
        (10, 22, 190, 14),
        ID_INPUT_EDIT as u16,
        0x0081,
        "",
    );
    push_item(
        &mut v,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
        (94, 48, 50, 15),
        IDOK.0 as u16,
        0x0080,
        "OK",
    );
    push_item(
        &mut v,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
        (150, 48, 50, 15),
        IDCANCEL.0 as u16,
        0x0080,
        "キャンセル",
    );
    v
}

unsafe extern "system" fn input_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            // SAFETY: init_param に渡した InputState は、ダイアログが閉じるまで有効。
            let state = unsafe { &mut *(lparam.0 as *mut InputState) };
            // SAFETY: 自分のダイアログの部品への設定。ポインターは NUL 終端。
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    hwnd,
                    PCWSTR(state.caption.as_ptr()),
                );
                let _ = SetDlgItemTextW(hwnd, ID_INPUT_LABEL, PCWSTR(state.label.as_ptr()));
                let _ = SetDlgItemTextW(hwnd, ID_INPUT_EDIT, PCWSTR(state.initial.as_ptr()));
                windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    lparam.0,
                );
                if let Ok(edit) = GetDlgItem(Some(hwnd), ID_INPUT_EDIT) {
                    let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(edit));
                }
            }
            // フォーカスを自分で置いたので 0。
            0
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == IDOK.0 {
                // SAFETY: WM_INITDIALOG で保存した InputState。
                let state = unsafe {
                    &mut *(windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    ) as *mut InputState)
                };
                let mut buf = vec![0u16; 4096];
                // SAFETY: 出力先は buf。
                let n = unsafe { GetDlgItemTextW(hwnd, ID_INPUT_EDIT, &mut buf) } as usize;
                state.result = Some(from_wide(&buf[..n.min(buf.len())]));
                // SAFETY: 自分のダイアログを閉じる。
                unsafe {
                    let _ = EndDialog(hwnd, IDOK.0 as isize);
                }
                1
            } else if id == IDCANCEL.0 {
                // SAFETY: 自分のダイアログを閉じる。
                unsafe {
                    let _ = EndDialog(hwnd, IDCANCEL.0 as isize);
                }
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn input_dialog(owner: HWND, caption: &str, label: &str, initial: &str) -> Option<String> {
    let template = input_template();
    let mut state = InputState {
        caption: to_wide(caption),
        label: to_wide(label),
        initial: to_wide(initial),
        result: None,
    };
    // SAFETY: 自分のモジュールのハンドル。
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok()?;
    // DLGTEMPLATE は DWORD 境界に置く必要がある。Vec<u8> は 4 バイト境界とは限らないので、
    // u32 の配列に写す。
    let mut aligned = vec![0u32; template.len().div_ceil(4)];
    // SAFETY: aligned は template 以上の長さ。
    unsafe {
        std::ptr::copy_nonoverlapping(
            template.as_ptr(),
            aligned.as_mut_ptr() as *mut u8,
            template.len(),
        );
    }
    // SAFETY: テンプレートと state は呼び出しの間有効。モーダル。
    let r = unsafe {
        DialogBoxIndirectParamW(
            Some(instance.into()),
            aligned.as_ptr() as *const windows::Win32::UI::WindowsAndMessaging::DLGTEMPLATE,
            Some(owner),
            Some(input_proc),
            LPARAM(&mut state as *mut InputState as isize),
        )
    };
    // 所有者が無効のまま残らないようにする（ダイアログが異常終了した場合の保険）。
    // SAFETY: 自分のウィンドウ。
    unsafe {
        let _ = EnableWindow(owner, true);
    }
    if r == IDOK.0 as isize {
        state.result
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(parts: &[&str]) -> Vec<u16> {
        let mut v = Vec::new();
        for p in parts {
            v.extend(p.encode_utf16());
            v.push(0);
        }
        v.push(0);
        v.resize(v.len() + 8, 0);
        v
    }

    #[test]
    fn filter_lists_all_files_first_then_text_files() {
        let f = filter_wide();
        let parts: Vec<String> = f
            .split(|&c| c == 0)
            .filter(|p| !p.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "すべてのファイル (*.*)");
        assert_eq!(parts[1], "*.*");
        assert!(parts[3].starts_with("*.txt;*.log;*.md;"));
        assert!(parts[3].contains("*.properties"));
        assert_eq!(f[f.len() - 2..], [0, 0]);
    }

    #[test]
    fn multi_select_results() {
        assert!(parse_multi_select(&wide(&[])).is_empty());
        assert_eq!(
            parse_multi_select(&wide(&["C:\\a\\b.txt"])),
            vec![PathBuf::from("C:\\a\\b.txt")]
        );
        assert_eq!(
            parse_multi_select(&wide(&["C:\\a", "x.txt", "日本.txt"])),
            vec![
                PathBuf::from("C:\\a\\x.txt"),
                PathBuf::from("C:\\a\\日本.txt")
            ]
        );
    }

    #[test]
    fn dialog_template_is_dword_aligned_per_item() {
        let t = input_template();
        // 先頭は DLGTEMPLATE（スタイル 4 + 拡張 4 + 数 2 + 位置 8）。
        assert_eq!(u16::from_le_bytes([t[8], t[9]]), 4);
        assert!(t.len() > 80);
    }

    #[test]
    fn scripted_prompts_default_to_the_safe_side() {
        let p = ScriptedPrompts::new();
        let owner = HWND::default();
        assert!(p.open_files(owner, None).is_empty());
        assert_eq!(p.close_choice(owner, "a"), CloseChoice::Cancel);
        assert!(!p.confirm(owner, "c", "t"));
        assert_eq!(p.restore_choice(owner, "t"), RestoreChoice::Later);
        p.confirm.borrow_mut().push_back(true);
        assert!(p.confirm(owner, "c", "t"));
        assert_eq!(p.shown.borrow().len(), 4);
    }
}
