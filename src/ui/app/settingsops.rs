//! 設定の再読み込みと設定画面（V4-1）。
//!
//! - **再読み込みは 1 か所**（[`App::reload_settings`]）。`settings.json` を読み直し、今の設定との
//!   差（[`crate::appdata::Settings::diff`]）の分だけ、実行中のウィンドウに反映する
//!   （テーマ、表示、キー割り当てとメニューの表示、グローバルショートカットの登録し直し、
//!   検索の履歴の欄）。印刷の設定・前回のタブの復元・最近使ったファイルは、使うときに設定を
//!   読むので置き換えるだけでよい。
//! - 再読み込みの引き金は [`ReloadReason`]: コマンド（手動）、ウィンドウが前面に戻ったとき
//!   （更新時刻と長さが前回読んだときと違うとき）、Linote のタブで設定ファイルを保存したとき、
//!   設定画面で保存する直前（ほかのウィンドウの変更を失わないため）。
//! - **自分の書き込みでは再読み込みしない**: 書き出し（ワーカー）の終わりに、書いた直後の
//!   ファイルの印を受け取って覚える。書き出しが残っている間は自動検出を止める。そのうえで
//!   読み直した内容が今の設定と同じなら何も反映しない。
//! - 設定ファイルが読めない間は、今の設定を保ち、状態欄で知らせる。書き込みは止まり
//!   （[`super::fileio::SettingsState::Broken`]）、直って読めたら再開する。

use std::path::Path;

use crate::appdata::settings::{apply_edits, stamp_changed, SettingsChanges, SettingsStamp};
use crate::appdata::Settings;
use crate::command::{CommandDef, CommandId, Registry, Value};
use crate::core::{Error, Result, TaskContext};
use crate::file;
use crate::ui::dialogs::SettingsSession;
use crate::ui::statusbar::MessageKind;

use super::commands::clipboard_hotkey_text;
use super::fileio::SettingsState;
use super::App;

pub const APP_SETTINGS: CommandDef = CommandDef::new(
    CommandId::new("app.settings"),
    "設定...",
    "設定画面（一般・表示・キー）を開く。OK・適用で設定ファイルに保存し、実行中のウィンドウに反映する。",
)
.default_key("Ctrl+Comma");

pub const APP_RELOAD_SETTINGS: CommandDef = CommandDef::new(
    CommandId::new("app.reload_settings"),
    "設定を再読み込み",
    "settings.json を読み直して、実行中のウィンドウにすべて反映する。読めないときは今の設定を保つ。",
)
.result("{\"changed\": 変わった項目の名前の配列, \"broken\": 設定ファイルが読めないか}");

pub const APP_OPEN_SETTINGS_FILE: CommandDef = CommandDef::new(
    CommandId::new("app.open_settings_file"),
    "設定ファイルを開く",
    "settings.json を Linote のタブで開く（なければ今の設定で作ってから開く）。保存すると自動で再読み込みする。",
);

pub(super) fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(APP_SETTINGS, |app, _| {
        app.open_settings_dialog();
        Ok(Value::Null)
    })?;
    registry.register(APP_RELOAD_SETTINGS, |app, _| {
        let outcome = app.reload_settings(ReloadReason::Manual);
        Ok(serde_json::json!({
            "changed": outcome.changes.labels(),
            "broken": outcome.broken,
        }))
    })?;
    registry.register(APP_OPEN_SETTINGS_FILE, |app, _| {
        app.open_settings_file()?;
        Ok(Value::Null)
    })?;
    Ok(())
}

/// 再読み込みの引き金。状態欄に出す知らせの量が違う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReloadReason {
    /// コマンド（常に結果を知らせる）。
    Manual,
    /// ウィンドウが前面に戻って、ファイルが変わっていた（変更があったときだけ知らせる）。
    Activated,
    /// Linote のタブで設定ファイルを保存した（結果を知らせる）。
    SavedInTab,
    /// 設定画面で保存する直前（最新の内容を取り込むだけ。知らせない）。
    BeforeApply,
}

/// 再読み込みの結果。
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ReloadOutcome {
    pub changes: SettingsChanges,
    /// 設定ファイルが読めなかった（今の設定を保った）。
    pub broken: bool,
}

/// 同じファイルか（Windows のパスは大文字小文字を区別しない）。
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canon(a), canon(b));
    a == b
        || a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
}

impl App {
    /// 設定ファイルのパスか。
    pub(super) fn is_settings_file(&self, path: &Path) -> bool {
        self.paths
            .as_ref()
            .is_some_and(|p| same_path(&p.settings_file(), path))
    }

    /// 設定の書き出し（ワーカー）が終わった。書いた直後のファイルの印を、自分の書き込みとして
    /// 覚える（`None` は失敗か、後の書き出しに譲って書かなかった場合で、印は変えない）。
    pub(super) fn settings_written(&mut self, stamp: Option<SettingsStamp>) {
        self.settings_writes_pending = self.settings_writes_pending.saturating_sub(1);
        if let Some(stamp) = stamp {
            self.settings_stamp = Some(stamp);
        }
    }

