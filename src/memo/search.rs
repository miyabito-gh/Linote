//! メモの検索。
//!
//! 取り決め: 大文字小文字を区別せず、全角英数・記号を半角に、ひらがなをカタカナに
//! そろえて比べる（[`fold_for_search`]）。検索語は空白で区切った AND で、
//! 各語が名前か本文のどちらかに含まれればよい。名前に当たったものを先に並べ、
//! 同じなら更新の新しい順。語が空なら全件（更新の新しい順）。

use crate::core::{Budget, Completion, MemoId, Outcome, TaskContext};

use super::Memo;

/// 検索の 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoHit {
    pub id: MemoId,
    pub name: String,
    pub updated_ms: u64,
    /// 名前に当たったか。
    pub name_match: bool,
    /// 本文の当たりの周辺（本文に当たったときだけ。制御文字は空白にする）。
    pub snippet: Option<String>,
}

/// 検索用にそろえる。全角 ASCII（U+FF01..U+FF5E）と全角空白を半角に、
/// ひらがな（U+3041..U+3096）をカタカナに、英字を小文字にする。
/// 文字数は変わらない（1 文字 → 1 文字）。
pub fn fold_for_search(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    let c = match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3041}'..='\u{3096}' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
        _ => c,
    };
    c.to_lowercase().next().unwrap_or(c)
}

pub(super) fn search(memos: &[Memo], query: &str, ctx: &TaskContext) -> Outcome<Vec<MemoHit>> {
    let folded = fold_for_search(query);
    let terms: Vec<&str> = folded.split_whitespace().collect();
    let mut hits = Vec::new();
    let mut budget = Budget::new(ctx.limits);
    let mut completion = Completion::Complete;
    for memo in memos {
        if ctx.cancel.is_cancelled() {
            completion = Completion::Cancelled;
            break;
        }
        let name = fold_for_search(&memo.name);
        let body = fold_for_search(&memo.body);
        if !terms.iter().all(|t| name.contains(t) || body.contains(t)) {
            continue;
        }
        let name_match = terms.iter().any(|t| name.contains(t));
        let snippet = terms
            .iter()
            .find_map(|t| body.find(t))
            .map(|pos| snippet_at(&memo.body, &body, pos));
        if let Err(c) = budget.try_add(memo.name.len() as u64) {
            completion = c;
            break;
        }
        hits.push(MemoHit {
            id: memo.id.clone(),
            name: memo.name.clone(),
            updated_ms: memo.updated_ms,
            name_match,
            snippet,
        });
    }
    // 安定ソート: 元の順（更新の新しい順）を保ったまま、名前に当たったものを前へ。
    hits.sort_by_key(|h| !h.name_match);
    Outcome::new(hits, completion)
}

/// `folded` のバイト位置 `pos` を、同じ文字位置の `original` の周辺に直す。
fn snippet_at(original: &str, folded: &str, pos: usize) -> String {
    let char_pos = folded[..pos].chars().count();
    let chars: Vec<char> = original.chars().collect();
    let start = char_pos.saturating_sub(20);
    let end = (char_pos + 60).min(chars.len());
    chars[start..end]
        .iter()
        .map(|c| if c.is_control() { ' ' } else { *c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_width_case_and_kana() {
        assert_eq!(fold_for_search("ＡＢｃ１２３　あ"), "abc123 ア");
    }
}
