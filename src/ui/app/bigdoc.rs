//! 巨大ファイルの文書（統合 I2 の J・L）。
//!
//! 通常編集（Scintilla に全文を読み込む）では扱えない大きさ・形のファイルを、読み取り専用の
//! [`BigView`] のタブとして開く。本文は読み込まず、表示範囲だけをディスクから読む。
//!
//! - 文書ごとに [`BigView`] のウィンドウを 1 つ持つ（[`DocView::External`]）。タブの切り替えでは
//!   表示の状態（[`BigViewState`]）を保存・復元する。
//! - 行索引は [`BigFile::spawn_index`] でワーカーに作らせる。進み具合と外部変更の検知のため、
//!   巨大ファイルのタブがある間だけタイマー（[`TIMER_BIG`]）で [`BigView::refresh`] を呼ぶ。
//! - ステータスバーには「読み取り専用」と索引の進みを出す。
//! - 検索は [`BigView::find`]（置換は使えない）。行への移動は [`BigView::goto_line`]。

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::bigfile::{BigFile, BigFileConfig};
use crate::core::{CancelToken, DocumentId, Error, Result, TaskContext, WorkerPool};
use crate::document::DocumentState;
use crate::text::{Encoding, Eol, TextFormat};
use crate::ui::bigview::{BigView, BigViewEvent, BigViewState, BigViewStatus, GotoResult};
use crate::ui::statusbar::MessageKind;

use super::{register_editor_hwnd, unregister_editor_hwnd, App, UiMessage};

/// 巨大ファイルのビューの子ウィンドウ ID。
const ID_BIGVIEW: isize = 102;
/// 索引の進みと外部変更を見るタイマー（B が 1〜99、I1 が 110。ここは 130）。
pub(super) const TIMER_BIG: usize = 130;
const BIG_TICK_MS: u32 = 250;

/// 巨大ファイルの文書 1 つ分の部品。
pub(super) struct BigDoc {
    pub view: BigView,
    pub file: Arc<BigFile>,
    /// 索引作成の中止（閉じたとき）。
    pub cancel: CancelToken,
    /// タブを離れるときに控えた表示の状態。
    pub saved: BigViewState,
    /// 索引が済むまで待っている行への移動（Grep の結果・Ctrl+G）。
    pub pending_goto: Option<u64>,
}