    /// ウィンドウが前面に戻ったとき、設定ファイルが前回読んだときから変わっていれば読み直す。
    /// 調べるのはファイルの更新時刻と長さだけ（軽い）。
    pub(super) fn check_settings_file(&mut self) {
        if self.settings_writes_pending > 0 {
            return;
        }
        let Some(paths) = &self.paths else { return };
        let now = SettingsStamp::of(&paths.settings_file());
        if stamp_changed(self.settings_stamp, now) {
            self.reload_settings(ReloadReason::Activated);
        }
    }

    /// `settings.json` を読み直して、実行中のウィンドウに反映する（再読み込みの唯一の経路）。
    pub(super) fn reload_settings(&mut self, reason: ReloadReason) -> ReloadOutcome {
        let mut outcome = ReloadOutcome::default();
        let Some(paths) = self.paths.clone() else {
            if reason == ReloadReason::Manual {
                self.set_status(
                    "データの保存場所を決められないため、設定を読み直せません",
                    MessageKind::Warning,
                );
            }
            return outcome;
        };
        // 書き出しが残っている間は、今の設定のほうが新しい（読み直すと失う）。
        if self.settings_writes_pending > 0
            && matches!(reason, ReloadReason::Activated | ReloadReason::BeforeApply)
        {
            return outcome;
        }
        let file = paths.settings_file();
        // 印は読む前に取る（読んでいる間の更新は、次の検出で拾う）。
        self.settings_stamp = SettingsStamp::of(&file);
        let (loaded, state) = super::fileio::load_settings_from(&file);
        let quiet = reason == ReloadReason::BeforeApply;
        let was_broken = !self.settings_state.writable();
        match state {
            SettingsState::Broken(why) => {
                self.settings_state = SettingsState::Broken(why.clone());
                outcome.broken = true;
                if !quiet {
                    self.set_status(
                        &format!(
                            "設定ファイルを読めません（{why}）。今の設定のまま動作します（ファイルは上書きしません）"
                        ),
                        MessageKind::Warning,
                    );
                }
                return outcome;
            }
            SettingsState::Missing => {
                // ファイルがない（消された）: 今の設定を保つ。設定を変えれば作り直す。
                self.settings_state = SettingsState::Missing;
                if matches!(reason, ReloadReason::Manual | ReloadReason::SavedInTab) {
                    self.set_status(
                        "設定ファイルがありません。今の設定のまま動作します（設定を変えると作ります）",
                        MessageKind::Info,
                    );
                }
                return outcome;
            }
            SettingsState::Loaded => {}
        }
        self.settings_state = SettingsState::Loaded;
        let old = std::mem::replace(&mut self.settings, loaded);
        outcome.changes = old.diff(&self.settings);
        let notes = self.apply_settings_changes(&old, &outcome.changes);
        if quiet {
            return outcome;
        }
        let labels = outcome.changes.labels();
        let silent = outcome.changes.is_empty() && !was_broken && notes.is_empty();
        if reason == ReloadReason::Activated && silent {
            return outcome;
        }
        let head = if reason == ReloadReason::Activated {
            "設定ファイルの変更を反映しました"
        } else {
            "設定を再読み込みしました"
        };
        let mut text = if outcome.changes.is_empty() {
            format!("{head}（変更なし）")
        } else if labels.is_empty() {
            head.to_owned()
        } else {
            format!("{head}（変更: {}）", labels.join("、"))
        };
        if was_broken {
            text.push_str("。設定ファイルを読めたので、保存を再開します");
        }
        let kind = if notes.is_empty() {
            MessageKind::Info
        } else {
            text.push_str(" / ");
            text.push_str(&notes.join(" / "));
            MessageKind::Warning
        };
        self.set_status(&text, kind);
        outcome
    }

    /// `old` から今の設定（`self.settings`）に変わった分を、実行中のウィンドウに反映する。
    /// 利用者に知らせるべき問題（キーの設定の誤りなど）を返す。
    pub(super) fn apply_settings_changes(
        &mut self,
        old: &Settings,
        changes: &SettingsChanges,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        if changes.theme {
            let mode = self.settings.theme;
            self.set_theme_mode(mode);
        }
        if changes.view {
            self.apply_view_settings();
        }
        if changes.keys {
            // アクセラレーター（キー入力の解釈）とメニューのキー表示を作り直す。
            notes = self.build_keys_and_menu();
            self.relayout();
            if clipboard_hotkey_text(old) != clipboard_hotkey_text(&self.settings) {
                let text = clipboard_hotkey_text(&self.settings);
                self.apply_hotkey(&text);
                if let Some(status) = self.hotkey_status() {
                    use crate::platform::hotkey::HotkeyStatus;
                    if !matches!(status, HotkeyStatus::Registered(_) | HotkeyStatus::Unset) {
                        notes.push(status.message());
                    }
                }
            }
        }
        if changes.grep {
            self.sync_grep_settings();
        }
        if changes.recent_files_max {
            self.settings.truncate_recent_files();
        }
        if changes.search_history {
            self.sync_find_history();
        }
        if changes.keys || changes.recent_files || changes.recent_files_max {
            // 文書 0 件の案内（キーの表示、最近使ったファイル）を描き直す。
            self.invalidate(self.layout.editor);
        }
        notes
    }

