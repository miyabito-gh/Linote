//! 前回のタブの記録と復元、ウィンドウの位置の記憶（V1）。
//!
//! - 正常終了のとき（確認を通って終了するとき）に、ファイルのある文書のパス・タブ順・
//!   アクティブなタブ・カーソル位置を `session.json` に書く（形式は
//!   [`crate::appdata::session`]）。起動時に引数も受け渡しもなければ、それを読んで開く。
//! - 開く処理は既存の非同期の読み込みを使う。終わる順番は不定なので、開き終えた文書を
//!   記録の順に並べ直し、カーソルを戻し、アクティブだったタブだけを前面にする
//!   （[`App::apply_session_restore`]）。
//! - ウィンドウの位置・大きさ・最大化は終了時に `settings.json` の `window` へ書き、
//!   起動時に（作業領域の内側に収めて）戻す。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SetWindowPlacement, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWNORMAL,
    WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
};

use crate::appdata::session::{Session, SessionDoc};
use crate::appdata::WindowSettings;
use crate::core::{DocumentId, TaskContext};
use crate::document::{DocumentKind, DocumentState, Selection};
use crate::file;
use crate::ui::editor::ViewState;

use super::{App, DocView};

/// 開き終えるのを待っている、前回のタブ 1 つ。
#[derive(Debug, Clone, Copy)]
pub(super) struct RestoreEntry {
    /// 記録の中での順番。
    order: usize,
    caret: u64,
    first_line: u64,
    /// 前回アクティブだったタブ。
    active: bool,
}

/// 前回のタブの復元の進み具合。
#[derive(Debug, Default)]
pub(super) struct RestoreState {
    pending: HashMap<PathBuf, RestoreEntry>,
    /// 並べ終えた文書の記録の順番。
    placed: HashMap<DocumentId, usize>,
}

impl RestoreState {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// 文書 `placed`（順番付き）の中で、順番 `order` の文書を置く位置。順番のない文書（引数で
/// 開いたものなど）は前に置く。
pub(super) fn target_index(placed_orders: &[Option<usize>], order: usize) -> usize {
    placed_orders
        .iter()
        .filter(|o| o.is_none_or(|o| o < order))
        .count()
}

impl App {
    // ---------------------------------------------------------------- 復元

    /// 前回の記録を読み、存在するファイルを開き始める。開き始めた数を返す。
    /// 設定で無効・記録がない・壊れているときは 0（何もしない）。
    pub(super) fn begin_session_restore(&mut self) -> usize {
        if !self.settings.restore_session {
            return 0;
        }
        let Some(paths) = &self.paths else { return 0 };
        let Ok(text) = std::fs::read_to_string(paths.session_file()) else {
            return 0;
        };
        let Some(session) = Session::from_json(&text) else {
            return 0;
        };
        let session = session.existing(Path::is_file);
        for (i, d) in session.docs.iter().enumerate() {
            self.restore.pending.insert(
                d.path.clone(),
                RestoreEntry {
                    order: i,
                    caret: d.caret,
                    first_line: d.first_line,
                    active: session.active == Some(i),
                },
            );
        }
        for d in &session.docs {
            self.open_path(&d.path);
        }
        session.docs.len()
    }

    /// 文書を加えるときにアクティブにするか。復元中のタブは、前回アクティブだったものだけ
    /// 前面にする（ほかは裏で開く）。
    pub(super) fn session_activation(&self, state: &DocumentState, requested: bool) -> bool {
        match state.path().and_then(|p| self.restore.pending.get(p)) {
            Some(entry) => entry.active,
            None => requested,
        }
    }

    /// 開き終えた文書が前回のタブなら、順序とカーソルを戻す。
    pub(super) fn apply_session_restore(&mut self, id: DocumentId) {
        if self.restore.is_empty() {
            return;
        }
        let Some(path) = self.state(id).and_then(|s| s.path()).map(Path::to_path_buf) else {
            return;
        };
        let Some(entry) = self.restore.pending.remove(&path) else {
            return;
        };
        let orders: Vec<Option<usize>> = self
            .docs
            .iter()
            .filter(|d| d.id() != id)
            .map(|d| self.restore.placed.get(&d.id()).copied())
            .collect();
        self.restore.placed.insert(id, entry.order);
        let to = target_index(&orders, entry.order);
        self.move_tab(id, to);
        // 巨大ファイルは外部の表示部品が持つので、位置は戻さない。
        let Ok(i) = self.index_of(id) else { return };
        if !matches!(self.docs[i].view, DocView::Scintilla(_)) {
            return;
        }
        let Ok(caret) = self.with_text(id, |ed| {
            let len = ed.length();
            let mut pos = entry.caret.min(len);
            while pos > 0 && !ed.is_char_boundary(pos) {
                pos -= 1;
            }
            pos
        }) else {
            return;
        };
        let state = ViewState {
            selection: Selection::caret_at(caret),
            first_visible_line: entry.first_line,
            ..ViewState::default()
        };
        self.docs[i].view_state = state;
        if self.active == Some(i) {
            self.editor.restore_view_state(&state);
            self.refresh_status();
        }
    }