impl App {
    /// 巨大ファイルを読み取り専用のビューのタブで開く（本文は読み込まない）。
    pub fn open_big(&mut self, path: &Path) -> Result<DocumentId> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let file = Arc::new(BigFile::open(&path, BigFileConfig::default())?);
        // 巨大ファイルのビューは UTF-8（ASCII 互換）の本文だけを扱う。
        if !file.looks_like_utf8()? {
            return Err(Error::unsupported(
                "UTF-8 ではない巨大ファイルは開けません（通常編集で開ける大きさにしてから、文字コードを指定して開いてください）",
            ));
        }
        let bom = file.has_bom().unwrap_or(false);
        let view = BigView::create(self.hwnd, self.instance, ID_BIGVIEW, false)?;
        view.set_theme(&self.theme);
        view.set_dpi(self.dpi);
        let sender = self.sender();
        let state = DocumentState::open_file(
            path.clone(),
            true,
            TextFormat {
                encoding: Encoding::Utf8,
                bom,
                eol: Eol::Lf,
            },
            file.stamp(),
        );
        let id = state.id();
        {
            let sender = sender.clone();
            view.set_callback(Rc::new(move |event: &BigViewEvent| {
                let event = event.clone();
                sender.send(UiMessage::Run(Box::new(move |app| {
                    app.on_bigview_event(id, event)
                })));
            }));
        }
        view.set_source(Some(file.clone()));
        register_editor_hwnd(view.hwnd());
        let cancel = CancelToken::new();
        let mut ctx = TaskContext::detached();
        ctx.cancel = cancel.clone();
        file.spawn_index(WorkerPool::global(), ctx, move |result| {
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.on_big_index_done(id, result)
            })));
        });
        let title = super::file_title(&path);
        let hwnd = view.hwnd();
        self.bigviews.insert(
            id,
            BigDoc {
                view,
                file,
                cancel,
                saved: BigViewState::default(),
                pending_goto: None,
            },
        );
        if let Err(e) = self.add_external(state, hwnd, title, true) {
            self.close_big(id);
            return Err(e);
        }
        if !self.big_timer {
            self.big_timer = true;
            // SAFETY: 自分のウィンドウのタイマー。
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_BIG, BIG_TICK_MS, None);
            }
        }
        self.set_status(
            "巨大ファイルのため、読み取り専用で開きました（行索引を作成中です）",
            MessageKind::Info,
        );
        self.apply_pending_jump(id);
        Ok(id)
    }

    /// 巨大ファイルのビューを手放す（文書を閉じるとき）。
    pub(super) fn close_big(&mut self, id: DocumentId) {
        let Some(big) = self.bigviews.remove(&id) else {
            return;
        };
        big.cancel.cancel();
        big.view.cancel_find();
        unregister_editor_hwnd(big.view.hwnd());
        big.view.destroy();
        if self.bigviews.is_empty() && self.big_timer {
            self.big_timer = false;
            // SAFETY: 自分のタイマーを止める。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_BIG);
            }
        }
    }

    /// タブを離れる・戻るときの表示の状態の保存と復元。
    pub(super) fn big_leave(&mut self, id: DocumentId) {
        if let Some(b) = self.bigviews.get_mut(&id) {
            b.saved = b.view.state();
        }
    }

    pub(super) fn big_enter(&self, id: DocumentId) {
        if let Some(b) = self.bigviews.get(&id) {
            b.view.restore_state(b.saved);
        }
    }

    /// タイマー: 索引の進みと外部変更を反映する。
    pub(super) fn big_tick(&mut self) {
        let Some(id) = self.active_id() else { return };
        if !self.bigviews.contains_key(&id) {
            return;
        }
        if let Some(b) = self.bigviews.get(&id) {
            b.view.refresh();
        }
        self.retry_pending_goto(id);
        self.refresh_status();
    }

    fn retry_pending_goto(&mut self, id: DocumentId) {
        let Some(b) = self.bigviews.get_mut(&id) else {
            return;
        };
        let Some(line) = b.pending_goto else { return };
        match b.view.goto_line(line) {
            GotoResult::IndexPending => {}
            GotoResult::Done => b.pending_goto = None,
            GotoResult::OutOfRange { total } => {
                b.pending_goto = None;
                b.view.goto_line(total);
            }
        }
    }

    pub(super) fn on_big_index_done(&mut self, id: DocumentId, result: Result<()>) {
        match result {
            Ok(()) => {
                self.retry_pending_goto(id);
                if self.active_id() == Some(id) {
                    self.refresh_status();
                }
            }
            Err(Error::Cancelled) => {}
            Err(Error::ExternalChange { .. }) => self.on_big_invalidated(id),
            Err(e) => self.set_status(
                &format!("行索引を作れませんでした: {e}"),
                MessageKind::Warning,
            ),
        }
    }

    /// ファイルがほかで変更された（置換・切り詰め・追記）。以後の表示は信用できない。
    fn on_big_invalidated(&mut self, id: DocumentId) {
        let title = self
            .document(id)
            .map(|d| d.title.clone())
            .unwrap_or_default();
        let _ = self.update_state(id, |s| s.record_external_change());
        self.set_status(
            &format!(
                "「{title}」がほかのプログラムで変更されました。このタブを閉じて、開き直してください"
            ),
            MessageKind::Warning,
        );
    }

    pub(super) fn on_bigview_event(&mut self, id: DocumentId, event: BigViewEvent) {
        if !self.bigviews.contains_key(&id) {
            return;
        }
        let active = self.active_id() == Some(id);
        match event {
            BigViewEvent::Status(_) => {
                if active {
                    self.refresh_status();
                }
            }
            BigViewEvent::GotoLineRequested => {
                if active {
                    self.run_command(super::commands::EDIT_GOTO_LINE.id, serde_json::Value::Null);
                }
            }
            BigViewEvent::Message(m) => self.set_status(&m, MessageKind::Info),
            BigViewEvent::Error(m) => {
                self.set_status(&m, MessageKind::Warning);
                if self
                    .bigviews
                    .get(&id)
                    .is_some_and(|b| b.file.is_invalidated())
                {
                    self.on_big_invalidated(id);
                }
            }
            BigViewEvent::Found { found, wrapped } => {
                if active {
                    let text = match (found, wrapped) {
                        (false, _) => "見つかりません",
                        (true, true) => "見つかりました（端で折り返しました）",
                        (true, false) => "見つかりました",
                    };
                    self.find_message(text, !found);
                }
            }
            BigViewEvent::SearchState { running } => {
                if running && active {
                    self.find_message("検索中...", false);
                }
            }
            BigViewEvent::Focused => {}
        }
    }

    /// 行へ移動（Ctrl+G）。`line` が `None` なら入力欄で尋ねる。
    pub(super) fn big_goto(
        &mut self,
        id: DocumentId,
        line: Option<u64>,
    ) -> Result<serde_json::Value> {
        let status = self.bigviews.get(&id).and_then(|b| b.view.status());
        let line = match line {
            Some(n) => n,
            None => {
                let label = match status.map(|s| s.index) {
                    Some(ix) if ix.complete => format!("行番号（1 〜 {}）:", ix.lines),
                    _ => "行番号（索引の作成中は、作成が済んだ範囲まで）:".to_owned(),
                };
                let current = status.and_then(|s| s.line).unwrap_or(1).to_string();
                let owner = self.hwnd;
                let prompts = self.prompts.clone();
                let Some(input) = prompts.input_text(owner, "行へ移動", &label, &current)
                else {
                    return Ok(serde_json::Value::Null);
                };
                match crate::ui::bigview::logic::parse_line_number(&input) {
                    Some(n) => n,
                    None => {
                        self.set_status(
                            "行番号は 1 以上の数字で入力してください",
                            MessageKind::Warning,
                        );
                        return Ok(serde_json::Value::Null);
                    }
                }
            }
        };
        if line == 0 {
            return Err(Error::invalid_argument("line", "1 以上の行番号です"));
        }
        let Some(b) = self.bigviews.get_mut(&id) else {
            return Err(Error::not_found(format!("文書 {id}")));
        };
        match b.view.goto_line(line) {
            GotoResult::Done => {
                b.pending_goto = None;
                Ok(serde_json::json!({ "line": line }))
            }
            GotoResult::OutOfRange { total } => {
                b.pending_goto = None;
                b.view.goto_line(total);
                self.set_status(
                    &format!("行は 1 〜 {total} です。最終行へ移動しました"),
                    MessageKind::Info,
                );
                Ok(serde_json::json!({ "line": total }))
            }
            GotoResult::IndexPending => {
                b.pending_goto = Some(line);
                self.set_status(
                    "行索引を作成中です。作成が済み次第、その行へ移動します",
                    MessageKind::Info,
                );
                Ok(serde_json::json!({ "line": null, "pending": true }))
            }
        }
    }

    /// ステータスバー向けの位置と索引の進み（`(位置の文字列, 索引の文字列)`）。
    pub(super) fn big_status_text(&self, id: DocumentId) -> Option<(String, String)> {
        let b = self.bigviews.get(&id)?;
        let st: BigViewStatus = b.view.status()?;
        let mut position = match st.line {
            Some(line) if st.index.complete => format!("行 {line} / {}", st.index.lines),
            Some(line) => format!("行 {line}"),
            None => format!("位置 {} バイト", st.offset),
        };
        if st.selection_bytes > 0 {
            position.push_str(&format!(" ({} バイト選択)", st.selection_bytes));
        }
        let index = if st.index.complete {
            "巨大ファイル（索引済み）".to_owned()
        } else {
            let pct = (st.index.indexed_bytes.saturating_mul(100))
                .checked_div(st.file_len)
                .unwrap_or(100)
                .min(100);
            format!("巨大ファイル 索引作成中 {pct}%")
        };
        Some((position, index))
    }
}