    /// 設定ファイルを Linote のタブで開く。なければ今の設定で作ってから開く。
    pub(super) fn open_settings_file(&mut self) -> Result<()> {
        let Some(paths) = self.paths.clone() else {
            return Err(Error::unavailable(
                "設定ファイル",
                "データの保存場所を決められません",
            ));
        };
        let path = paths.settings_file();
        if !path.exists() {
            paths.ensure_dir(paths.root())?;
            file::write_atomic(
                &path,
                self.settings.to_json().as_bytes(),
                &TaskContext::detached(),
            )?;
            self.settings_state = SettingsState::Loaded;
            self.settings_stamp = SettingsStamp::of(&path);
        }
        self.open_path(&path);
        Ok(())
    }

    // ---------------------------------------------------------------- 設定画面

    /// 設定画面を開く（モーダル）。
    pub(super) fn open_settings_dialog(&mut self) {
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        let mut session = AppSettingsSession { app: self };
        prompts.settings_dialog(owner, &mut session);
    }

    /// 設定画面の「適用」: 最新の設定ファイルを取り込んでから、画面で変えた項目だけを差し替え、
    /// 反映して保存する。設定ファイルが壊れている間は保存せず、この起動の間だけ反映する。
    /// 反映後の設定と、利用者に知らせるべき問題（キーの重複、ショートカットの登録の失敗）を返す。
    pub(super) fn apply_settings_edit(
        &mut self,
        baseline: &Settings,
        edited: &Settings,
    ) -> (Settings, Vec<String>) {
        self.reload_settings(ReloadReason::BeforeApply);
        let mut next = self.settings.clone();
        if apply_edits(baseline, edited, &mut next).is_empty() {
            return (self.settings.clone(), Vec::new());
        }
        let old = std::mem::replace(&mut self.settings, next);
        let changes = old.diff(&self.settings);
        let notes = self.apply_settings_changes(&old, &changes);
        let reason = self.settings_save_blocker();
        self.persist_settings();
        let (mut text, mut kind) = match reason {
            None => ("設定を保存して反映しました".to_owned(), MessageKind::Info),
            Some(why) => (
                format!("設定をこの起動の間だけ反映しました（{why}ため保存していません）"),
                MessageKind::Warning,
            ),
        };
        if !notes.is_empty() {
            text.push_str(" / ");
            text.push_str(&notes.join(" / "));
            kind = MessageKind::Warning;
        }
        self.set_status(&text, kind);
        (self.settings.clone(), notes)
    }

    /// 設定を保存できない理由（保存できるなら `None`）。
    fn settings_save_blocker(&self) -> Option<&'static str> {
        if self.paths.is_none() {
            Some("データの保存場所がない")
        } else if !self.settings_state.writable() {
            Some("設定ファイルが壊れている")
        } else {
            None
        }
    }

    /// 試験用: 実際の設定画面を開き、`driver` に操作させる（`driver` が OK などを押して閉じる）。
    pub(crate) fn run_real_settings_dialog_for_test(
        &mut self,
        driver: &mut dyn FnMut(windows::Win32::Foundation::HWND),
    ) {
        let owner = self.hwnd;
        let mut session = AppSettingsSession { app: self };
        crate::ui::settings_dialog::run(owner, &mut session, Some(driver));
    }
}

/// 設定画面から [`App`] を操作する窓口。
struct AppSettingsSession<'a> {
    app: &'a mut App,
}

impl SettingsSession for AppSettingsSession<'_> {
    fn current(&self) -> Settings {
        self.app.settings.clone()
    }

    fn warning(&self) -> Option<String> {
        if let SettingsState::Broken(why) = &self.app.settings_state {
            return Some(format!(
                "設定ファイルを読めません（{why}）。OK・適用は、この起動の間だけ反映します（保存しません）。ファイルを直して再読み込みすると、保存できるようになります。"
            ));
        }
        self.app.paths.is_none().then(|| {
            "データの保存場所を決められないため、OK・適用は、この起動の間だけ反映します（保存しません）。"
                .to_owned()
        })
    }

    fn apply(&mut self, baseline: &Settings, edited: &Settings) -> Settings {
        let (settings, notes) = self.app.apply_settings_edit(baseline, edited);
        if !notes.is_empty() {
            let owner = self.app.hwnd;
            self.notice(owner, &notes.join("\n"));
        }
        settings
    }

    fn command_defs(&self) -> Vec<CommandDef> {
        self.app.registry.definitions().copied().collect()
    }

    fn confirm(
        &mut self,
        owner: windows::Win32::Foundation::HWND,
        caption: &str,
        text: &str,
    ) -> bool {
        let prompts = self.app.prompts.clone();
        prompts.confirm(owner, caption, text)
    }

    fn notice(&mut self, owner: windows::Win32::Foundation::HWND, text: &str) {
        use crate::ui::dialogs::NoticeKind;
        let prompts = self.app.prompts.clone();
        prompts.notice(owner, NoticeKind::Warning, "設定", text);
    }
}
