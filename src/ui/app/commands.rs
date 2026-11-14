//! 統合 I1 のコマンド（ファイル、編集、表示、メモ、クリップボード）と、メニュー・
//! ショートカット・グローバルホットキーからの呼び出し。
//!
//! 操作の中身はここのハンドラーにだけ書く。メニュー（[`crate::ui::menu`]）、キー入力、
//! `WM_HOTKEY` はどれも [`Invocation::user`] を作って登録表に渡すだけ。

use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{HMENU, MSG, WM_KEYDOWN, WM_SYSKEYDOWN};

use crate::appdata::ThemeMode;
use crate::command::{
    CommandDef, CommandId, Effect, Invocation, ParamDef, ParamType, Registry, TargetKind, Value,
};
use crate::core::{Error, Result};
use crate::document::{Capabilities, Capability, DocumentState};
use crate::platform::clipboard;
use crate::platform::hotkey::{
    bring_to_front, HotkeyStatus, CLIPBOARD_HOTKEY_ID, DEFAULT_CLIPBOARD_CHORD,
};
use crate::text::{Encoding, EolStats};
use crate::ui::dialogs::NoticeKind;
use crate::ui::editor::Language;
use crate::ui::menu::{chord_from_input, current_modifiers, KeyMap, MenuModel};
use crate::ui::statusbar::MessageKind;

use super::fileio::{After, SaveDest, SaveOpts};
use super::{is_our_editor, App};

const EDIT: Capabilities = Capabilities::of(&[Capability::Edit]);
const UNDO: Capabilities = Capabilities::of(&[Capability::Edit, Capability::Undo]);

pub const FILE_OPEN: CommandDef = CommandDef::new(
    CommandId::new("file.open"),
    "開く...",
    "ファイルを選んでタブで開く（path を指定すると、選ばずにそのファイルを開く）。",
)
.params(&[ParamDef::optional(
    "path",
    ParamType::String,
    "開くファイルのパス",
)])
.default_key("Ctrl+O");

pub const FILE_SAVE: CommandDef = CommandDef::new(
    CommandId::new("file.save"),
    "上書き保存",
    "文書を保存する。まだファイルになっていなければ名前を付けて保存する。メモはメモとして保存する。",
)
.target(TargetKind::Document)
.default_key("Ctrl+S");

pub const FILE_SAVE_AS: CommandDef = CommandDef::new(
    CommandId::new("file.save_as"),
    "名前を付けて保存...",
    "保存先を選んで保存する（path を指定すると、選ばずにそこへ保存する）。",
)
.target(TargetKind::Document)
.params(&[ParamDef::optional(
    "path",
    ParamType::String,
    "保存先のパス",
)])
.requires(Capabilities::of(&[Capability::SaveAs]))
.default_key("Ctrl+Shift+S");

/// 文字コードの指定の値。
const ENCODINGS: &[&str] = &["utf-8", "utf-8-bom", "utf-16le", "utf-16be", "cp932"];

pub const FILE_REOPEN: CommandDef = CommandDef::new(
    CommandId::new("file.reopen_with_encoding"),
    "文字コードを指定して開き直す",
    "ファイルを指定した文字コードで読み直す。未保存の変更は失われるので確認する。",
)
.target(TargetKind::Document)
.params(&[ParamDef::required(
    "encoding",
    ParamType::Enum(ENCODINGS),
    "文字コード（utf-8、utf-8-bom、utf-16le、utf-16be、cp932）",
)]);

pub const FILE_EXIT: CommandDef =
    CommandDef::new(CommandId::new("file.exit"), "終了", "Linote を終了する。");

pub const EDIT_UNDO: CommandDef = CommandDef::new(
    CommandId::new("edit.undo"),
    "元に戻す",
    "直前の編集を元に戻す。",
)
.target(TargetKind::Document)
.requires(UNDO)
.effect(Effect::Edit)
.default_key("Ctrl+Z");

