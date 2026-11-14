//! 検索の強調表示（インジケーター）と編集コマンド（行の複製・削除・移動、大文字小文字、
//! 行末の空白、タブ⇔スペース、日時、対応する括弧）の編集欄側の処理。
//!
//! 編集はどれも 1 つの Undo のまとまりにする。計算は [`super::transform`] の純粋な関数。

use super::sci::*;
use super::transform::{self, Edit};
use super::viewopts::is_brace;
use super::{cstr, Editor};
use crate::document::{Selection, TextRange};
use crate::text::tidy::{save_tidy_edits, TidyOptions};

/// すべての一致を強調するインジケーター。
const INDIC_ALL: usize = 9;
/// 現在の一致を強調するインジケーター。
const INDIC_CURRENT: usize = 10;
/// 「選択範囲内」の検索の対象範囲を示すインジケーター（編集で範囲がずれても追従する）。
const INDIC_SCOPE: usize = 11;

impl Editor {
    /// 検索の強調のインジケーターの見た目を決める（作成時に 1 回）。色は明暗どちらの
    /// 背景でも読める琥珀色と橙色の半透明。
    pub(super) fn configure_match_indicators(&self) {
        for (indic, rgb, alpha, outline) in [
            (INDIC_ALL, 0x00C8FFusize, 80usize, 150usize),
            (INDIC_CURRENT, 0x0070FF, 150, 220),
        ] {
            self.send(SCI_INDICSETSTYLE, indic, INDIC_STRAIGHTBOX as isize);
            self.send(SCI_INDICSETFORE, indic, rgb as isize);
            self.send(SCI_INDICSETALPHA, indic, alpha as isize);
            self.send(SCI_INDICSETOUTLINEALPHA, indic, outline as isize);
            self.send(SCI_INDICSETUNDER, indic, 1);
        }
        // 検索範囲: 青系の薄い枠（一致の強調と区別できるように）。
        self.send(SCI_INDICSETSTYLE, INDIC_SCOPE, INDIC_FULLBOX as isize);
        self.send(SCI_INDICSETFORE, INDIC_SCOPE, 0xE0A040);
        self.send(SCI_INDICSETALPHA, INDIC_SCOPE, 45);
        self.send(SCI_INDICSETOUTLINEALPHA, INDIC_SCOPE, 120);
        self.send(SCI_INDICSETUNDER, INDIC_SCOPE, 1);
    }

    /// 検索範囲のインジケーターを `range` に付け替える（前の範囲は消す）。
    pub fn set_search_scope(&self, range: TextRange) {
        self.clear_search_scope();
        self.send(SCI_SETINDICATORCURRENT, INDIC_SCOPE, 0);
        self.send(
            SCI_INDICATORFILLRANGE,
            range.start as usize,
            range.len() as isize,
        );
    }

    /// 検索範囲のインジケーターを消す。
    pub fn clear_search_scope(&self) {
        self.send(SCI_SETINDICATORCURRENT, INDIC_SCOPE, 0);
        self.send(SCI_INDICATORCLEARRANGE, 0, self.length() as isize);
    }

    /// 検索範囲（インジケーターが付いている区間）。編集でずれた後の位置を返す。
    /// 付いていない（または空になった）ときは `None`。
    pub fn search_scope(&self) -> Option<TextRange> {
        let len = self.length();
        // 位置 0 から見て、付いていない区間の終わりが範囲の始まり（0 から付いていれば 0）。
        let start = self.send(SCI_INDICATOREND, INDIC_SCOPE, 0).max(0) as u64;
        let start = if self.send(SCI_INDICATORVALUEAT, INDIC_SCOPE, 0) != 0 {
            0
        } else {
            start
        };
        if start >= len {
            return None;
        }
        let end = (self
            .send(SCI_INDICATOREND, INDIC_SCOPE, start as isize)
            .max(0) as u64)
            .min(len);
        (end > start).then(|| TextRange::new(start, end))
    }

