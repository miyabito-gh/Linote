//! 検索の一致の強調表示（改善 U3）。
//!
//! - 検索欄の入力に合わせて（[`FindEvent::Changed`](crate::ui::find::FindEvent::Changed)）、
//!   アクティブな文書のすべての一致を Scintilla のインジケーターで強調する。現在の一致
//!   （選択と一致する範囲）は別の色。F3 などで検索欄が閉じていても強調する。
//! - 強調は最大 [`MAX_HIGHLIGHTS`] 件。超えたら件数に「+」を付ける。1 MiB 以上の文書は
//!   本文の写しをワーカーで検索し、結果を文書の版と世代で照合してから反映する。
//! - 消えるとき: Esc、文書の編集、別のタブへの切り替え、検索条件が空・無効。
//! - 巨大モードの文書は強調しない（一致の移動だけ）。

use crate::core::WorkerPool;
use crate::core::{CancelToken, Completion, DocStamp, DocumentId, Outcome, Result, TaskContext};
use crate::document::{MemoryText, TextRange};
use crate::search::SearchOptions;
use crate::ui::find::FindPanel;

use super::{App, UiMessage};

/// 強調する一致の上限。
pub(super) const MAX_HIGHLIGHTS: usize = 20_000;
/// これ以上の大きさの文書は、強調の検索をワーカーで行う。
const HIGHLIGHT_WORKER_BYTES: usize = 1024 * 1024;

/// 強調の状態（文書ごとではなく、同時に 1 文書だけ）。
#[derive(Default)]
pub(super) struct Highlight {
    shown: Option<Shown>,
    cancel: Option<CancelToken>,
    generation: u64,
}

struct Shown {
    doc: DocumentId,
    options: SearchOptions,
    ranges: Vec<TextRange>,
    truncated: bool,
}

/// 現在の一致（選択と同じ範囲）が何番目か。
pub(super) fn current_index(ranges: &[TextRange], selection: TextRange) -> Option<usize> {
    let i = ranges
        .binary_search_by_key(&selection.start, |r| r.start)
        .ok()?;
    (ranges[i].end == selection.end).then_some(i)
}

/// 「n / 全 m 件」（`n` が分からなければ「全 m 件」）。
pub(super) fn count_label(index: Option<usize>, total: usize, truncated: bool) -> String {
    let m = if truncated {
        format!("{total}+")
    } else {
        total.to_string()
    };
    match index {
        Some(i) => format!("{} / 全 {m} 件", i + 1),
        None => format!("全 {m} 件"),
    }
}

impl App {
    /// 強調している文書。
    pub(super) fn highlight_doc(&self) -> Option<DocumentId> {
        self.highlight
            .shown
            .as_ref()
            .map(|s| s.doc)
            .filter(|d| self.state(*d).is_some())
    }

    /// 強調している一致の数（試験用）。
    pub fn highlight_count(&self) -> usize {
        self.highlight.shown.as_ref().map_or(0, |s| s.ranges.len())
    }

    /// 強調を消す（作業中の検索も中止する）。
    pub(super) fn clear_highlight(&mut self) {
        self.highlight.generation += 1;
        if let Some(t) = self.highlight.cancel.take() {
            t.cancel();
        }
        if let Some(s) = self.highlight.shown.take() {
            let _ = self.with_text(s.doc, |ed| ed.clear_match_highlight());
        }
    }

    /// 文書の編集などで強調を消し、検索欄に出していた件数も消す。
    pub(super) fn clear_highlight_and_count(&mut self) {
        if self.highlight.shown.is_none() {
            return;
        }
        self.clear_highlight();
        if let Some(p) = self.find_panel.filter(FindPanel::is_visible) {
            p.set_status("", false);
        }
    }

    /// 検索欄の条件が変わったとき。
    pub(super) fn on_query_changed(&mut self, options: SearchOptions) {
        if options.query.is_empty() || options.compile().is_err() {
            self.clear_highlight();
            return;
        }
        self.refresh_highlight(options, true);
    }

    /// アクティブな文書に `options` の強調が付いていなければ付ける（付いていれば、現在の
    /// 一致の印だけ更新する）。一致の数の案内は出さない（[`Self::report_highlight`]）。
    pub(super) fn ensure_highlight(&mut self, options: &SearchOptions) {
        let Some(id) = self.active_id() else { return };
        if self.bigviews.contains_key(&id) {
            self.clear_highlight();
            return;
        }
        let same = self
            .highlight
            .shown
            .as_ref()
            .is_some_and(|s| s.doc == id && &s.options == options);
        if same {
            self.update_current_match();
        } else {
            self.refresh_highlight(options.clone(), false);
        }
    }

