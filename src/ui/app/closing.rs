//! 文書を閉じる・終了するときの判断（画面に依存しない純粋な関数）。
//!
//! 確認ダイアログ自体は [`crate::ui::dialogs::Prompts`]、実行は `fileio` が行う。ここは
//! 「何を尋ねるか」「選択の結果、何をするか」だけを決める（試験しやすいように分けた）。

use std::collections::HashMap;

use crate::core::{DocumentId, Version};
use crate::document::DocumentKind;
use crate::ui::dialogs::CloseChoice;

/// 未保存かもしれない文書を閉じるときの進め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosePlan {
    /// そのまま閉じる（未変更）。
    Close,
    /// メモ: 利用者に尋ねず、保存領域へ保存してから閉じる（失敗したら閉じない）。
    SaveMemoThenClose,
    /// 利用者に尋ねる（保存／保存しない／取り消し）。
    Ask,
}

/// 閉じるときの進め方を決める。
pub fn plan(kind: &DocumentKind, dirty: bool) -> ClosePlan {
    match (kind, dirty) {
        (_, false) => ClosePlan::Close,
        (DocumentKind::Memo(_), true) => ClosePlan::SaveMemoThenClose,
        // 巨大ファイルは編集できず、未保存にならない。
        (DocumentKind::LargeFile, true) => ClosePlan::Close,
        _ => ClosePlan::Ask,
    }
}

/// 利用者の選択に対する動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceAction {
    /// 保存を始め、成功したら閉じる。
    SaveThenClose,
    /// メモに保存して閉じる（保存先は選ばない）。
    SaveAsMemoThenClose,
    /// 保存せずに閉じる（復元データも破棄する）。
    DiscardAndClose,
    /// 閉じない。
    Keep,
}

pub fn act_on(choice: CloseChoice) -> ChoiceAction {
    match choice {
        CloseChoice::Save => ChoiceAction::SaveThenClose,
        CloseChoice::SaveAsMemo => ChoiceAction::SaveAsMemoThenClose,
        CloseChoice::Discard => ChoiceAction::DiscardAndClose,
        CloseChoice::Cancel => ChoiceAction::Keep,
    }
}

/// 保存の前に保存先を尋ねる必要があるか（まだファイルになっていない文書）。
pub fn needs_save_as(has_path: bool) -> bool {
    !has_path
}

/// 終了時に次に尋ねる文書。`docs` は（ID、未保存か、種類、版）を並び順に。「保存しない」と
/// 答えた文書は、そのときの版のままなら飛ばす（その後に編集していれば、もう一度尋ねる）。
/// 尋ねずに済む文書も飛ばす。
pub fn next_to_ask(
    docs: &[(DocumentId, bool, DocumentKind, Version)],
    discarded: &HashMap<DocumentId, Version>,
) -> Option<DocumentId> {
    docs.iter()
        .find(|(id, dirty, kind, version)| {
            discarded.get(id) != Some(version) && plan(kind, *dirty) == ClosePlan::Ask
        })
        .map(|(id, ..)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MemoId;

    #[test]
    fn plans_by_kind_and_state() {
        assert_eq!(plan(&DocumentKind::File, false), ClosePlan::Close);
        assert_eq!(plan(&DocumentKind::File, true), ClosePlan::Ask);
        assert_eq!(plan(&DocumentKind::Untitled, true), ClosePlan::Ask);
        assert_eq!(plan(&DocumentKind::Clipboard, true), ClosePlan::Ask);
        assert_eq!(
            plan(&DocumentKind::Memo(MemoId::generate()), true),
            ClosePlan::SaveMemoThenClose
        );
        assert_eq!(
            plan(&DocumentKind::Memo(MemoId::generate()), false),
            ClosePlan::Close
        );
        assert_eq!(plan(&DocumentKind::LargeFile, true), ClosePlan::Close);
    }

    #[test]
    fn choices_map_to_actions() {
        assert_eq!(act_on(CloseChoice::Save), ChoiceAction::SaveThenClose);
        assert_eq!(
            act_on(CloseChoice::SaveAsMemo),
            ChoiceAction::SaveAsMemoThenClose
        );
        assert_eq!(act_on(CloseChoice::Discard), ChoiceAction::DiscardAndClose);
        assert_eq!(act_on(CloseChoice::Cancel), ChoiceAction::Keep);
        assert!(needs_save_as(false) && !needs_save_as(true));
    }

    #[test]
    fn exit_asks_about_each_dirty_document_once() {
        let a = DocumentId::allocate();
        let b = DocumentId::allocate();
        let c = DocumentId::allocate();
        let v = Version::from_raw(3);
        let docs = vec![
            (a, false, DocumentKind::File, v),
            (b, true, DocumentKind::Untitled, v),
            (c, true, DocumentKind::File, v),
        ];
        let mut discarded = HashMap::new();
        assert_eq!(next_to_ask(&docs, &discarded), Some(b));
        discarded.insert(b, v);
        assert_eq!(next_to_ask(&docs, &discarded), Some(c));
        discarded.insert(c, v);
        assert_eq!(next_to_ask(&docs, &discarded), None);
        // 「保存しない」と答えた後に編集した文書は、もう一度尋ねる。
        let edited = vec![(b, true, DocumentKind::Untitled, v.next())];
        assert_eq!(next_to_ask(&edited, &discarded), Some(b));
    }
}
