//! 行の整理（V6）。選択を行単位に広げ、行末の形式と末尾の改行の有無を保つ。
use std::collections::HashSet;
use std::ops::Range;

use super::tidy::TextEdit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOperation {
    SortAscending,
    SortDescending,
    RemoveDuplicates,
}

/// 改行を含む各行の範囲。末尾の改行の後にある空の行も数える。
fn lines(text: &str) -> Vec<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'\r' | b'\n') {
            let body_end = i;
            i += if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            result.push((start, body_end, i));
            start = i;
        } else {
            i += 1;
        }
    }
    result.push((start, bytes.len(), bytes.len()));
    result
}

/// 選択に触れる行全体。単一のカーソルだけなら本文全体。複数選択は外側の行まで含める。
/// 選択の終わりが行頭なら、その行を含めない。位置は UTF-8 のバイト位置。
pub fn selected_line_range(text: &str, selections: &[Range<usize>]) -> Range<usize> {
    if selections.is_empty() || (selections.len() == 1 && selections[0].is_empty()) {
        return 0..text.len();
    }
    let rows = lines(text);
    let mut first = text.len();
    let mut last = 0;
    for selection in selections {
        let start = selection.start.min(text.len());
        let end = selection.end.min(text.len()).max(start);
        let a = rows.partition_point(|r| r.0 <= start).saturating_sub(1);
        // 非空の選択だけは終端を含めない。カーソルだけの追加選択はその行を含める。
        let last_pos = if end > start { end - 1 } else { end };
        let b = rows.partition_point(|r| r.0 <= last_pos).saturating_sub(1);
        first = first.min(rows[a].0);
        last = last.max(rows[b].2);
    }
    first..last
}

/// 整理に必要な 1 件の置換。変化がなければ `None`（版や Undo 履歴を動かさない）。
/// 比較は UTF-8 の辞書順＝Unicode コードポイント順。改行は比較・重複判定に含めない。
/// 混在改行は位置ごとの区切りとして保ち、末尾の改行の有無も変えない。
pub fn organize_lines(
    text: &str,
    selections: &[Range<usize>],
    operation: LineOperation,
) -> Option<TextEdit> {
    let range = selected_line_range(text, selections);
    let region = &text[range.clone()];
    let mut rows = lines(region);
    // 末尾の改行の後にある仮想の空行は並べ替えない。
    if rows.len() > 1 && rows.last().is_some_and(|r| r.0 == region.len()) {
        rows.pop();
    }
    let mut bodies: Vec<&str> = rows.iter().map(|&(a, b, _)| &region[a..b]).collect();
    match operation {
        LineOperation::SortAscending => bodies.sort(),
        LineOperation::SortDescending => bodies.sort_by(|a, b| b.cmp(a)),
        LineOperation::RemoveDuplicates => {
            let mut seen = HashSet::new();
            bodies.retain(|line| seen.insert(*line));
        }
    }
    let mut replacement = String::with_capacity(region.len());
    for (i, body) in bodies.iter().enumerate() {
        replacement.push_str(body);
        let separator = if i + 1 == bodies.len() {
            rows.last().unwrap()
        } else {
            &rows[i]
        };
        replacement.push_str(&region[separator.1..separator.2]);
    }
    (replacement != region).then_some(TextEdit {
        start: range.start,
        end: range.end,
        text: replacement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tidy::apply;
    fn arranged(text: &str, selections: &[Range<usize>], operation: LineOperation) -> String {
        apply(
            text,
            &organize_lines(text, selections, operation)
                .into_iter()
                .collect::<Vec<_>>(),
        )
    }
    #[test]
    fn sort_preserves_endings_and_final_newline() {
        for eol in ["\n", "\r\n", "\r"] {
            for final_eol in ["", eol] {
                let before = format!("b{eol}a{eol}c{final_eol}");
                assert_eq!(
                    arranged(&before, &[], LineOperation::SortAscending),
                    format!("a{eol}b{eol}c{final_eol}")
                );
                assert_eq!(
                    arranged(&before, &[], LineOperation::SortDescending),
                    format!("c{eol}b{eol}a{final_eol}")
                );
            }
        }
        assert_eq!(
            arranged("c\r\na\nb", &[], LineOperation::SortAscending),
            "a\r\nb\nc"
        );
    }
    #[test]
    fn unicode_order_is_case_sensitive_and_empty_lines_participate() {
        assert_eq!(
            arranged("猫\na\n\nA\nあ\n", &[], LineOperation::SortAscending),
            "\nA\na\nあ\n猫\n"
        );
        assert_eq!(
            arranged("2\n10", &[], LineOperation::SortAscending),
            "10\n2"
        );
    }
    #[test]
    fn duplicates_keep_first_occurrence_and_final_eol() {
        assert_eq!(
            arranged("b\na\nb\nA\na", &[], LineOperation::RemoveDuplicates),
            "b\na\nA"
        );
        assert_eq!(
            arranged("b\r\na\r\nb\r\n", &[], LineOperation::RemoveDuplicates),
            "b\r\na\r\n"
        );
        assert_eq!(
            arranged("\n\n\na\n\n", &[], LineOperation::RemoveDuplicates),
            "\na\n"
        );
        assert_eq!(
            arranged("a\na\n", &[], LineOperation::RemoveDuplicates),
            "a\n"
        );
    }
    #[test]
    fn selection_expands_to_lines_but_excludes_next_line_start() {
        let text = "z\nc\nb\na\nx";
        assert_eq!(
            selected_line_range(text, std::slice::from_ref(&(3..6))),
            2..6
        );
        assert_eq!(
            arranged(
                text,
                std::slice::from_ref(&(3..6)),
                LineOperation::SortAscending
            ),
            "z\nb\nc\na\nx"
        );
        assert_eq!(selected_line_range(text, &[3..5, 7..8]), 2..8);
        assert_eq!(
            arranged(text, &[3..5, 7..8], LineOperation::SortAscending),
            "z\na\nb\nc\nx"
        );
        assert_eq!(
            selected_line_range("a\r\nb\r\nc", std::slice::from_ref(&(1..3))),
            0..3
        );
    }
    #[test]
    fn no_selection_means_whole_document_and_unchanged_is_none() {
        assert_eq!(
            selected_line_range("a\nb", std::slice::from_ref(&(2..2))),
            0..3
        );
        assert_eq!(selected_line_range("x\nc\nb\na\nz", &[3..3, 7..7]), 2..8);
        for text in ["", "a", "a\n", "\n", "a\nb"] {
            assert!(organize_lines(text, &[], LineOperation::SortAscending).is_none());
            assert!(organize_lines(text, &[], LineOperation::RemoveDuplicates).is_none());
        }
    }
}
