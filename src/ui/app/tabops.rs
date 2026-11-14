//! タブの操作（U4）: 並べ替え、右側を閉じる、右クリックメニュー、
//! 別ウィンドウ（別プロセス）への移動と、その受け取り。
//!
//! 受け渡しのファイル形式と確認の手順は [`crate::appdata::handoff`]。ここでは次のことをする。
//!
//! - 送り側（[`App::move_to_new_window`]）: 受け渡しファイルを書き、`linote.exe --adopt` を
//!   起動して、[`TIMER_HANDOFF`] のタイマーで受け取りを見張る（UI を止めない）。ファイルが消えたら
//!   元のタブを閉じる。時間切れ・受け側の終了・起動の失敗では元のタブを残して知らせる。
//!   待っている間に元の文書を編集したときも、元のタブを残して知らせる（編集を失わない）。
//! - 受け側（[`App::adopt_handoff`]）: ファイルを読み、文書を開いてから受け取りを宣言する。
//!   未保存の内容は未保存のまま開き、復元データの対象にする。

use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetWindowRect, KillTimer, SetTimer,
    SetWindowPos, TrackPopupMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

use crate::appdata::handoff::{self, Handoff, HandoffDoc, TextDoc, TextKind};
use crate::command::Invocation;
use crate::core::{DocumentId, Error, Result, TargetRef, Version};
use crate::document::{DocumentKind, DocumentState};
use crate::file::{check_external_change, FileStamp};
use crate::platform::system;
use crate::ui::dialogs::NoticeKind;
use crate::ui::editor::Language;
use crate::ui::statusbar::MessageKind;
use crate::ui::tabs::{active_after_move, move_item, tab_menu_enabled, TAB_MENU};

use super::{file_title, App};

pub(super) const TIMER_HANDOFF: usize = 140;
const HANDOFF_TICK_MS: u32 = 100;

/// 受け取りを待っている移動 1 件。
pub(super) struct PendingHandoff {
    pub(super) id: DocumentId,
    /// 受け渡した時点の版（待つ間に編集されたら元のタブを閉じない）。
    version: Version,
    pub(super) file: PathBuf,
    title: String,
    pub(super) deadline: Instant,
    /// 起動した受け側（試験では `None`）。
    child: Option<std::process::Child>,
}

/// 受け側の結末。
enum Outcome {
    Waiting,
    Adopted,
    Failed(String),
}

impl PendingHandoff {
    /// 今の様子（受け渡しファイルと受け側のプロセスから）。
    fn poll(&mut self, now: Instant) -> Outcome {
        if !handoff::is_pending(&self.file) {
            return Outcome::Adopted;
        }
        let exited = self
            .child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
            .is_some();
        let timed_out = now >= self.deadline;
        if !exited && !timed_out {
            return Outcome::Waiting;
        }
        // 受け側と競合しても、取れた側だけが勝つ（`handoff` のモジュール文書）。
        if handoff::cancel(&self.file) {
            Outcome::Failed(if exited {
                "新しいウィンドウのプロセスが、文書を取り込む前に終了しました".to_owned()
            } else {
                "新しいウィンドウが時間内に文書を取り込みませんでした".to_owned()
            })
        } else {
            Outcome::Adopted
        }
    }
}

impl App {
    // ---------------------------------------------------------------- 並べ替え

    /// タブ `id` を `to` 番目へ動かす。動かしたら `true`。
    pub(super) fn move_tab(&mut self, id: DocumentId, to: usize) -> bool {
        let Ok(from) = self.index_of(id) else {
            return false;
        };
        if !move_item(&mut self.docs, from, to) {
            return false;
        }
        self.active = active_after_move(self.active, from, to);
        self.refresh_tabs();
        true
    }

    /// 指定した文書より右のタブを閉じる。閉じた数を返す。
    pub(super) fn close_documents_right_of(&mut self, id: DocumentId) -> usize {
        let Ok(i) = self.index_of(id) else { return 0 };
        let ids: Vec<DocumentId> = self.docs[i + 1..].iter().map(|d| d.id()).collect();
        let n = self.close_each(ids);
        let _ = self.activate(id);
        n
    }

    // ---------------------------------------------------------------- 右クリックメニュー