    /// すべての一致を強調する（前の強調は消す）。
    pub fn set_match_highlight(&self, ranges: &[TextRange]) {
        self.clear_match_highlight();
        self.send(SCI_SETINDICATORCURRENT, INDIC_ALL, 0);
        for r in ranges {
            self.send(SCI_INDICATORFILLRANGE, r.start as usize, r.len() as isize);
        }
    }

    /// 現在の一致の強調を `range` に付け替える（`None` なら外す）。
    pub fn set_current_match(&self, range: Option<TextRange>) {
        self.send(SCI_SETINDICATORCURRENT, INDIC_CURRENT, 0);
        self.send(SCI_INDICATORCLEARRANGE, 0, self.length() as isize);
        if let Some(r) = range {
            self.send(SCI_INDICATORFILLRANGE, r.start as usize, r.len() as isize);
        }
    }

    /// 検索の強調をすべて消す。
    pub fn clear_match_highlight(&self) {
        let len = self.length() as isize;
        for indic in [INDIC_ALL, INDIC_CURRENT] {
            self.send(SCI_SETINDICATORCURRENT, indic, 0);
            self.send(SCI_INDICATORCLEARRANGE, 0, len);
        }
    }

    /// 位置 `pos` にある（または隣接する）単語。
    pub fn word_at(&self, pos: u64) -> Option<String> {
        let start = self.send(SCI_WORDSTARTPOSITION, pos as usize, 1).max(0) as u64;
        let end = self.send(SCI_WORDENDPOSITION, pos as usize, 1).max(0) as u64;
        if end <= start || end - start > 256 {
            return None;
        }
        Some(String::from_utf8_lossy(&self.text_range(start, end)).into_owned())
    }

    /// 選択があれば選択を、なければ現在の行を複製する。
    pub fn duplicate_line_or_selection(&self) {
        if self.selection().range().is_empty() {
            self.send(SCI_LINEDUPLICATE, 0, 0);
        } else {
            self.send(SCI_SELECTIONDUPLICATE, 0, 0);
        }
    }

    /// 選択にかかる行（なければカーソルの行）を削除する。
    pub fn delete_lines(&self) {
        let sel = self.selection().range();
        let first = self.line_from_position(sel.start);
        let mut last = self.line_from_position(sel.end);
        if sel.end > sel.start && last > first && self.line_start(last) == Some(sel.end) {
            last -= 1;
        }
        let (start, end) = if last + 1 < self.line_count() {
            (
                self.line_start(first).unwrap_or(0),
                self.line_start(last + 1).unwrap_or(self.length()),
            )
        } else if first > 0 {
            // 最後の行: 直前の改行ごと取り除く。
            (self.line_end(first - 1), self.length())
        } else {
            (0, self.length())
        };
        if end > start {
            self.send(SCI_DELETERANGE, start as usize, (end - start) as isize);
        }
    }

    /// 選択にかかる行（なければカーソルの行）を上（下）へ移動する。
    pub fn move_lines(&self, up: bool) {
        self.send(
            if up {
                SCI_MOVESELECTEDLINESUP
            } else {
                SCI_MOVESELECTEDLINESDOWN
            },
            0,
            0,
        );
    }

    /// 選択を大文字（小文字）にする。選択がなければ `false`。
    pub fn convert_case(&self, upper: bool) -> bool {
        if self.selection().range().is_empty() {
            return false;
        }
        self.send(if upper { SCI_UPPERCASE } else { SCI_LOWERCASE }, 0, 0);
        true
    }

    /// 編集の対象範囲（選択にかかる行全体。選択がなければ本文全体）。
    fn edit_region(&self) -> (u64, u64) {
        let sel = self.selection().range();
        if sel.is_empty() {
            return (0, self.length());
        }
        let first = self.line_from_position(sel.start);
        let mut last = self.line_from_position(sel.end);
        if last > first && self.line_start(last) == Some(sel.end) {
            last -= 1;
        }
        (self.line_start(first).unwrap_or(0), self.line_end(last))
    }