    // ---------------------------------------------------------------- 記録

    /// 今のタブの一覧を記録の形にする（ファイルのある通常文書と巨大文書だけ）。
    pub(super) fn current_session(&self) -> Session {
        let mut docs = Vec::new();
        let mut active = None;
        for (i, d) in self.docs.iter().enumerate() {
            if !matches!(d.state.kind(), DocumentKind::File | DocumentKind::LargeFile) {
                continue;
            }
            let Some(path) = d.state.path() else { continue };
            let view = if self.active == Some(i) && matches!(d.view, DocView::Scintilla(_)) {
                self.editor.view_state()
            } else {
                d.view_state
            };
            if self.active == Some(i) {
                active = Some(docs.len());
            }
            docs.push(SessionDoc {
                path: path.to_path_buf(),
                caret: view.selection.caret,
                first_line: view.first_visible_line,
            });
        }
        // 復元の途中で終了するときは、まだ開き終えていない前回のタブを末尾に残す。
        let mut waiting: Vec<(&PathBuf, &RestoreEntry)> = self.restore.pending.iter().collect();
        waiting.sort_by_key(|(_, e)| e.order);
        for (path, e) in waiting {
            if !docs.iter().any(|d| &d.path == path) {
                docs.push(SessionDoc {
                    path: path.clone(),
                    caret: e.caret,
                    first_line: e.first_line,
                });
            }
        }
        Session { docs, active }
    }

    /// 終了のときに、タブの記録とウィンドウの位置を書く（同期。起動を軽くするため
    /// 終了側で行う）。失敗しても終了は妨げない。
    pub(super) fn save_session_and_window(&mut self) {
        let Some(paths) = self.paths.clone() else {
            return;
        };
        if self.settings.restore_session {
            let json = self.current_session().to_json();
            let _ = paths.ensure_dir(paths.root()).and_then(|()| {
                file::write_atomic(
                    &paths.session_file(),
                    json.as_bytes(),
                    &TaskContext::detached(),
                )
            });
        }
        if let Some(window) = self.window_settings() {
            if self.settings.window != Some(window) {
                self.settings.window = Some(window);
                self.persist_settings_now();
            }
        }
    }

    // ---------------------------------------------------------------- ウィンドウの位置

    /// 今のウィンドウの位置（通常状態の矩形）と最大化。最小化中は最小化する前の状態。
    pub(super) fn window_settings(&self) -> Option<WindowSettings> {
        let mut wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: 出力先は構造体 1 つ（length を設定済み）。
        unsafe { GetWindowPlacement(self.hwnd, &mut wp) }.ok()?;
        let r = wp.rcNormalPosition;
        let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
            || (wp.showCmd == SW_SHOWMINIMIZED.0 as u32
                && wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0);
        Some(WindowSettings {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
            maximized,
        })
    }

    /// 保存した位置にウィンドウを置いて表示する（作業領域の内側に収める）。置いたら `true`
    /// （表示までしている）。記録がなければ何もせず `false`。
    pub(super) fn apply_saved_window(&self) -> bool {
        let Some(saved) = self.settings.window else {
            return false;
        };
        let rect = RECT {
            left: saved.x,
            top: saved.y,
            right: saved.x + saved.width,
            bottom: saved.y + saved.height,
        };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: 出力先は構造体 1 つ。
        let ok = unsafe {
            let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
            GetMonitorInfoW(monitor, &mut info).as_bool()
        };
        if !ok {
            return false;
        }
        let w = info.rcWork;
        let fitted = saved.fitted((w.left, w.top, w.right, w.bottom));
        let wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: if fitted.maximized {
                SW_SHOWMAXIMIZED.0 as u32
            } else {
                SW_SHOWNORMAL.0 as u32
            },
            ptMinPosition: windows::Win32::Foundation::POINT { x: -1, y: -1 },
            ptMaxPosition: windows::Win32::Foundation::POINT { x: -1, y: -1 },
            rcNormalPosition: RECT {
                left: fitted.x,
                top: fitted.y,
                right: fitted.x + fitted.width,
                bottom: fitted.y + fitted.height,
            },
            ..Default::default()
        };
        // SAFETY: 自分のウィンドウに、組み立てた配置を渡す。
        unsafe { SetWindowPlacement(self.hwnd, &wp) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_tabs_are_placed_by_recorded_order() {
        // 既に [順番 0, 順番 2] が並んでいるところへ、順番 1 を入れる。
        assert_eq!(target_index(&[Some(0), Some(2)], 1), 1);
        assert_eq!(target_index(&[Some(0), Some(2)], 3), 2);
        assert_eq!(target_index(&[Some(1), Some(2)], 0), 0);
        // 順番のない文書（引数で開いたもの）は前に置く。
        assert_eq!(target_index(&[None, Some(2)], 1), 1);
        assert_eq!(target_index(&[], 5), 0);
    }
}
