//! メモ（タスク E の画面側のつなぎ込み）。
//!
//! - コマンド `memo.open_panel`: 初めてのときに小窓を作り（メモは読まない）、以降は開閉する。
//! - 小窓から届く [`PanelEvent`] は [`UiMessage::Run`] で UI スレッドに戻して処理する
//!   （小窓のウィンドウプロシージャの中で重い処理をしない）。
//! - メモのタブは [`DocumentState::memo`]。保存領域の版は文書の版とは別なので
//!   [`MemoTab`] に控える。自動保存はワーカー（[`MemoStore::save_in_background`]）で、
//!   結果は文書の存在と版を確かめて反映する。衝突したら別のメモとして残り、タブはそれに
//!   切り替わる（文書の種類に持つ ID は開いたときのままなので、実際の ID は [`MemoTab`] を見る）。
//! - 閉じる・終了では、尋ねずに同期して保存する（失敗したら閉じない）。

use std::rc::Rc;

use crate::core::{DocumentId, Error, MemoId, Result, Version};
use crate::document::{DocumentKind, DocumentState};
use crate::memo::{Memo, MemoStore, MemoTab, SaveOutcome};
use crate::ui::editor::Language;
use crate::ui::memo_panel::{MemoPanel, PanelEvent};
use crate::ui::statusbar::MessageKind;

use super::{App, UiMessage};

impl App {
    pub(super) fn memo_store(&self) -> Option<MemoStore> {
        self.paths.as_ref().map(MemoStore::new)
    }

    /// 保存領域上のメモの現在の版（開いているタブがあればその控え）。
    pub(super) fn memo_version(&self, id: &MemoId) -> Result<Version> {
        if let Some(tab) = self
            .docs
            .iter()
            .filter_map(|d| d.memo.as_ref())
            .find(|t| t.id() == id)
        {
            return Ok(tab.store_version());
        }
        let store = self
            .memo_store()
            .ok_or_else(|| Error::unavailable("メモ", "データの保存場所がありません"))?;
        Ok(store.load(id)?.version)
    }

    /// メモの小窓を開閉する（初めてなら作る。一覧はこのとき初めて読む）。
    pub fn toggle_memo_panel(&mut self) -> Result<()> {
        if self.memo_panel.is_none() {
            let store = self
                .memo_store()
                .ok_or_else(|| Error::unavailable("メモ", "データの保存場所がありません"))?;
            let sender = self.sender();
            let on_event: Rc<dyn Fn(PanelEvent)> = Rc::new(move |event| {
                sender.send(UiMessage::Run(Box::new(move |app| {
                    app.on_panel_event(event)
                })));
            });
            let panel = MemoPanel::create(self.hwnd, store, &self.theme, self.dpi, on_event)?;
            self.memo_panel = Some(panel);
        }
        if let Some(panel) = self.memo_panel {
            panel.toggle();
        }
        Ok(())
    }

    /// 小窓が開いているか。
    pub fn memo_panel_visible(&self) -> bool {
        self.memo_panel.is_some_and(|p| p.is_visible())
    }

    fn on_panel_event(&mut self, event: PanelEvent) {
        match event {
            PanelEvent::Open(memo) => {
                if let Err(e) = self.open_memo(memo) {
                    self.set_status(
                        &format!("メモを開けませんでした: {e}"),
                        MessageKind::Warning,
                    );
                }
            }
            PanelEvent::Delete { id, name } => self.delete_memo_confirmed(&id, &name),
            PanelEvent::Closed => self.focus_active(),
            PanelEvent::Failed(e) => {
                self.set_status(&format!("メモ: {e}"), MessageKind::Warning);
            }
        }
    }