    /// タブ `index` の右クリックメニューを出し、選ばれたコマンドを実行する。
    pub(super) fn show_tab_menu(&mut self, index: usize) {
        let Some(doc) = self.docs.get(index) else {
            return;
        };
        let id = doc.id();
        let has_path = doc.state.path().is_some();
        let count = self.docs.len();
        let _ = self.activate(id);
        // SAFETY: 作ったポップアップメニューを、このウィンドウの上で出して破棄する。
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (n, item) in TAB_MENU.iter().enumerate() {
                match item {
                    None => {
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
                    }
                    Some((command, label)) => {
                        let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
                        let flags = if tab_menu_enabled(command, index, count, has_path) {
                            MF_STRING
                        } else {
                            MF_STRING | MF_GRAYED
                        };
                        let _ =
                            AppendMenuW(menu, flags, n + 1, windows::core::PCWSTR(wide.as_ptr()));
                    }
                }
            }
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(self.hwnd);
            let paint_state = crate::ui::menu::drawing::popup(menu);
            let r = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                p.x,
                p.y,
                None,
                self.hwnd,
                None,
            );
            crate::ui::menu::drawing::popup_closed(paint_state);
            let _ = DestroyMenu(menu);
            r.0 as usize
        };
        let Some(Some((command, _))) = chosen.checked_sub(1).and_then(|n| TAB_MENU.get(n)) else {
            return;
        };
        let inv = Invocation::user(crate::command::CommandId::new(command))
            .with_target(TargetRef::Document(id), None);
        let _ = self.execute(&inv);
    }

    // ---------------------------------------------------------------- 別ウィンドウへ移す（送り側）

    /// 受け渡す内容を作る。移せない文書は理由つきで断る。
    fn build_handoff_doc(&mut self, id: DocumentId) -> Result<HandoffDoc> {
        let i = self.index_of(id)?;
        if self.docs[i].saving {
            return Err(Error::Busy {
                what: "保存中の文書（保存が終わってからもう一度お試しください）".into(),
            });
        }
        let kind = self.docs[i].state.kind().clone();
        match kind {
            DocumentKind::Memo(_) => {
                // 受け側は保存領域から読むので、先に書き出す。
                if !self.save_memo_now(id) {
                    return Err(Error::Busy {
                        what: "メモの保存（保存できなかったので移せません）".into(),
                    });
                }
                let i = self.index_of(id)?;
                let tab = self.docs[i]
                    .memo
                    .as_ref()
                    .ok_or_else(|| Error::unsupported("メモの保存領域との対応がありません"))?;
                Ok(HandoffDoc::Memo {
                    id: tab.id().clone(),
                })
            }
            DocumentKind::LargeFile => {
                let path = self.path_of(id)?;
                Ok(HandoffDoc::File { path })
            }
            DocumentKind::File | DocumentKind::Untitled | DocumentKind::Clipboard => {
                let state = &self.docs[i].state;
                let dirty = state.is_dirty();
                if let (DocumentKind::File, false, Some(path)) = (&kind, dirty, state.path()) {
                    return Ok(HandoffDoc::File {
                        path: path.to_path_buf(),
                    });
                }
                if self.docs[i].lossy {
                    return Err(Error::unsupported(
                        "読み込みで不正なバイトを置き換えた文書は、確認の情報を渡せないので移せません",
                    ));
                }
                let doc_kind = match kind {
                    DocumentKind::File => TextKind::File,
                    DocumentKind::Clipboard => TextKind::Clipboard,
                    _ => TextKind::Untitled,
                };
                let state = &self.docs[i].state;
                let (path, format, disk) = (
                    state.path().map(Path::to_path_buf),
                    state.format,
                    state.disk_stamp(),
                );
                let title = self.docs[i].title.clone();
                let (_, text) = self.text_snapshot(id)?;
                Ok(HandoffDoc::Text(TextDoc {
                    kind: doc_kind,
                    path,
                    title,
                    format,
                    dirty,
                    disk,
                    text,
                }))
            }
        }
    }

    /// タブを新しいウィンドウ（新しいプロセス）へ移す。`position` は新しいウィンドウの
    /// 置き場所の目安（画面座標）。受け取りの確認ができたときに元のタブを閉じる
    /// （[`handoff_tick`](Self::handoff_tick)）。
    pub fn move_to_new_window(
        &mut self,
        id: DocumentId,
        position: Option<(i32, i32)>,
    ) -> Result<()> {
        self.sync();
        let i = self.index_of(id)?;
        if self.docs.len() < 2 {
            return Err(Error::unsupported(
                "タブが 1 つだけのときは新しいウィンドウへ移せません",
            ));
        }
        if self.handoffs.iter().any(|h| h.id == id) {
            return Err(Error::Busy {
                what: "このタブの移動（新しいウィンドウの応答待ち）".into(),
            });
        }
        let paths = self
            .paths
            .clone()
            .ok_or_else(|| Error::unavailable("受け渡し", "データの保存場所がありません"))?;
        let doc = self.build_handoff_doc(id)?;
        let i = self.index_of(id).unwrap_or(i);
        let version = self.docs[i].state.version();
        let title = self.docs[i].title.clone();
        handoff::sweep(&paths, std::time::SystemTime::now());
        self.handoff_seq += 1;
        let file = handoff::write(&paths, &Handoff::new(doc), self.handoff_seq)?;
        let child = if self.handoff_spawn {
            match spawn_adopter(&file, position) {
                Ok(child) => Some(child),
                Err(e) => {
                    let _ = handoff::cancel(&file);
                    return Err(e);
                }
            }
        } else {
            None
        };
        self.handoffs.push(PendingHandoff {
            id,
            version,
            file,
            title: title.clone(),
            deadline: Instant::now() + handoff::CONFIRM_TIMEOUT,
            child,
        });
        self.set_status(
            &format!("「{title}」を新しいウィンドウへ移しています..."),
            MessageKind::Info,
        );
        if !self.handoff_timer {
            self.handoff_timer = true;
            // SAFETY: 自分のウィンドウのタイマー。
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_HANDOFF, HANDOFF_TICK_MS, None);
            }
        }
        Ok(())
    }

    /// 受け取りの見張り（タイマー）。確認できたら元のタブを閉じ、失敗したら残して知らせる。
    pub(super) fn handoff_tick(&mut self) {
        let now = Instant::now();
        let mut pending = std::mem::take(&mut self.handoffs);
        let mut keep = Vec::new();
        let mut done: Vec<(PendingHandoff, Outcome)> = Vec::new();
        for mut h in pending.drain(..) {
            match h.poll(now) {
                Outcome::Waiting => keep.push(h),
                other => done.push((h, other)),
            }
        }
        self.handoffs = keep;
        for (h, outcome) in done {
            match outcome {
                Outcome::Adopted => self.finish_handoff(&h),
                Outcome::Failed(reason) => {
                    self.notify(
                        NoticeKind::Warning,
                        &format!(
                            "「{}」を新しいウィンドウへ移せませんでした。元のタブはそのまま残しています。\n{reason}",
                            h.title
                        ),
                    );
                }
                Outcome::Waiting => {}
            }
        }
        if self.handoffs.is_empty() && self.handoff_timer {
            self.handoff_timer = false;
            // SAFETY: 自分のタイマーを止める。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_HANDOFF);
            }
        }
    }

    fn finish_handoff(&mut self, h: &PendingHandoff) {
        let Ok(i) = self.index_of(h.id) else { return };
        if self.docs[i].state.version() != h.version {
            self.notify(
                NoticeKind::Warning,
                &format!(
                    "「{}」は新しいウィンドウへ移しましたが、移している間に元のタブを編集したため、\
                     元のタブは閉じずに残しました。必要な内容を確かめてから閉じてください。",
                    h.title
                ),
            );
            return;
        }
        // 受け側が引き継いだので、元の復元データも不要（`close_document` が破棄する）。
        match self.close_document_quietly(h.id) {
            Ok(_) => self.set_status(
                &format!("「{}」を新しいウィンドウへ移しました", h.title),
                MessageKind::Info,
            ),
            Err(e) => self.set_status(&e.to_string(), MessageKind::Warning),
        }
    }

    // ---------------------------------------------------------------- 受け取る（受け側）

    /// 起動引数 `--adopt <ファイル>` の処理: 受け渡しファイルから文書を開き、受け取りを
    /// 宣言する。失敗しても受け渡しファイルには触れない（送り側が時間切れで元のタブを残す）。
    pub fn adopt_handoff(&mut self, file: &Path) -> Result<()> {
        let h = handoff::read(file, handoff::now_ms())?;
        match h.doc {
            HandoffDoc::File { path } => {
                if !path.is_file() {
                    return Err(Error::not_found(format!("ファイル {}", path.display())));
                }
                self.claim_or_cancel(file)?;
                self.open_path(&path);
                Ok(())
            }
            HandoffDoc::Memo { id } => {
                let store = self
                    .memo_store()
                    .ok_or_else(|| Error::unavailable("メモ", "データの保存場所がありません"))?;
                let memo = store.load(&id)?;
                self.claim_or_cancel(file)?;
                self.open_memo(memo).map(|_| ())
            }
            HandoffDoc::Text(t) => self.adopt_text(file, t),
        }
    }

    fn claim_or_cancel(&self, file: &Path) -> Result<()> {
        if handoff::claim(file) {
            Ok(())
        } else {
            // 送り側が時間切れで取り消した（元のタブは残っている）。
            Err(Error::Cancelled)
        }
    }

    fn adopt_text(&mut self, file: &Path, t: TextDoc) -> Result<()> {
        let mut note = None;
        let mut state = match (&t.kind, &t.path) {
            (TextKind::File, Some(path)) => {
                let disk = t
                    .disk
                    .or_else(|| FileStamp::read(path).ok().flatten())
                    .unwrap_or(FileStamp {
                        len: 0,
                        modified: None,
                        id: None,
                    });
                let mut state = DocumentState::open_file(path.clone(), false, t.format, disk);
                if t.disk.is_some()
                    && check_external_change(path, t.disk).is_ok_and(|c| c.is_changed())
                {
                    state.record_external_change();
                    note = Some(format!(
                        "「{}」の元のファイルは、移している間に変更されています。\
                         上書きせず、「名前を付けて保存」で別に保存してください",
                        t.title
                    ));
                }
                state
            }
            (TextKind::Clipboard, _) => DocumentState::clipboard(),
            _ => DocumentState::untitled(),
        };
        state.format = t.format;
        if t.dirty {
            state.mark_unsaved();
        }
        let language = t
            .path
            .as_deref()
            .map(Language::from_path)
            .unwrap_or(Language::PlainText);
        let title = t.path.as_deref().map(file_title).unwrap_or(t.title);
        let id = self.open_text(state, &t.text, title, language, true)?;
        if self.claim_or_cancel(file).is_err() {
            let _ = self.close_document_quietly(id);
            return Err(Error::Cancelled);
        }
        if t.dirty {
            // 復元データの対象にして、すぐ書き出す。
            self.note_change(id);
            self.flush_pending_writes();
        }
        if let Some(note) = note {
            self.set_status(&note, MessageKind::Warning);
        }
        Ok(())
    }

    /// 新しいウィンドウの位置を、`pos`（画面座標）に合わせる（表示する前に呼ぶ）。
    pub(super) fn place_at(&self, pos: (i32, i32)) {
        let mut rect = RECT::default();
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: 出力先は構造体 1 つずつ。
        let ok = unsafe {
            let monitor = MonitorFromPoint(POINT { x: pos.0, y: pos.1 }, MONITOR_DEFAULTTONEAREST);
            GetWindowRect(self.hwnd, &mut rect).is_ok()
                && GetMonitorInfoW(monitor, &mut info).as_bool()
        };
        if !ok {
            return;
        }
        let w = info.rcWork;
        let (x, y) = handoff::place_window(
            pos,
            (rect.right - rect.left, rect.bottom - rect.top),
            (w.left, w.top, w.right, w.bottom),
            system::scale(20, self.dpi),
        );
        // SAFETY: 自分のウィンドウを動かす（大きさ・重なり順・アクティブ状態は変えない）。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// カーソルの画面座標と、このウィンドウ全体の矩形（画面座標）。
    pub(super) fn cursor_and_window_rect(&self) -> ((i32, i32), RECT) {
        let mut p = POINT::default();
        let mut r = RECT::default();
        // SAFETY: 出力先は構造体 1 つずつ。
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        ((p.x, p.y), r)
    }

    /// タブのドラッグで外へ離された: 新しいウィンドウへ移す（失敗は状態表示）。
    pub(super) fn detach_tab(&mut self, id: DocumentId, at: (i32, i32)) {
        let inv = Invocation::user(super::commands::FILE_MOVE_TO_NEW_WINDOW.id)
            .with_target(TargetRef::Document(id), None)
            .with_args(serde_json::json!({ "position": handoff::format_position(at) }));
        let _ = self.execute(&inv);
    }
}

/// `linote.exe --adopt <ファイル> [--pos x,y]` を起動する。
fn spawn_adopter(file: &Path, position: Option<(i32, i32)>) -> Result<std::process::Child> {
    let exe = std::env::current_exe().map_err(|e| Error::io("起動", Path::new("linote"), e))?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--adopt").arg(file);
    if let Some(p) = position {
        cmd.arg("--pos").arg(handoff::format_position(p));
    }
    cmd.spawn()
        .map_err(|e| Error::io("新しいウィンドウの起動", &exe, e))
}
