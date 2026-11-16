//! すべて保存と、閉じたタブの開き直し（V2）。
//!
//! - すべて保存: 変更のある文書を左から順に保存する。名前のない文書は保存ダイアログを出し、
//!   取り消したらそこで止める。メモは保存し、巨大ファイルは対象外。
//! - 閉じたタブの開き直し: 閉じたファイルのパスとカーソル位置、メモの ID を最大
//!   [`CLOSED_TABS_MAX`] 件、その起動の間だけ覚える。新しいものから開き、ファイルが
//!   なければ飛ばして次へ進む。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::command::{CommandDef, CommandId, Registry};
use crate::core::{DocumentId, MemoId, Result};
use crate::document::DocumentKind;
use crate::ui::statusbar::MessageKind;

use super::fileio::{After, SaveDest, SaveOpts};
use super::{App, DocView};

/// 覚えておく閉じたタブの最大数。
pub(super) const CLOSED_TABS_MAX: usize = 20;

pub const FILE_SAVE_ALL: CommandDef = CommandDef::new(
    CommandId::new("file.save_all"),
    "すべて保存",
    "変更のある文書を左から順に保存する。名前のない文書は保存先を尋ね、取り消すとそこで止まる。メモは保存し、巨大ファイルは対象外。",
)
.result("{\"saved\": 保存を始めた（メモは保存した）数, \"cancelled\": 途中で取り消したか}")
.default_key("Ctrl+Alt+S");

pub const FILE_REOPEN_CLOSED: CommandDef = CommandDef::new(
    CommandId::new("file.reopen_closed"),
    "閉じたタブを開き直す",
    "最後に閉じたタブを開き直す（新しいものから最大 20 件、この起動の間だけ。ファイルがなければ飛ばして次へ。メモは ID で開く）。",
)
.result("{\"reopened\": 開き直したか}")
.default_key("Ctrl+Shift+T");

/// 閉じたタブ 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClosedTab {
    File {
        path: PathBuf,
        caret: u64,
        first_line: u64,
    },
    Memo(MemoId),
}

/// 閉じたタブの履歴（新しいものが末尾）。
#[derive(Debug, Default)]
pub(super) struct ClosedTabs {
    list: Vec<ClosedTab>,
    /// 開き直しの読み込みが終わるのを待っているカーソル位置。
    pending_view: HashMap<PathBuf, (u64, u64)>,
    /// 記録を止めている（受け渡し・開き直しで置き換えるときなど）。
    quiet: bool,
}

impl ClosedTabs {
    /// 記録する。同じファイル・メモの古い記録は消し、最大数を超えたら古いものを捨てる。
    pub(super) fn push(&mut self, tab: ClosedTab) {
        if self.quiet {
            return;
        }
        let same = |o: &ClosedTab| match (o, &tab) {
            (ClosedTab::File { path: a, .. }, ClosedTab::File { path: b, .. }) => a == b,
            (ClosedTab::Memo(a), ClosedTab::Memo(b)) => a == b,
            _ => false,
        };
        self.list.retain(|o| !same(o));
        self.list.push(tab);
        if self.list.len() > CLOSED_TABS_MAX {
            self.list.remove(0);
        }
    }