    /// `base` からの位置で表した編集を、後ろから 1 つの Undo のまとまりとして適用する。
    fn apply_edits(&self, base: u64, edits: &[Edit]) {
        if edits.is_empty() {
            return;
        }
        self.begin_undo_action();
        for e in edits.iter().rev() {
            self.replace_raw(
                base + e.start as u64,
                base + e.end as u64,
                e.text.as_bytes(),
            );
        }
        self.end_undo_action();
    }

    /// 編集の対象範囲の本文に `f` を適用する。変更した箇所の数を返す。
    fn transform_region(&self, f: impl FnOnce(&[u8]) -> Vec<Edit>) -> usize {
        let (start, end) = self.edit_region();
        let text = self.text_range(start, end);
        let edits = f(&text);
        self.apply_edits(start, &edits);
        edits.len()
    }

    /// 行末の空白を削除する（選択があればその行だけ）。削除した行の数を返す。
    pub fn trim_trailing_whitespace(&self) -> usize {
        self.transform_region(transform::trailing_whitespace_edits)
    }

    /// 保存時の整形（V5）を本文全体に適用する。1 つの Undo のまとまりにし、先頭表示行と、
    /// 本文の末尾にあったカーソル・選択の端を保つ（行末の空白の削除では Scintilla が位置を
    /// 追従させる）。変化がなければ何もせず `false`。
    pub fn apply_save_tidy(&self, opts: &TidyOptions<'_>) -> bool {
        let len = self.length();
        let text = self.text_range(0, len);
        let edits = save_tidy_edits(&text, opts);
        if edits.is_empty() {
            return false;
        }
        let sel = self.selection();
        let top = self.send(SCI_GETFIRSTVISIBLELINE, 0, 0);
        self.apply_edits(0, &edits);
        // 末尾への挿入では、末尾にあったカーソルが挿入の後ろへ動くことがあるので戻す。
        if sel.anchor == len || sel.caret == len {
            let removed: u64 = edits.iter().map(|e| (e.end - e.start) as u64).sum();
            let end = len - removed;
            let now = self.selection();
            self.set_selection(Selection {
                anchor: if sel.anchor == len { end } else { now.anchor },
                caret: if sel.caret == len { end } else { now.caret },
            });
        }
        self.send(SCI_SETFIRSTVISIBLELINE, top.max(0) as usize, 0);
        true
    }

    /// タブ文字をスペースにする（選択があればその行だけ）。変換したタブの数を返す。
    pub fn tabs_to_spaces(&self) -> usize {
        let width = self.send(SCI_GETTABWIDTH, 0, 0).max(1) as usize;
        self.transform_region(|t| transform::tabs_to_spaces_edits(t, width))
    }

    /// 行頭のスペースをタブにする（選択があればその行だけ）。変換した行の数を返す。
    pub fn spaces_to_tabs(&self) -> usize {
        let width = self.send(SCI_GETTABWIDTH, 0, 0).max(1) as usize;
        self.transform_region(|t| transform::leading_spaces_to_tabs_edits(t, width))
    }

    /// 選択を `text` に置き換える（選択がなければカーソル位置に挿入）。
    pub fn replace_selection_text(&self, text: &str) {
        let c = cstr(text);
        self.send(SCI_REPLACESEL, 0, c.as_ptr() as isize);
    }

    /// カーソルの位置（またはすぐ前）の括弧に対応する括弧へ移動する。括弧がない、
    /// または対応がなければ `false`。
    pub fn goto_matching_brace(&self) -> bool {
        let caret = self.selection().caret;
        for pos in [Some(caret), caret.checked_sub(1)].into_iter().flatten() {
            if pos >= self.length() || !is_brace(self.byte_at(pos)) {
                continue;
            }
            let other = self.send(SCI_BRACEMATCH, pos as usize, 0);
            if other < 0 {
                return false;
            }
            self.set_selection(Selection::caret_at(other as u64));
            self.scroll_caret();
            return true;
        }
        false
    }
}
