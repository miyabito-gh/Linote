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
    SetDlgItemTextW, IDCANCEL, IDNO, IDOK, IDYES, MB_DEFBUTTON2, MB_DEFBUTTON3, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MB_YESNOCANCEL, WM_COMMAND, WM_INITDIALOG,
};

use crate::appdata::settings::{FONT_SIZE_MAX, FONT_SIZE_MIN};
use crate::appdata::Settings;
use crate::platform::system::{dpi_for_window, from_wide, os_from_wide, to_wide};
use crate::ui::theme::Theme;

/// 未保存の文書を閉じるときの利用者の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    /// メモに保存して閉じる（無題の文書などで、保存先を選ばずに残す）。
    SaveAsMemo,
    Discard,
    Cancel,
}

/// 外部変更があるため上書きできないファイルへの操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveConflictChoice {
    SaveAs,
    OpenSavedCopy,
    Cancel,
}

/// 保存されていない内容の復元での利用者の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreChoice {
    /// 復元して開く。
    Restore,
    /// 復元データを破棄する。
    Discard,
    /// 今は何もしない。あとでファイルメニューから開ける。
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
    /// 設定ファイルをタブで開く（設定画面は閉じたあと）。既定は何もしない。
    fn open_settings_file(&mut self) {}
    /// 「キー」ページを最初に開くか（ヘルプ > キーボード ショートカット）。
    fn initial_keys_page(&self) -> bool {
        false
    }
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
    /// 未保存の文書を閉じる前の確認（保存／メモに保存／保存しない／取り消し）。`can_memo` が
    /// 偽のときは「メモに保存」を出さない。
    fn close_choice(&self, owner: HWND, doc_title: &str, can_memo: bool) -> CloseChoice;
    /// はい／いいえの確認。
    fn confirm(&self, owner: HWND, caption: &str, text: &str) -> bool;
    fn confirm_themed(&self, owner: HWND, caption: &str, text: &str, _: &Theme) -> bool {
        self.confirm(owner, caption, text)
    }
    /// 他のプログラムの変更を守ったまま取れる操作。
    fn save_conflict_choice(
        &self,
        owner: HWND,
        doc_title: &str,
        can_preview: bool,
        theme: &Theme,
    ) -> SaveConflictChoice;
    /// OK だけの知らせ。
    fn notice(&self, owner: HWND, kind: NoticeKind, caption: &str, text: &str);
    /// 復元データの扱い。
    fn restore_choice(&self, owner: HWND, text: &str) -> RestoreChoice;
    /// 候補ごとの扱い。台本では既存の一括操作を使う。
    fn restore_selection(
        &self,
        owner: HWND,
        labels: &[String],
        _: &Theme,
    ) -> (RestoreChoice, Vec<usize>) {
        restore_all_choice(self, owner, labels)
    }
    /// 1 行の入力（行ジャンプ、名前の変更）。取り消しは `None`。
    fn input_text(&self, owner: HWND, caption: &str, label: &str, initial: &str) -> Option<String>;
    /// フォント選択ダイアログ。現在の書体とポイント数を初期値にし、選んだ（書体、ポイント数）を
    /// 返す。取り消しは `None`。
    fn choose_font(&self, owner: HWND, current: &str, points: u32) -> Option<(String, u32)>;
    /// 設定画面（モーダル）。OK・適用で `session.apply` を呼ぶ。
    fn settings_dialog(&self, owner: HWND, session: &mut dyn SettingsSession);
}