    /// 強調を検索し直す。小さい文書はその場で、大きい文書はワーカーで。
    fn refresh_highlight(&mut self, options: SearchOptions, report: bool) {
        self.clear_highlight();
        let Some(id) = self.active_id() else { return };
        if self.bigviews.contains_key(&id) {
            return;
        }
        let Ok(searcher) = options.compile() else {
            return;
        };
        let Ok(len) = self.with_text(id, |ed| ed.length()) else {
            return;
        };
        let worker_bytes = self.worker_search_bytes.min(HIGHLIGHT_WORKER_BYTES);
        if (len as usize) < worker_bytes {
            let ctx = TaskContext::detached();
            let found = self.with_text(id, |ed| searcher.find_all(ed, &ctx, MAX_HIGHLIGHTS));
            if let Ok(Ok(outcome)) = found {
                self.show_highlight(id, options, outcome.value, report);
            }
            return;
        }
        let Ok((stamp, text)) = self.text_snapshot(id) else {
            return;
        };
        let token = CancelToken::new();
        self.highlight.cancel = Some(token.clone());
        let generation = self.highlight.generation;
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let mut ctx = TaskContext::detached();
            ctx.cancel = token;
            let src = MemoryText::new(text);
            let result = searcher.find_all(&src, &ctx, MAX_HIGHLIGHTS);
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_highlight(stamp, generation, options, result, report)
            })));
        });
    }

    fn finish_highlight(
        &mut self,
        stamp: DocStamp,
        generation: u64,
        options: SearchOptions,
        result: Result<Outcome<(Vec<TextRange>, bool)>>,
        report: bool,
    ) {
        // 検索している間に、編集・切り替え・条件の変更があったときは使わない。
        if generation != self.highlight.generation
            || !self.is_current(stamp)
            || self.active_id() != Some(stamp.document)
        {
            return;
        }
        if let Ok(outcome) = result {
            if outcome.completion == Completion::Complete {
                self.show_highlight(stamp.document, options, outcome.value, report);
            }
        }
    }

    fn show_highlight(
        &mut self,
        id: DocumentId,
        options: SearchOptions,
        found: (Vec<TextRange>, bool),
        report: bool,
    ) {
        let (ranges, truncated) = found;
        if self
            .with_text(id, |ed| ed.set_match_highlight(&ranges))
            .is_err()
        {
            return;
        }
        self.highlight.shown = Some(Shown {
            doc: id,
            options,
            ranges,
            truncated,
        });
        self.update_current_match();
        if report {
            self.report_highlight(false);
        }
    }

    /// 現在の選択に合わせて、現在の一致の印を付け替える。
    pub(super) fn update_current_match(&mut self) {
        let Some(id) = self.highlight_doc() else {
            return;
        };
        let Ok(sel) = self.with_text(id, |ed| ed.selection().range()) else {
            return;
        };
        let current = self
            .highlight
            .shown
            .as_ref()
            .and_then(|s| current_index(&s.ranges, sel).map(|i| s.ranges[i]));
        let _ = self.with_text(id, |ed| ed.set_current_match(current));
    }

    /// 一致の数の案内（「n / 全 m 件」）を出す。強調がまだなければ `false`。
    pub(super) fn report_highlight(&mut self, wrapped: bool) -> bool {
        let Some(s) = &self.highlight.shown else {
            return false;
        };
        let id = s.doc;
        let Ok(sel) = self.with_text(id, |ed| ed.selection().range()) else {
            return false;
        };
        let Some(s) = &self.highlight.shown else {
            return false;
        };
        let mut text = if s.ranges.is_empty() {
            "見つかりません".to_owned()
        } else {
            count_label(current_index(&s.ranges, sel), s.ranges.len(), s.truncated)
        };
        if wrapped {
            text.push_str("（端で折り返しました）");
        }
        let error = s.ranges.is_empty();
        self.find_message(&text, error);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: u64, b: u64) -> TextRange {
        TextRange::new(a, b)
    }

    #[test]
    fn current_index_needs_exact_range() {
        let v = [r(0, 2), r(6, 8), r(12, 14)];
        assert_eq!(current_index(&v, r(6, 8)), Some(1));
        assert_eq!(current_index(&v, r(6, 7)), None);
        assert_eq!(current_index(&v, r(7, 8)), None);
        assert_eq!(current_index(&v, r(0, 2)), Some(0));
    }

    #[test]
    fn count_labels() {
        assert_eq!(count_label(Some(1), 3, false), "2 / 全 3 件");
        assert_eq!(count_label(None, 3, false), "全 3 件");
        assert_eq!(count_label(Some(0), 20000, true), "1 / 全 20000+ 件");
    }
}