    /// 一番新しい記録を取り出す。
    pub(super) fn pop(&mut self) -> Option<ClosedTab> {
        self.list.pop()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

impl App {
    /// 閉じる直前の文書 `i` を履歴に加える（ファイルのある文書とメモだけ）。
    pub(super) fn remember_closed_tab(&mut self, i: usize) {
        let d = &self.docs[i];
        let tab = match d.state.kind() {
            DocumentKind::File | DocumentKind::LargeFile => {
                let Some(path) = d.state.path() else { return };
                let view = if self.active == Some(i) && matches!(d.view, DocView::Scintilla(_)) {
                    self.editor.view_state()
                } else {
                    d.view_state
                };
                ClosedTab::File {
                    path: path.to_path_buf(),
                    caret: view.selection.caret,
                    first_line: view.first_visible_line,
                }
            }
            DocumentKind::Memo(id) => ClosedTab::Memo(
                d.memo
                    .as_ref()
                    .map_or_else(|| id.clone(), |t| t.id().clone()),
            ),
            _ => return,
        };
        self.closed_tabs.push(tab);
    }

    /// 開き直せる記録があるか（メニューの有効・無効）。
    pub(super) fn closed_tabs_is_empty(&self) -> bool {
        self.closed_tabs.is_empty()
    }

    /// 自己診断用: 履歴を空にする。
    pub(crate) fn closed_tabs_clear_for_test(&mut self) {
        self.closed_tabs = ClosedTabs::default();
    }

    /// 履歴に残さずに閉じる（別の文書への置き換え・受け渡しで閉じるとき）。
    pub(super) fn close_document_quietly(&mut self, id: DocumentId) -> Result<bool> {
        let before = std::mem::replace(&mut self.closed_tabs.quiet, true);
        let result = self.close_document(id, true);
        self.closed_tabs.quiet = before;
        result
    }

    /// 開き終えた文書に、開き直しで覚えていたカーソル位置を戻す。
    pub(super) fn apply_reopen_view(&mut self, id: DocumentId) {
        if self.closed_tabs.pending_view.is_empty() {
            return;
        }
        let Some(path) = self.state(id).and_then(|s| s.path()).map(Path::to_path_buf) else {
            return;
        };
        if let Some((caret, first_line)) = self.closed_tabs.pending_view.remove(&path) {
            self.restore_view_at(id, caret, first_line);
        }
    }

    /// 閉じたタブを新しいものから開き直す。開き直したら `true`。
    pub fn reopen_closed_tab(&mut self) -> bool {
        while let Some(tab) = self.closed_tabs.pop() {
            match tab {
                ClosedTab::File {
                    path,
                    caret,
                    first_line,
                } => {
                    if !path.is_file() {
                        continue;
                    }
                    let open = self
                        .docs
                        .iter()
                        .any(|d| d.state.path() == Some(path.as_path()));
                    if !open {
                        self.closed_tabs
                            .pending_view
                            .insert(path.clone(), (caret, first_line));
                    }
                    self.open_path(&path);
                    return true;
                }
                ClosedTab::Memo(id) => {
                    let Some(store) = self.memo_store() else {
                        continue;
                    };
                    let Ok(memo) = store.load(&id) else { continue };
                    if self.open_memo(memo).is_ok() {
                        return true;
                    }
                }
            }
        }
        self.set_status("開き直せる閉じたタブがありません", MessageKind::Info);
        false
    }

    /// 変更のある文書を左から順に保存する。`(保存した数, 取り消したか)`。
    pub fn save_all(&mut self) -> (usize, bool) {
        let ids: Vec<DocumentId> = self
            .docs
            .iter()
            .filter(|d| d.state.is_dirty() && matches!(d.view, DocView::Scintilla(_)))
            .map(|d| d.id())
            .collect();
        let mut saved = 0;
        for id in ids {
            let Ok(i) = self.index_of(id) else { continue };
            if matches!(self.docs[i].state.kind(), DocumentKind::Memo(_)) {
                if self.save_memo_now(id) {
                    saved += 1;
                }
                continue;
            }
            if self.docs[i].state.path().is_none() && self.wants_memo_save(id) {
                match self.save_as_memo(id, None) {
                    Ok(()) => saved += 1,
                    Err(e) => self.report_failure("保存できませんでした", &e),
                }
                continue;
            }
            if self.docs[i].state.path().is_none() {
                let _ = self.activate(id);
                match self.save_as_flow_asked(id, After::Nothing) {
                    Ok(true) => saved += 1,
                    Ok(false) => {
                        self.set_status("すべて保存を中止しました", MessageKind::Info);
                        return (saved, true);
                    }
                    Err(e) => self.report_failure("保存できませんでした", &e),
                }
            } else if let Err(e) =
                self.save_document(id, SaveDest::Current, SaveOpts::new(After::Nothing))
            {
                self.report_failure("保存できませんでした", &e);
            } else {
                saved += 1;
            }
        }
        self.set_status(&format!("すべて保存: {saved} 件"), MessageKind::Info);
        (saved, false)
    }
}

/// 登録表につなぐ。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(FILE_SAVE_ALL, |app, _| {
        let (saved, cancelled) = app.save_all();
        Ok(serde_json::json!({ "saved": saved, "cancelled": cancelled }))
    })?;
    registry.register(FILE_REOPEN_CLOSED, |app, _| {
        Ok(serde_json::json!({ "reopened": app.reopen_closed_tab() }))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(p: &str) -> ClosedTab {
        ClosedTab::File {
            path: PathBuf::from(p),
            caret: 1,
            first_line: 0,
        }
    }

    #[test]
    fn newest_first_and_deduplicated() {
        let mut h = ClosedTabs::default();
        h.push(file("a"));
        h.push(file("b"));
        h.push(file("a"));
        assert_eq!(h.pop(), Some(file("a")));
        assert_eq!(h.pop(), Some(file("b")));
        assert_eq!(h.pop(), None);
        assert!(h.is_empty());
    }

    #[test]
    fn keeps_at_most_twenty() {
        let mut h = ClosedTabs::default();
        for i in 0..30 {
            h.push(file(&format!("f{i}")));
        }
        assert_eq!(h.list.len(), CLOSED_TABS_MAX);
        assert_eq!(h.pop(), Some(file("f29")));
        assert_eq!(h.list.first(), Some(&file("f10")));
    }

    #[test]
    fn quiet_records_nothing() {
        let mut h = ClosedTabs {
            quiet: true,
            ..Default::default()
        };
        h.push(file("a"));
        assert!(h.is_empty());
    }
}