    /// メモの削除（V2）。確認してから、保存領域の版を付けて消す。版が合わなければ
    /// 通知して消さない。開いているタブがあれば閉じる。
    pub(super) fn delete_memo_confirmed(&mut self, id: &MemoId, name: &str) {
        let Some(store) = self.memo_store() else {
            return;
        };
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        let ok = prompts.confirm(
            owner,
            "Linote - メモの削除",
            &format!("メモ「{name}」を削除しますか？\n\n削除したメモは元に戻せません。"),
        );
        if !ok {
            return;
        }
        let result = self
            .memo_version(id)
            .and_then(|version| store.delete(id, version));
        match result {
            Ok(()) => {
                let open: Vec<DocumentId> = self
                    .docs
                    .iter()
                    .filter(|d| d.memo.as_ref().is_some_and(|t| t.id() == id))
                    .map(|d| d.id())
                    .collect();
                for doc in open {
                    let _ = self.close_document_quietly(doc);
                }
                self.set_status(&format!("メモ「{name}」を削除しました"), MessageKind::Info);
            }
            Err(e @ Error::VersionMismatch { .. }) => {
                self.notify(
                    crate::ui::dialogs::NoticeKind::Warning,
                    &format!(
                        "メモ「{name}」は別の場所で変更されているため、削除しませんでした。\
                         開き直して内容を確かめてください。({e})"
                    ),
                );
            }
            Err(e) => self.set_status(
                &format!("メモを削除できませんでした: {e}"),
                MessageKind::Warning,
            ),
        }
        if let Some(panel) = self.memo_panel {
            panel.refresh();
        }
    }

    /// メモをタブで開く（既に開いていればそのタブへ）。
    pub fn open_memo(&mut self, memo: Memo) -> Result<DocumentId> {
        if let Some(d) = self
            .docs
            .iter()
            .find(|d| d.memo.as_ref().is_some_and(|t| t.id() == &memo.id))
        {
            let id = d.id();
            self.activate(id)?;
            return Ok(id);
        }
        let state = DocumentState::memo(memo.id.clone());
        let id = self.open_text(
            state,
            &memo.body,
            memo.name.clone(),
            Language::PlainText,
            true,
        )?;
        let i = self.index_of(id)?;
        self.docs[i].memo = Some(MemoTab::opened(&memo));
        Ok(id)
    }