fn restore_all_choice(
    prompts: &(impl Prompts + ?Sized),
    owner: HWND,
    labels: &[String],
) -> (RestoreChoice, Vec<usize>) {
    let mut lines: Vec<_> = labels.iter().take(8).cloned().collect();
    if labels.len() > 8 {
        lines.push(format!("ほか {} 件", labels.len() - 8));
    }
    let text = format!(
        "保存されていない内容が {} 件あります。\n\n{}",
        labels.len(),
        lines.join("\n")
    );
    (
        prompts.restore_choice(owner, &text),
        (0..labels.len()).collect(),
    )
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
    pub save_conflict: RefCell<VecDeque<SaveConflictChoice>>,
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

    fn close_choice(&self, _: HWND, title: &str, _can_memo: bool) -> CloseChoice {
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

    fn save_conflict_choice(
        &self,
        _: HWND,
        title: &str,
        can_preview: bool,
        _: &Theme,
    ) -> SaveConflictChoice {
        self.record("save_conflict", title);
        match self.save_conflict.borrow_mut().pop_front() {
            Some(SaveConflictChoice::OpenSavedCopy) if !can_preview => SaveConflictChoice::Cancel,
            Some(choice) => choice,
            None => SaveConflictChoice::Cancel,
        }
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

/// 説明つきのボタンで選ばせる確認（Windows のタスク ダイアログ）。`buttons` は（ID、表示）。
/// 戻り値は押したボタンの ID（Esc・閉じるは `cancel_id`）。タスク ダイアログが使えない
/// 環境では `None`（呼び出し側がメッセージ ボックスで代わりに尋ねる）。
fn task_dialog(
    owner: HWND,
    caption: &str,
    instruction: &str,
    content: &str,
    buttons: &[(i32, &str)],
    default_id: i32,
    warning: bool,
) -> Option<i32> {
    use windows::Win32::UI::Controls::{
        TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
        TDF_POSITION_RELATIVE_TO_WINDOW,
    };
    type TaskDialogIndirectFn = unsafe extern "system" fn(
        *const TASKDIALOGCONFIG,
        *mut i32,
        *mut i32,
        *mut windows::core::BOOL,
    ) -> windows::core::HRESULT;
    // コモン コントロール v6（マニフェストで指定）にだけある関数なので、実行時に探す。
    // SAFETY: 名前で関数を探すだけ。見つかった関数は上の型のとおり（Win32 の定義）。
    let f: TaskDialogIndirectFn = unsafe {
        let module =
            windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::w!("comctl32.dll"))
                .ok()?;
        let p = windows::Win32::System::LibraryLoader::GetProcAddress(
            module,
            windows::core::s!("TaskDialogIndirect"),
        )?;
        std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirectFn>(p)
    };
    let caption = to_wide(caption);
    let instruction = to_wide(instruction);
    let content = to_wide(content);
    let labels: Vec<Vec<u16>> = buttons.iter().map(|(_, t)| to_wide(t)).collect();
    let items: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), label)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: PCWSTR(label.as_ptr()),
        })
        .collect();
    let mut config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: PCWSTR(caption.as_ptr()),
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: items.len() as u32,
        pButtons: items.as_ptr(),
        nDefaultButton: default_id,
        ..Default::default()
    };
    // 警告（!）または情報（i）のアイコン（MAKEINTRESOURCE の値）。
    config.Anonymous1.pszMainIcon =
        PCWSTR(if warning { 65535 } else { 65533 } as usize as *const u16);
    let mut pressed = 0i32;
    // SAFETY: 構造体と、それが指す文字列・ボタンは呼び出しの間有効。モーダル。
    let hr = unsafe {
        f(
            &config,
            &mut pressed,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    hr.is_ok().then_some(pressed)
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

    fn close_choice(&self, owner: HWND, doc_title: &str, can_memo: bool) -> CloseChoice {
        const ID_MEMO: i32 = 1001;
        let mut buttons = vec![(IDYES.0, "保存する")];
        if can_memo {
            buttons.push((ID_MEMO, "メモに保存"));
        }
        buttons.push((IDNO.0, "保存しない"));
        buttons.push((IDCANCEL.0, "キャンセル"));
        if let Some(id) = task_dialog(
            owner,
            "Linote",
            &format!("「{doc_title}」への変更を保存しますか？"),
            "保存しないと、変更は失われます。「メモに保存」は、保存先を選ばずにメモとして残します。",
            &buttons,
            IDYES.0,
            true,
        ) {
            return match id {
                i if i == IDYES.0 => CloseChoice::Save,
                ID_MEMO => CloseChoice::SaveAsMemo,
                i if i == IDNO.0 => CloseChoice::Discard,
                _ => CloseChoice::Cancel,
            };
        }
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

    fn confirm_themed(&self, owner: HWND, caption: &str, text: &str, theme: &Theme) -> bool {
        if let Some(id) = themed_choice(
            owner,
            theme,
            caption,
            text,
            &[(IDYES.0, "はい"), (IDNO.0, "いいえ")],
            IDNO.0,
        ) {
            return id == IDYES.0;
        }
        self.confirm(owner, caption, text)
    }

    fn save_conflict_choice(
        &self,
        owner: HWND,
        doc_title: &str,
        can_preview: bool,
        theme: &Theme,
    ) -> SaveConflictChoice {
        let caption = "Linote - ファイルが変更されています";
        let title = format!("「{doc_title}」を上書きできません");
        let content = if can_preview {
            "開いてから別のプログラムで変更されました。保存されている内容を別タブで確認しても、編集中の内容は残ります。"
        } else {
            "元のファイルを確認できません。編集中の内容は別の名前で保存できます。"
        };
        let mut buttons = vec![(IDYES.0, "名前を付けて保存")];
        if can_preview {
            buttons.push((IDNO.0, "保存されている内容を別タブで開く"));
        }
        buttons.push((IDCANCEL.0, "キャンセル"));
        let clicked = themed_choice(
            owner, theme, caption, &format!("{title}\n\n{content}"), &buttons, IDCANCEL.0,
        )
            .unwrap_or_else(|| {
                if can_preview {
                    let text = format!(
                        "{title}\n{content}\n\nはい: 名前を付けて保存\nいいえ: 保存されている内容を別タブで開く"
                    );
                    message_box(
                        owner,
                        caption,
                        &text,
                        MB_YESNOCANCEL.0 | MB_ICONWARNING.0 | MB_DEFBUTTON3.0,
                    )
                } else {
                    let text = format!("{title}\n{content}\n\n別の名前で保存しますか？");
                    message_box(
                        owner,
                        caption,
                        &text,
                        MB_YESNO.0 | MB_ICONWARNING.0 | MB_DEFBUTTON2.0,
                    )
                }
            });
        match clicked {
            i if i == IDYES.0 => SaveConflictChoice::SaveAs,
            i if i == IDNO.0 && can_preview => SaveConflictChoice::OpenSavedCopy,
            _ => SaveConflictChoice::Cancel,
        }
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
        if let Some(id) = task_dialog(
            owner,
            "Linote - 未保存の内容の復元",
            "未保存の内容を復元しますか？",
            text,
            &[
                (IDYES.0, "復元して開く"),
                (IDNO.0, "破棄する"),
                (IDCANCEL.0, "閉じる（あとで復元）"),
            ],
            IDYES.0,
            false,
        ) {
            return match id {
                i if i == IDYES.0 => RestoreChoice::Restore,
                i if i == IDNO.0 => RestoreChoice::Discard,
                _ => RestoreChoice::Later,
            };
        }
        let text = format!(
            "{text}\n\nはい: 復元して開く\nいいえ: 破棄する\nキャンセル: 閉じる（あとで復元）"
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

    fn restore_selection(
        &self,
        owner: HWND,
        labels: &[String],
        theme: &Theme,
    ) -> (RestoreChoice, Vec<usize>) {
        if !labels.is_empty() {
            if let Some(selection) = recovery_picker(owner, labels, theme) {
                return selection;
            }
            let (choice, indices) = restore_all_choice(self, owner, labels);
            if choice == RestoreChoice::Discard {
                let answer = message_box(
                    owner,
                    "Linote - 復元データの破棄",
                    &format!("復元データ {} 件すべてを破棄しますか？", indices.len()),
                    MB_YESNO.0 | MB_ICONWARNING.0 | MB_DEFBUTTON2.0,
                );
                if answer != IDYES.0 {
                    return (RestoreChoice::Later, Vec::new());
                }
            }
            return (choice, indices);
        }
        restore_all_choice(self, owner, labels)
    }

    fn input_text(&self, owner: HWND, caption: &str, label: &str, initial: &str) -> Option<String> {
        input_dialog(owner, caption, label, initial)
    }

    fn settings_dialog(&self, owner: HWND, session: &mut dyn SettingsSession) {
        crate::ui::settings_dialog::run(owner, session, None);
    }

    fn choose_font(&self, owner: HWND, current: &str, points: u32) -> Option<(String, u32)> {
        let mut lf = LOGFONTW {
            // LOGFONT の高さはピクセル。所有ウィンドウのモニターに合わせる。
            lfHeight: font_height(points, dpi_for_window(owner)),
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

/// ポイント数から LOGFONT の文字高へ変換する（負数はセルではなく文字自体の高さ）。
fn font_height(points: u32, dpi: u32) -> i32 {
    -(((u64::from(points) * u64::from(dpi) + 36) / 72).min(i32::MAX as u64) as i32)
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
        WS_CHILD | WS_VISIBLE | 0x80, /* SS_NOPREFIX */
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

const ID_RECOVERY_LIST: i32 = 1001;
const ID_RECOVERY_ALL: i32 = 1002;
const ID_RECOVERY_DISCARD: i32 = 1003;

struct DialogColors<'a> {
    theme: &'a Theme,
    background: windows::Win32::Graphics::Gdi::HBRUSH,
    list: windows::Win32::Graphics::Gdi::HBRUSH,
}

impl<'a> DialogColors<'a> {
    fn new(theme: &'a Theme) -> Self {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::Graphics::Gdi::CreateSolidBrush;
        let brush = |color: crate::ui::theme::Rgb| {
            if theme.dark {
                // SAFETY: ブラシはこのダイアログだけで使い、閉じたあとに破棄する。
                unsafe { CreateSolidBrush(COLORREF(color.colorref())) }
            } else {
                Default::default()
            }
        };
        Self {
            theme,
            background: brush(theme.ui.bar_bg),
            list: brush(theme.editor.bg),
        }
    }

    fn initialize(&self, hwnd: HWND) {
        crate::platform::system::set_dark_title_bar(hwnd, self.theme.dark);
    }

    fn message(
        &self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        labels: &[String],
    ) -> Option<isize> {
        use windows::Win32::Foundation::{COLORREF, RECT};
        use windows::Win32::Graphics::Gdi::{
            SetBkColor, SetBkMode, SetTextColor, HDC, TRANSPARENT,
        };
        use windows::Win32::UI::Controls::{
            DRAWITEMSTRUCT, NMCUSTOMDRAW, NM_CUSTOMDRAW, ODS_FOCUS, ODS_SELECTED,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetParent, SetWindowLongPtrW, DWLP_MSGRESULT, WINDOW_LONG_PTR_INDEX, WM_CTLCOLORDLG,
            WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_NOTIFY,
        };
        if !self.theme.dark && msg != WM_DRAWITEM {
            return None;
        }
        let ui = &self.theme.ui;
        let editor = &self.theme.editor;
        // SAFETY: 各メッセージに対応する DC と構造体は呼び出し中だけ参照する。
        unsafe {
            match msg {
                WM_CTLCOLORDLG => Some(self.background.0 as isize),
                WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX => {
                    let hdc = HDC(wparam.0 as *mut _);
                    let list = msg == WM_CTLCOLORLISTBOX;
                    let bg = if list { editor.bg } else { ui.bar_bg };
                    SetTextColor(hdc, COLORREF(editor.fg.colorref()));
                    SetBkColor(hdc, COLORREF(bg.colorref()));
                    if !list {
                        SetBkMode(hdc, TRANSPARENT);
                    }
                    Some(if list { self.list.0 } else { self.background.0 } as isize)
                }
                WM_DRAWITEM if lparam.0 != 0 => {
                    let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
                    if item.CtlID != ID_RECOVERY_LIST as u32 {
                        return None;
                    }
                    let canvas = crate::ui::window::Canvas {
                        hdc: item.hDC,
                        dpi: dpi_for_window(item.hwndItem),
                    };
                    let rect = item.rcItem;
                    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
                    canvas.fill(
                        &rect,
                        if selected {
                            editor.selection_bg
                        } else {
                            editor.bg
                        },
                    );
                    if item.itemState.0 & ODS_FOCUS.0 != 0 {
                        canvas.fill(
                            &RECT {
                                right: rect.left + canvas.px(2),
                                ..rect
                            },
                            ui.accent,
                        );
                    }
                    if let Some(label) = labels.get(item.itemID as usize) {
                        canvas.text(
                            &RECT {
                                left: rect.left + canvas.px(5),
                                ..rect
                            },
                            label,
                            editor.fg,
                            false,
                        );
                    }
                    Some(1)
                }
                WM_NOTIFY if lparam.0 != 0 => {
                    let header = &*(lparam.0 as *const windows::Win32::UI::Controls::NMHDR);
                    if header.code != NM_CUSTOMDRAW {
                        return None;
                    }
                    let item = &*(lparam.0 as *const NMCUSTOMDRAW);
                    if !draw_dark_button(self.theme, item) {
                        return None;
                    }
                    if let Ok(parent) = GetParent(header.hwndFrom) {
                        // DWLP_MSGRESULT: WM_NOTIFY のカスタム描画結果。
                        SetWindowLongPtrW(
                            parent,
                            WINDOW_LONG_PTR_INDEX(DWLP_MSGRESULT as i32),
                            windows::Win32::UI::Controls::CDRF_SKIPDEFAULT as isize,
                        );
                    }
                    Some(1)
                }
                _ => None,
            }
        }
    }
}

/// BUTTON のキーボード操作とアクセシビリティを保ち、ダーク時の描画だけを置き換える。
pub(super) fn draw_dark_button(
    theme: &Theme,
    item: &windows::Win32::UI::Controls::NMCUSTOMDRAW,
) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::Controls::{
        CDDS_PREPAINT, CDIS_CHECKED, CDIS_DEFAULT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT,
        CDIS_SELECTED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, BM_GETCHECK};
    if !theme.dark || item.dwDrawStage != CDDS_PREPAINT {
        return false;
    }
    let hwnd = item.hdr.hwndFrom;
    let state = item.uItemState;
    let ui = &theme.ui;
    let editor = &theme.editor;
    let frame = if (state & (CDIS_FOCUS | CDIS_DEFAULT)).0 != 0 {
        ui.accent
    } else {
        ui.splitter
    };
    // SAFETY: The notification comes from this BUTTON and the buffer is writable.
    let mut wide = [0u16; 160];
    let (checked, len) = unsafe {
        (
            SendMessageW(hwnd, BM_GETCHECK, None, None).0 == 1,
            GetWindowTextW(hwnd, &mut wide),
        )
    };
    let bg = if checked || (state & (CDIS_SELECTED | CDIS_CHECKED)).0 != 0 {
        editor.selection_bg
    } else if (state & CDIS_HOT).0 != 0 {
        ui.tab_hover_bg
    } else {
        editor.bg
    };
    let color = if (state & CDIS_DISABLED).0 != 0 {
        ui.status_dim
    } else {
        editor.fg
    };
    let canvas = crate::ui::window::Canvas {
        hdc: item.hdc,
        dpi: dpi_for_window(hwnd),
    };
    let rect = item.rc;
    canvas.fill(&rect, frame);
    let pad = canvas.px(1).max(1);
    canvas.fill(
        &RECT {
            left: rect.left + pad,
            right: rect.right - pad,
            top: rect.top + pad,
            bottom: rect.bottom - pad,
        },
        bg,
    );
    let text = String::from_utf16_lossy(&wide[..len.max(0) as usize]);
    let left = rect.left + ((rect.right - rect.left - canvas.measure(&text)) / 2).max(pad);
    canvas.text(
        &RECT {
            left,
            right: rect.right - pad,
            ..rect
        },
        &text,
        color,
        false,
    );
    true
}

impl Drop for DialogColors<'_> {
    fn drop(&mut self) {
        // SAFETY: Brushes were created here and the modal window has closed.
        unsafe {
            if !self.background.0.is_null() {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.background.into());
            }
            if !self.list.0.is_null() {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.list.into());
            }
        }
    }
}

struct ThemedChoice<'a> {
    colors: DialogColors<'a>,
    default: i32,
}

fn themed_choice(
    owner: HWND,
    theme: &Theme,
    caption: &str,
    message: &str,
    choices: &[(i32, &str)],
    default: i32,
) -> Option<i32> {
    use windows::Win32::UI::WindowsAndMessaging::{
        DLGTEMPLATE, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    };
    // 既存の入力画面と同じ余白・ボタン高。長い操作名には文字数に応じた幅を取る。
    let widths: Vec<i16> = choices
        .iter()
        .map(|(_, label)| {
            let units: i16 = label
                .chars()
                .map(|ch| if ch.is_ascii() { 4 } else { 7 })
                .sum();
            (units + 14).max(50)
        })
        .collect();
    let buttons_width = widths.iter().sum::<i16>() + 6 * widths.len().saturating_sub(1) as i16;
    let width = 286.max(buttons_width + 20);
    let text_width = width - 20;
    let rows: usize = message
        .lines()
        .map(|line| {
            let units: usize = line
                .chars()
                .map(|ch| if ch.is_ascii() { 4 } else { 7 })
                .sum();
            units.div_ceil(text_width as usize).max(1)
        })
        .sum();
    let body_height = 6 + 11 * rows.clamp(2, 12) as i16;
    let first_button = body_height + 20;
    let mut template = Vec::new();
    push_u32(
        &mut template,
        (WS_POPUP | WS_CAPTION | WS_SYSMENU).0 | 0x08c0,
    );
    push_u32(&mut template, 0);
    push_u16(&mut template, 1 + choices.len() as u16);
    for x in [0i16, 0, width, first_button + 25] {
        push_i16(&mut template, x);
    }
    push_u16(&mut template, 0);
    push_u16(&mut template, 0);
    push_wstr(&mut template, caption);
    push_u16(&mut template, 9);
    push_wstr(&mut template, "Yu Gothic UI");
    let child = (WS_CHILD | WS_VISIBLE).0;
    push_item(
        &mut template,
        child | 0x0080,
        (10, 8, text_width, body_height),
        0xffff,
        0x0082,
        message,
    );
    let mut left = width - 10 - buttons_width;
    for (&(id, label), &button_width) in choices.iter().zip(&widths) {
        push_item(
            &mut template,
            child | WS_TABSTOP.0 | u32::from(id == default),
            (left, first_button, button_width, 15),
            id as u16,
            0x0080,
            label,
        );
        left += button_width + 6;
    }
    let mut aligned = vec![0u32; template.len().div_ceil(4)];
    let state = ThemedChoice {
        colors: DialogColors::new(theme),
        default,
    };
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok()?;
    // SAFETY: The buffer and colors stay alive until the modal dialog returns.
    let pressed = unsafe {
        std::ptr::copy_nonoverlapping(
            template.as_ptr(),
            aligned.as_mut_ptr().cast::<u8>(),
            template.len(),
        );
        DialogBoxIndirectParamW(
            Some(instance.into()),
            aligned.as_ptr().cast::<DLGTEMPLATE>(),
            Some(owner),
            Some(themed_choice_proc),
            LPARAM((&state as *const ThemedChoice<'_>) as isize),
        )
    } as i32;
    (pressed != -1).then_some(pressed)
}

unsafe extern "system" fn themed_choice_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWLP_USERDATA,
    };
    // SAFETY: The pointer is owned by themed_choice for the entire dialog lifetime.
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            let state = &*(lparam.0 as *const ThemedChoice<'_>);
            state.colors.initialize(hwnd);
            if let Ok(button) = GetDlgItem(Some(hwnd), state.default) {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(button));
                return 0;
            }
            return 1;
        }
        if msg == WM_COMMAND && (wparam.0 >> 16) == 0 {
            let _ = EndDialog(hwnd, (wparam.0 & 0xffff) as isize);
            return 1;
        }
        (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ThemedChoice<'_>)
            .as_ref()
            .and_then(|state| state.colors.message(msg, wparam, lparam, &[]))
            .unwrap_or(0)
    }
}

struct RecoveryPicker<'a> {
    labels: &'a [String],
    selected: Vec<usize>,
    colors: DialogColors<'a>,
}