pub const EDIT_REDO: CommandDef = CommandDef::new(
    CommandId::new("edit.redo"),
    "やり直し",
    "元に戻した編集をやり直す。",
)
.target(TargetKind::Document)
.requires(UNDO)
.effect(Effect::Edit)
.default_key("Ctrl+Y");

pub const EDIT_CUT: CommandDef = CommandDef::new(
    CommandId::new("edit.cut"),
    "切り取り",
    "選択範囲を切り取ってクリップボードへ。",
)
.target(TargetKind::Document)
.requires(EDIT)
.effect(Effect::Edit)
.default_key("Ctrl+X");

pub const EDIT_COPY: CommandDef = CommandDef::new(
    CommandId::new("edit.copy"),
    "コピー",
    "選択範囲をクリップボードへコピーする。",
)
.target(TargetKind::Document)
.default_key("Ctrl+C");

pub const EDIT_PASTE: CommandDef = CommandDef::new(
    CommandId::new("edit.paste"),
    "貼り付け",
    "クリップボードの内容を貼り付ける。",
)
.target(TargetKind::Document)
.requires(EDIT)
.effect(Effect::Edit)
.default_key("Ctrl+V");

pub const EDIT_SELECT_ALL: CommandDef = CommandDef::new(
    CommandId::new("edit.select_all"),
    "すべて選択",
    "本文をすべて選択する。",
)
.target(TargetKind::Document)
.default_key("Ctrl+A");

pub const EDIT_GOTO_LINE: CommandDef = CommandDef::new(
    CommandId::new("edit.goto_line"),
    "行へ移動...",
    "指定した行（1 始まり）へカーソルを移動する（line を省くと入力欄を出す）。",
)
.target(TargetKind::Document)
.params(&[ParamDef::optional(
    "line",
    ParamType::Integer,
    "行番号（1 始まり）",
)])
.result("{\"line\": 移動した行（1 始まり）}")
.default_key("Ctrl+G");

pub const VIEW_TOGGLE_THEME: CommandDef = CommandDef::new(
    CommandId::new("view.toggle_theme"),
    "テーマを切り替え（システム／ライト／ダーク）",
    "明暗の設定を、システム → ライト → ダーク の順に切り替えて保存する。",
)
.result("{\"theme\": \"system\" | \"light\" | \"dark\"}");

pub const MEMO_OPEN_PANEL: CommandDef = CommandDef::new(
    CommandId::new("memo.open_panel"),
    "メモの一覧...",
    "メモの小窓を開く（開いていれば閉じる）。一覧は初めて開くときに読む。",
)
.default_key("Ctrl+Shift+M");

pub const MEMO_RENAME: CommandDef = CommandDef::new(
    CommandId::new("memo.rename"),
    "メモの名前を変更...",
    "開いているメモの名前を変える（name を省くと入力欄を出す）。",
)
.target(TargetKind::Document)
.params(&[ParamDef::optional("name", ParamType::String, "新しい名前")])
.requires(Capabilities::of(&[Capability::AutoSave]));

pub const CLIPBOARD_NEW_TAB: CommandDef = CommandDef::new(
    CommandId::new("clipboard.new_tab"),
    "クリップボードから新しいタブ",
    "クリップボードのテキストを、未保存の新しいタブに開く。グローバルショートカットでも呼ばれる。",
)
.result("{\"document\": 文書 ID または null}")
.default_key(DEFAULT_CLIPBOARD_CHORD);

fn string_arg(call: &crate::command::Call<'_>, name: &str) -> Option<String> {
    call.opt_str(name).map(str::to_owned)
}