    /// 入力が落ち着いたメモを、ワーカーで保存する。
    pub(super) fn autosave_memo(&mut self, id: DocumentId) {
        let Some(store) = self.memo_store() else {
            return;
        };
        let Ok(i) = self.index_of(id) else { return };
        let Some(tab) = self.docs[i].memo.as_mut() else {
            return;
        };
        let Some((memo_id, expected)) = tab.begin_save() else {
            // 保存中。終わったときに再保存される。
            return;
        };
        let (stamp, text) = match self.text_snapshot(id) {
            Ok(r) => r,
            Err(_) => {
                if let Some(t) = self.docs[i].memo.as_mut() {
                    t.finish_failed();
                }
                return;
            }
        };
        self.docs[i].pending = None;
        self.docs[i].write_in_flight = true;
        let sender = self.sender();
        let version = stamp.version;
        store.save_in_background(memo_id, expected, text, move |result| {
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_memo_save(id, version, result)
            })));
        });
    }

    fn finish_memo_save(&mut self, id: DocumentId, version: Version, result: Result<SaveOutcome>) {
        let Ok(i) = self.index_of(id) else {
            // 閉じられた。閉じる前に保存しているので、ここへ来るのは稀。
            return;
        };
        self.docs[i].write_in_flight = false;
        match result {
            Ok(outcome) => {
                let finished = self.docs[i].memo.as_mut().map(|t| t.finish(&outcome));
                let _ = self.record_saved(id, version, None, None);
                if let Some(f) = finished {
                    if let Some(forked) = f.forked {
                        let _ = self.set_title(id, forked.name.clone());
                        self.set_status(
                            &format!(
                                "別のウィンドウで更新されていたため、「{}」として保存しました",
                                forked.name
                            ),
                            MessageKind::Warning,
                        );
                    }
                    if f.resave {
                        self.autosave_memo(id);
                    }
                }
            }
            Err(e) => {
                let retry = self.docs[i]
                    .memo
                    .as_mut()
                    .map(|t| t.finish_failed())
                    .unwrap_or(false);
                self.set_status(
                    &format!("メモを保存できませんでした: {e}"),
                    MessageKind::Warning,
                );
                if retry {
                    self.autosave_memo(id);
                }
            }
        }
    }

    /// メモを同期して保存する（閉じる・終了の前。尋ねない）。保存できた（または保存の
    /// 必要がない）なら `true`。
    pub(super) fn save_memo_now(&mut self, id: DocumentId) -> bool {
        let Ok(i) = self.index_of(id) else {
            return true;
        };
        if !matches!(self.docs[i].state.kind(), DocumentKind::Memo(_)) {
            return true;
        }
        if self.docs[i].write_in_flight {
            self.set_status(
                "メモを保存しています。少し待ってからもう一度お試しください",
                MessageKind::Info,
            );
            return false;
        }
        let Some(store) = self.memo_store() else {
            self.set_status(
                "メモを保存できません（データの保存場所がありません）",
                MessageKind::Warning,
            );
            return false;
        };
        let Some((memo_id, expected)) = self.docs[i].memo.as_mut().and_then(|t| t.begin_save())
        else {
            return false;
        };
        let (stamp, text) = match self.text_snapshot(id) {
            Ok(r) => r,
            Err(_) => {
                if let Some(t) = self.docs[i].memo.as_mut() {
                    t.finish_failed();
                }
                return false;
            }
        };
        match store.save_or_fork(&memo_id, expected, &text) {
            Ok(outcome) => {
                if let Some(t) = self.docs[i].memo.as_mut() {
                    t.finish(&outcome);
                }
                self.docs[i].pending = None;
                let _ = self.record_saved(id, stamp.version, None, None);
                true
            }
            Err(e) => {
                if let Some(t) = self.docs[i].memo.as_mut() {
                    t.finish_failed();
                }
                self.set_status(
                    &format!("メモを保存できませんでした: {e}"),
                    MessageKind::Warning,
                );
                false
            }
        }
    }

    /// 未保存のメモをすべて同期して保存する。すべて保存できたら `true`。
    pub(super) fn flush_memos_sync(&mut self) -> bool {
        let ids: Vec<DocumentId> = self
            .docs
            .iter()
            .filter(|d| d.memo.is_some() && d.state.is_dirty())
            .map(|d| d.id())
            .collect();
        let results: Vec<bool> = ids.into_iter().map(|id| self.save_memo_now(id)).collect();
        results.iter().all(|&ok| ok)
    }

    /// 文書がまだ名前を持たない（無題・クリップボード由来）か。名前を持たなければ、メモにする
    /// ときに本文の最初の一文を名前にする。
    fn is_unnamed(&self, i: usize) -> bool {
        matches!(
            self.docs[i].state.kind(),
            DocumentKind::Untitled | DocumentKind::Clipboard
        )
    }

    /// 文書を新しいメモとして残せるか（メモの保存場所があり、メモ以外の編集できる文書）。
    pub(super) fn can_save_as_memo(&self, id: DocumentId) -> bool {
        self.memo_store().is_some()
            && self.document(id).is_some_and(|d| {
                matches!(d.view, super::DocView::Scintilla(_))
                    && !matches!(d.state.kind(), DocumentKind::Memo(_))
            })
    }

    /// 設定（`save.untitled_to_memo`）に従って、この文書の保存先をメモにするか
    /// （まだ名前のない文書で、メモを残せるとき）。
    pub(super) fn wants_memo_save(&self, id: DocumentId) -> bool {
        self.settings.save.untitled_to_memo
            && self.can_save_as_memo(id)
            && self.index_of(id).is_ok_and(|i| self.is_unnamed(i))
    }

    /// 閉じる前に、内容をメモに残す（タブは変えない。そのまま閉じる）。残せたら `true`。
    /// 失敗したら理由を出して `false`（閉じない）。
    pub(super) fn keep_in_memo(&mut self, id: DocumentId) -> bool {
        match self.create_memo_from(id, None) {
            Ok(memo) => {
                self.set_status(
                    &format!("メモ「{}」として保存しました", memo.name),
                    MessageKind::Info,
                );
                true
            }
            Err(e) => {
                self.report_failure("メモに保存できませんでした", &e);
                false
            }
        }
    }

    /// 文書の今の本文から、保存領域に新しいメモを作る（タブは変えない）。名前は `name`
    /// （空なら、名前のない文書は本文の最初の一文、ファイルはそのファイル名）。
    pub(super) fn create_memo_from(&mut self, id: DocumentId, name: Option<&str>) -> Result<Memo> {
        self.sync();
        let i = self.index_of(id)?;
        if !matches!(self.docs[i].view, super::DocView::Scintilla(_)) {
            return Err(Error::unsupported(
                "巨大ファイルは読み取り専用のため、メモにできません",
            ));
        }
        let store = self
            .memo_store()
            .ok_or_else(|| Error::unavailable("メモ", "データの保存場所がありません"))?;
        let (_, text) = self.text_snapshot(id)?;
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None if self.is_unnamed(i) => crate::memo::derive_name(&text),
            None => self.docs[i].title.clone(),
        };
        let memo = store.create(&name, &text)?;
        if let Some(panel) = self.memo_panel {
            panel.refresh();
        }
        Ok(memo)
    }

    /// 「メモとして保存」（`file.save_as_memo`）。
    ///
    /// - 無題・クリップボード由来の文書: 同じタブのままメモに切り替える（本文・Undo・カーソルは
    ///   そのまま。以降はメモの自動保存）。
    /// - ファイルを開いている文書: メモに複製して新しいタブで開く（元のタブは変えない）。
    /// - メモ: 今の内容を保存する。
    ///
    /// 保存に失敗したときは何も変えない（タブは未保存のまま）。
    pub fn save_as_memo(&mut self, id: DocumentId, name: Option<&str>) -> Result<()> {
        self.sync();
        let i = self.index_of(id)?;
        if matches!(self.docs[i].state.kind(), DocumentKind::Memo(_)) {
            return if self.save_memo_now(id) {
                Ok(())
            } else {
                Err(Error::Busy {
                    what: "メモの保存".into(),
                })
            };
        }
        let in_place = self.is_unnamed(i);
        let version = self.docs[i].state.version();
        let memo = self.create_memo_from(id, name)?;
        if in_place {
            let i = self.index_of(id)?;
            if !self.docs[i].state.become_memo(memo.id.clone(), version) {
                return Ok(());
            }
            self.docs[i].memo = Some(MemoTab::opened(&memo));
            // 内容はメモに保存した。無題の復元データはいらない。
            if let Some(slot) = self.docs[i].slot.take() {
                super::recover::discard_slot(slot);
            }
            self.docs[i].pending = None;
            self.docs[i].savepoint_valid = true;
            self.with_text(id, |ed| ed.set_save_point())?;
            self.set_title(id, memo.name.clone())?;
            self.sync();
            self.refresh_tabs();
            self.refresh_status();
            self.set_status(
                &format!("メモ「{}」として保存しました", memo.name),
                MessageKind::Info,
            );
        } else {
            let name = memo.name.clone();
            self.open_memo(memo)?;
            self.set_status(
                &format!("メモ「{name}」に複製しました（元のファイルは変えていません）"),
                MessageKind::Info,
            );
        }
        Ok(())
    }

    /// 開いているメモの名前を変える。
    pub fn rename_memo(&mut self, id: DocumentId, name: &str) -> Result<()> {
        let i = self.index_of(id)?;
        let store = self
            .memo_store()
            .ok_or_else(|| Error::unavailable("メモ", "データの保存場所がありません"))?;
        let tab = self.docs[i]
            .memo
            .as_mut()
            .ok_or_else(|| Error::unsupported("メモではない文書の名前の変更"))?;
        let memo = store.rename(tab.id(), tab.store_version(), name)?;
        tab.adopt(&memo);
        self.set_title(id, memo.name)
    }
}