/// 複数候補は通常の Windows リストで選択する。長い一覧はスクロールできる。
fn recovery_picker(
    owner: HWND,
    labels: &[String],
    theme: &Theme,
) -> Option<(RestoreChoice, Vec<usize>)> {
    use windows::Win32::UI::WindowsAndMessaging::{
        DLGTEMPLATE, LBS_HASSTRINGS, LBS_MULTIPLESEL, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
        LBS_OWNERDRAWFIXED, WS_BORDER, WS_CAPTION, WS_CHILD, WS_HSCROLL, WS_POPUP, WS_SYSMENU,
        WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
    };
    let mut template = Vec::new();
    // モーダル枠、既存の入力ダイアログと同じフォントとダイアログ単位。
    push_u32(
        &mut template,
        (WS_POPUP | WS_CAPTION | WS_SYSMENU).0 | 0x08c0,
    );
    push_u32(&mut template, 0);
    push_u16(&mut template, 6);
    for x in [0i16, 0, 316, 157] {
        push_i16(&mut template, x);
    }
    push_u16(&mut template, 0);
    push_u16(&mut template, 0);
    push_wstr(&mut template, "Linote - 復元できる文書");
    push_u16(&mut template, 9);
    push_wstr(&mut template, "Yu Gothic UI");
    let child = (WS_CHILD | WS_VISIBLE).0;
    let tab = child | WS_TABSTOP.0;
    push_item(
        &mut template,
        child,
        (10, 8, 296, 12),
        0xffff,
        0x0082,
        "復元する文書を選んでください（複数選択可）",
    );
    push_item(
        &mut template,
        tab | (WS_BORDER | WS_VSCROLL | WS_HSCROLL).0
            | (LBS_NOTIFY
                | LBS_MULTIPLESEL
                | LBS_NOINTEGRALHEIGHT
                | LBS_HASSTRINGS
                | LBS_OWNERDRAWFIXED) as u32,
        (10, 22, 296, 105),
        ID_RECOVERY_LIST as u16,
        0x0083,
        "",
    );
    for (id, label, x, width) in [
        (ID_RECOVERY_DISCARD, "選んだデータを破棄", 10, 80),
        (ID_RECOVERY_ALL, "すべて開く", 96, 60),
        (IDOK.0, "選んだ文書を開く", 162, 88),
        (IDCANCEL.0, "閉じる", 256, 50),
    ] {
        push_item(
            &mut template,
            tab | u32::from(id == IDOK.0),
            (x, 132, width, 15),
            id as u16,
            0x0080,
            label,
        );
    }
    let mut aligned = vec![0u32; template.len().div_ceil(4)];
    let mut state = RecoveryPicker {
        labels,
        selected: Vec::new(),
        colors: DialogColors::new(theme),
    };
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok()?;
    // SAFETY: テンプレートは DWORD 境界。ひな形と state はモーダル呼び出しの間有効。
    let pressed = unsafe {
        std::ptr::copy_nonoverlapping(
            template.as_ptr(),
            aligned.as_mut_ptr().cast::<u8>(),
            template.len(),
        );
        DialogBoxIndirectParamW(
            Some(instance.into()),
            aligned.as_ptr().cast::<DLGTEMPLATE>(),
            Some(owner),
            Some(recovery_picker_proc),
            LPARAM((&mut state as *mut RecoveryPicker<'_>) as isize),
        )
    } as i32;
    match pressed {
        i if i == IDOK.0 => Some((RestoreChoice::Restore, state.selected)),
        ID_RECOVERY_ALL => Some((RestoreChoice::Restore, (0..labels.len()).collect())),
        ID_RECOVERY_DISCARD => {
            let text = format!(
                "選んだ {} 件の復元データを破棄しますか？\n破棄すると元に戻せません。",
                state.selected.len()
            );
            let confirmed = themed_choice(
                owner,
                theme,
                "Linote - 復元データの破棄",
                &text,
                &[(IDYES.0, "破棄する"), (IDCANCEL.0, "キャンセル")],
                IDCANCEL.0,
            ) == Some(IDYES.0);
            if confirmed {
                Some((RestoreChoice::Discard, state.selected))
            } else {
                Some((RestoreChoice::Later, Vec::new()))
            }
        }
        -1 => None,
        _ => Some((RestoreChoice::Later, Vec::new())),
    }
}

unsafe extern "system" fn recovery_picker_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWLP_USERDATA, LBN_SELCHANGE, LB_ADDSTRING,
        LB_GETSELCOUNT, LB_GETSELITEMS, LB_SETHORIZONTALEXTENT, LB_SETSEL,
    };
    // SAFETY: 自分のダイアログと、モーダル呼び出し中だけ保持する state を参照する。
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            let state = &*(lparam.0 as *const RecoveryPicker<'_>);
            state.colors.initialize(hwnd);
            let Ok(list) = GetDlgItem(Some(hwnd), ID_RECOVERY_LIST) else {
                let _ = EndDialog(hwnd, -1);
                return 1;
            };
            let mut units = 0;
            for label in state.labels {
                let wide = to_wide(label);
                units = units.max(wide.len());
                if SendMessageW(
                    list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide.as_ptr() as isize)),
                )
                .0 < 0
                {
                    let _ = EndDialog(hwnd, -1);
                    return 1;
                }
            }
            let extent = units.min(8192) * 14 * dpi_for_window(hwnd) as usize / 96;
            if state.colors.theme.dark {
                windows::Win32::UI::Controls::SetWindowTheme(
                    list,
                    windows::core::w!("DarkMode_Explorer"),
                    PCWSTR::null(),
                )
                .ok();
            }
            let height = crate::platform::system::scale(20, dpi_for_window(hwnd));
            SendMessageW(
                list,
                windows::Win32::UI::WindowsAndMessaging::LB_SETITEMHEIGHT,
                None,
                Some(LPARAM(height as isize)),
            );
            SendMessageW(list, LB_SETHORIZONTALEXTENT, Some(WPARAM(extent)), None);
            SendMessageW(list, LB_SETSEL, Some(WPARAM(1)), Some(LPARAM(0)));
            return 1;
        }
        let state = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RecoveryPicker<'_>).as_ref();
        if let Some(state) = state {
            if let Some(result) = state.colors.message(msg, wparam, lparam, state.labels) {
                return result;
            }
        }
        if msg != WM_COMMAND {
            return 0;
        }
        let id = (wparam.0 & 0xffff) as i32;
        if matches!(id, ID_RECOVERY_ALL) || id == IDCANCEL.0 {
            let _ = EndDialog(hwnd, id as isize);
            return 1;
        }
        let Ok(list) = GetDlgItem(Some(hwnd), ID_RECOVERY_LIST) else {
            return 0;
        };
        let count = SendMessageW(list, LB_GETSELCOUNT, None, None).0;
        if id == ID_RECOVERY_LIST && (wparam.0 >> 16) as u32 == LBN_SELCHANGE {
            for id in [IDOK.0, ID_RECOVERY_DISCARD] {
                if let Ok(button) = GetDlgItem(Some(hwnd), id) {
                    let _ = EnableWindow(button, count > 0);
                }
            }
            return 1;
        }
        if (id == IDOK.0 || id == ID_RECOVERY_DISCARD) && count > 0 {
            let mut indices = vec![0i32; count as usize];
            let read = SendMessageW(
                list,
                LB_GETSELITEMS,
                Some(WPARAM(indices.len())),
                Some(LPARAM(indices.as_mut_ptr() as isize)),
            )
            .0;
            if read > 0 {
                let state =
                    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RecoveryPicker<'_>);
                state.selected = indices[..read as usize]
                    .iter()
                    .map(|&i| i as usize)
                    .collect();
                let _ = EndDialog(hwnd, id as isize);
            }
            return 1;
        }
        0
    }
}