/// I1 のコマンドを登録する。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(FILE_OPEN, |app, call| {
        if let Some(path) = string_arg(call, "path") {
            app.open_path(Path::new(&path));
            return Ok(Value::Null);
        }
        let dir = app
            .active_id()
            .and_then(|id| app.state(id))
            .and_then(|s| s.path())
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let owner = app.hwnd;
        let prompts = app.prompts.clone();
        for path in prompts.open_files(owner, dir.as_deref()) {
            app.open_path(&path);
        }
        Ok(Value::Null)
    })?;
    registry.register(FILE_SAVE, |app, call| {
        app.save_then(call.document()?, After::Nothing);
        Ok(Value::Null)
    })?;
    registry.register(FILE_SAVE_AS, |app, call| {
        let id = call.document()?;
        match string_arg(call, "path") {
            Some(path) => {
                app.save_document(id, SaveDest::As(path.into()), SaveOpts::new(After::Nothing))?
            }
            None => app.save_as_flow(id, After::Nothing)?,
        }
        Ok(Value::Null)
    })?;
    registry.register(FILE_REOPEN, |app, call| {
        let (encoding, bom) = match call.str("encoding")? {
            "utf-8" => (Encoding::Utf8, false),
            "utf-8-bom" => (Encoding::Utf8, true),
            "utf-16le" => (Encoding::Utf16Le, false),
            "utf-16be" => (Encoding::Utf16Be, false),
            "cp932" => (Encoding::Cp932, false),
            other => return Err(Error::invalid_argument("encoding", other.to_owned())),
        };
        app.reopen_with_encoding(call.document()?, encoding, bom)?;
        Ok(Value::Null)
    })?;
    registry.register(FILE_EXIT, |app, _| {
        app.request_exit();
        Ok(Value::Null)
    })?;

    registry.register(EDIT_UNDO, |app, call| {
        app.with_text(call.document()?, |ed| ed.undo())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_REDO, |app, call| {
        app.with_text(call.document()?, |ed| ed.redo())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_CUT, |app, call| {
        app.with_text(call.document()?, |ed| ed.cut())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_COPY, |app, call| {
        app.with_text(call.document()?, |ed| ed.copy())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_PASTE, |app, call| {
        app.with_text(call.document()?, |ed| ed.paste())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_SELECT_ALL, |app, call| {
        app.with_text(call.document()?, |ed| ed.select_all())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_GOTO_LINE, |app, call| {
        let id = call.document()?;
        let (total, current) = app.with_text(id, |ed| {
            let caret = ed.selection().caret;
            (ed.line_count(), ed.line_from_position(caret) + 1)
        })?;
        let line = match call.opt_u64("line") {
            Some(n) => n,
            None => {
                let owner = app.hwnd;
                let prompts = app.prompts.clone();
                let label = format!("行番号（1 〜 {total}）:");
                let Some(input) =
                    prompts.input_text(owner, "行へ移動", &label, &current.to_string())
                else {
                    return Ok(Value::Null);
                };
                match crate::ui::bigview::logic::parse_line_number(&input) {
                    Some(n) => n,
                    None => {
                        app.set_status(
                            "行番号は 1 以上の数字で入力してください",
                            MessageKind::Warning,
                        );
                        return Ok(Value::Null);
                    }
                }
            }
        };
        if line == 0 {
            return Err(Error::invalid_argument("line", "1 以上の行番号です"));
        }
        let line = line.min(total);
        app.with_text(id, |ed| ed.goto_line(line - 1))?;
        Ok(serde_json::json!({ "line": line }))
    })?;

    registry.register(VIEW_TOGGLE_THEME, |app, _| {
        let next = match app.theme_mode() {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        };
        app.set_theme_mode(next);
        app.settings.theme = next;
        app.persist_settings();
        app.set_status(&format!("テーマ: {}", theme_label(next)), MessageKind::Info);
        Ok(serde_json::json!({ "theme": next.name() }))
    })?;

    registry.register(MEMO_OPEN_PANEL, |app, _| {
        app.toggle_memo_panel()?;
        Ok(Value::Null)
    })?;
    registry.register(MEMO_RENAME, |app, call| {
        let id = call.document()?;
        let name = match string_arg(call, "name") {
            Some(n) => n,
            None => {
                let title = app
                    .document(id)
                    .map(|d| d.title.clone())
                    .unwrap_or_default();
                let owner = app.hwnd;
                let prompts = app.prompts.clone();
                match prompts.input_text(owner, "メモの名前を変更", "新しい名前:", &title)
                {
                    Some(n) => n,
                    None => return Ok(Value::Null),
                }
            }
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::invalid_argument("name", "名前が空です"));
        }
        app.rename_memo(id, name)?;
        Ok(Value::Null)
    })?;

    registry.register(CLIPBOARD_NEW_TAB, |app, _| {
        let owner = app.hwnd;
        match clipboard::read_text(Some(owner)) {
            Ok(Some(text)) => {
                let mut state = DocumentState::clipboard();
                if let Some(eol) = EolStats::scan(text.as_bytes()).dominant() {
                    state.format.eol = eol;
                }
                app.clipboard_counter += 1;
                let title = format!("クリップボード {}", app.clipboard_counter);
                let id = app.open_text(state, &text, title, Language::PlainText, true)?;
                app.set_status(
                    "クリップボードの内容を新しいタブに開きました",
                    MessageKind::Info,
                );
                Ok(serde_json::json!({ "document": id.as_u64() }))
            }
            Ok(None) => {
                app.set_status("クリップボードにテキストがありません", MessageKind::Warning);
                Ok(serde_json::json!({ "document": null }))
            }
            Err(Error::Busy { .. }) => {
                app.set_status(
                    "クリップボードを開けません（ほかのアプリが使用中です）",
                    MessageKind::Warning,
                );
                Ok(serde_json::json!({ "document": null }))
            }
            Err(e) => Err(e),
        }
    })?;
    Ok(())
}

fn theme_label(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::System => "システムに合わせる",
        ThemeMode::Light => "ライト",
        ThemeMode::Dark => "ダーク",
    }
}

impl App {
    /// 差し替え用: 確認・ファイル選択の実装（自己診断で台本つきに替える）。
    pub fn set_prompts(&mut self, prompts: std::rc::Rc<dyn crate::ui::dialogs::Prompts>) {
        self.prompts = prompts;
    }

    pub fn settings(&self) -> &crate::appdata::Settings {
        &self.settings
    }

    pub fn keymap(&self) -> &KeyMap {
        &self.keymap
    }

    pub fn menu(&self) -> Option<&MenuModel> {
        self.menu.as_ref()
    }

    /// キー割り当てを決め、メニューバーを付ける。
    pub(super) fn install_menu(&mut self) {
        let (keymap, problems) = KeyMap::build(self.registry.definitions(), &self.settings);
        self.keymap = keymap;
        self.startup_notes = problems
            .into_iter()
            .map(|p| format!("キーの設定に問題があります: {p}"))
            .collect();
        match MenuModel::install(self.hwnd, &self.registry, &self.keymap) {
            Ok(menu) => self.menu = Some(menu),
            Err(e) => self
                .startup_notes
                .push(format!("メニューを作れません: {e}")),
        }
        // メニューバーの分だけ作業領域が変わる。
        self.relayout();
    }

    /// 起動時に分かった問題（設定ファイル、保存場所、キーの重複）を知らせる。
    pub(super) fn startup_notices(&mut self) {
        let mut notes = std::mem::take(&mut self.startup_notes);
        if let super::fileio::SettingsState::Broken(reason) = &self.settings_state {
            notes.insert(
                0,
                format!(
                    "設定ファイルを読めません（{reason}）。既定の設定で動作します（ファイルは上書きしません）"
                ),
            );
        }
        if self.paths.is_none() {
            notes.push(
                "データの保存場所を決められないため、メモ・復元データ・設定の保存は使えません"
                    .to_owned(),
            );
        }
        if !notes.is_empty() {
            self.set_status(&notes.join(" / "), MessageKind::Warning);
        }
    }

    /// グローバルホットキー（クリップボードから新しいタブ）を設定から登録する。
    pub(super) fn init_hotkey(&mut self) {
        let text = self
            .settings
            .keys
            .get(CLIPBOARD_NEW_TAB.id.as_str())
            .cloned()
            .unwrap_or_else(|| DEFAULT_CLIPBOARD_CHORD.to_owned());
        self.apply_hotkey(&text);
    }

    /// 割り当ての表記（空なら解除）で登録し直す。競合や誤りは状態表示に出す。
    pub fn apply_hotkey(&mut self, text: &str) {
        let hwnd: HWND = self.hwnd;
        let hotkey = self.hotkey.get_or_insert_with(|| {
            crate::platform::hotkey::GlobalHotkey::new(hwnd, CLIPBOARD_HOTKEY_ID)
        });
        let status = hotkey.set_from_text(text).clone();
        match status {
            HotkeyStatus::Registered(_) | HotkeyStatus::Unset => {}
            other => self.set_status(&other.message(), MessageKind::Warning),
        }
    }

    pub fn hotkey_status(&self) -> Option<&HotkeyStatus> {
        self.hotkey.as_ref().map(|h| h.status())
    }

    /// コマンドを利用者の操作として実行する（失敗は状態表示に出る）。
    pub fn run_command(&mut self, command: CommandId, args: Value) {
        let mut invocation = Invocation::user(command);
        if !args.is_null() {
            invocation = invocation.with_args(args);
        }
        let _ = self.execute(&invocation);
    }

    /// キー入力がコマンドの割り当てなら実行して `true`（メッセージループから呼ぶ）。
    /// 編集欄とメインウィンドウへの入力だけを扱い、小窓や入力欄のキーは触らない。
    pub(super) fn translate_shortcut(&mut self, msg: &MSG) -> bool {
        if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
            return false;
        }
        if msg.hwnd != self.hwnd && !is_our_editor(msg.hwnd) {
            return false;
        }
        let Some(chord) = chord_from_input(msg.wParam.0 as u32, current_modifiers()) else {
            return false;
        };
        let Some(command) = self.keymap.command_for(&chord) else {
            return false;
        };
        self.run_command(command, Value::Null);
        true
    }

    /// メニュー項目が選ばれた。
    pub(super) fn on_menu_command(&mut self, item: u16) -> bool {
        let Some((command, args)) = self.menu.as_ref().and_then(|m| m.command_for_id(item)) else {
            return false;
        };
        self.run_command(command, args);
        true
    }

    /// メニューが開く直前に、項目の有効・無効を直す。
    pub(super) fn on_init_menu_popup(&mut self, popup: HMENU) {
        let Some(menu) = self.menu.take() else { return };
        let registry = self.registry();
        menu.update_enabled(popup, |entry| {
            registry
                .check_available(self, &Invocation::user(entry.command))
                .is_ok()
        });
        self.menu = Some(menu);
    }

    /// `WM_HOTKEY`: ウィンドウを前面に出し、クリップボードから新しいタブを作る。
    pub(super) fn on_hotkey(&mut self, wparam: WPARAM) -> Option<LRESULT> {
        if wparam.0 as i32 != CLIPBOARD_HOTKEY_ID {
            return None;
        }
        if !bring_to_front(self.hwnd) {
            self.set_status(
                "ウィンドウを前面に出せませんでした（タスクバーから切り替えてください）",
                MessageKind::Warning,
            );
        }
        self.run_command(CLIPBOARD_NEW_TAB.id, Value::Null);
        Some(LRESULT(0))
    }

    /// 重大な失敗を、状態表示とは別に知らせる（見落とされると困るもの）。
    pub fn notify(&mut self, kind: NoticeKind, text: &str) {
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        prompts.notice(owner, kind, "Linote", text);
    }

    /// `WM_COMMAND`（メニュー由来のもの）か。
    pub(super) fn is_menu_command(wparam: WPARAM, lparam: LPARAM) -> bool {
        (wparam.0 >> 16) & 0xFFFF == 0 && lparam.0 == 0
    }
}