/// Native input dialogs already use font-relative dialog units. Expand the label
/// when its actual text wraps, and move following controls by the measured delta.
fn fit_input_label(dlg: HWND, text: &[u16]) {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        DrawTextW, GetDC, ReleaseDC, RestoreDC, SaveDC, ScreenToClient, SelectObject, DT_CALCRECT,
        DT_NOPREFIX, DT_WORDBREAK, HFONT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, MoveWindow, SetWindowPos, SWP_NOZORDER, WM_GETFONT,
    };
    unsafe {
        let Ok(label) = GetDlgItem(Some(dlg), ID_INPUT_LABEL) else {
            return;
        };
        let mut rect = RECT::default();
        if GetWindowRect(label, &mut rect).is_err() {
            return;
        }
        let width = rect.right - rect.left;
        let old_height = rect.bottom - rect.top;
        let dc = GetDC(Some(label));
        let saved = SaveDC(dc);
        let font = SendMessageW(label, WM_GETFONT, None, None);
        if font.0 != 0 {
            SelectObject(dc, HFONT(font.0 as *mut _).into());
        }
        let mut measured = RECT {
            right: width,
            ..Default::default()
        };
        let mut wide = text.strip_suffix(&[0]).unwrap_or(text).to_vec();
        DrawTextW(
            dc,
            &mut wide,
            &mut measured,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        let _ = RestoreDC(dc, saved);
        ReleaseDC(Some(label), dc);
        let delta = (measured.bottom - measured.top + 2 - old_height).max(0);
        if delta == 0 {
            return;
        }
        for id in [ID_INPUT_LABEL, ID_INPUT_EDIT, IDOK.0, IDCANCEL.0] {
            let Ok(ctl) = GetDlgItem(Some(dlg), id) else {
                continue;
            };
            let mut r = RECT::default();
            let _ = GetWindowRect(ctl, &mut r);
            let mut point = POINT {
                x: r.left,
                y: r.top,
            };
            let _ = ScreenToClient(dlg, &mut point);
            let is_label = id == ID_INPUT_LABEL;
            let _ = MoveWindow(
                ctl,
                point.x,
                point.y + if is_label { 0 } else { delta },
                r.right - r.left,
                r.bottom - r.top + if is_label { delta } else { 0 },
                true,
            );
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(dlg, &mut r);
        let _ = SetWindowPos(
            dlg,
            None,
            r.left,
            (r.top - delta / 2).max(0),
            r.right - r.left,
            r.bottom - r.top + delta,
            SWP_NOZORDER,
        );
    }
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
                fit_input_label(hwnd, &state.label);
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

    #[test]
    fn font_height_follows_the_owners_dpi() {
        assert_eq!(font_height(12, 96), -16);
        assert_eq!(font_height(12, 144), -24);
        assert_eq!(font_height(12, 192), -32);
        assert_eq!(font_height(11, 120), -18);
    }

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
        assert_eq!(p.close_choice(owner, "a", true), CloseChoice::Cancel);
        assert!(!p.confirm(owner, "c", "t"));
        assert_eq!(p.restore_choice(owner, "t"), RestoreChoice::Later);
        p.confirm.borrow_mut().push_back(true);
        assert!(p.confirm(owner, "c", "t"));
        assert_eq!(p.shown.borrow().len(), 4);
    }
}
