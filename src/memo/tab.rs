//! メモをタブで開いているときの保存の状態（GUI 非依存）。
//!
//! タブ（[`crate::document::DocumentState::memo`]）ごとに 1 つ持つ。保存領域の版
//! （[`Memo::version`]）と、文書の版（[`crate::document::DocumentState::version`]）は
//! 別物なので、保存領域の版はここで控える。
//!
//! 使い方（`ui::app` 側）:
//! 1. 開くとき: `MemoTab::opened(&memo)`。本文は `memo.body` をエディタに入れる。
//! 2. 入力が落ち着いたら: `tab.begin_save()` が `Some((id, expected))` なら、
//!    文書の版 `v` と本文を控えて `store.save_in_background(id, expected, body, …)`。
//!    `None` は保存中なので、終了時に再保存される（`finish` の戻り値）。
//! 3. 結果が UI に届いたら: 文書が開いたままなら `tab.finish(&outcome)`。
//!    `Saved` なら `doc.record_saved(v, None, None)`。`forked` が `Some` なら、
//!    タブは新しいメモ（`forked.id`、名前 `forked.name`）に切り替わっている。
//!    `resave` が真なら、もう一度 2. を行う。失敗なら `tab.finish_failed()`。

use crate::core::{MemoId, Version};

use super::{Memo, SaveOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoTab {
    id: MemoId,
    original: Option<MemoId>,
    store_version: Version,
    in_flight: bool,
    pending: bool,
}

/// [`MemoTab::finish`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// 保存中にさらに入力があった。もう一度保存を始める。
    pub resave: bool,
    /// 衝突して別のメモとして残した。タブはこのメモに切り替わった。
    pub forked: Option<Memo>,
}

impl MemoTab {
    pub fn opened(memo: &Memo) -> Self {
        Self {
            id: memo.id.clone(),
            original: None,
            store_version: memo.version,
            in_flight: false,
            pending: false,
        }
    }

    pub fn id(&self) -> &MemoId {
        &self.id
    }

    /// 競合でこのタブが別のメモに切り替わる直前の保存先。
    pub fn original_id(&self) -> Option<&MemoId> {
        self.original.as_ref()
    }

    pub fn store_version(&self) -> Version {
        self.store_version
    }

    /// 保存を始めてよければ、保存先と前提の版を返す。保存中なら `None`（後で再保存）。
    pub fn begin_save(&mut self) -> Option<(MemoId, Version)> {
        if self.in_flight {
            self.pending = true;
            return None;
        }
        self.in_flight = true;
        Some((self.id.clone(), self.store_version))
    }

    /// 保存が成功して終わった。
    pub fn finish(&mut self, outcome: &SaveOutcome) -> Finished {
        self.in_flight = false;
        let forked = match outcome {
            SaveOutcome::Saved(memo) => {
                self.store_version = memo.version;
                None
            }
            SaveOutcome::Forked { memo, .. } => {
                self.original = Some(std::mem::replace(&mut self.id, memo.id.clone()));
                self.store_version = memo.version;
                Some(memo.clone())
            }
        };
        Finished {
            resave: std::mem::take(&mut self.pending),
            forked,
        }
    }

    /// 保存が失敗した。次の入力でまた試す（再試行の要否を返す）。
    pub fn finish_failed(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.pending)
    }

    /// 名前を変えた・別の経路で更新したあとの版を取り込む。
    pub fn adopt(&mut self, memo: &Memo) {
        if memo.id == self.id {
            self.store_version = memo.version;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MemoId;

    fn memo(version: u64) -> Memo {
        Memo {
            id: MemoId::generate(),
            name: "n".into(),
            body: String::new(),
            created_ms: 0,
            updated_ms: 0,
            version: Version::from_raw(version),
        }
    }

    #[test]
    fn serializes_saves_and_resaves() {
        let m = memo(3);
        let mut tab = MemoTab::opened(&m);
        assert_eq!(tab.begin_save(), Some((m.id.clone(), Version::from_raw(3))));
        assert_eq!(tab.begin_save(), None);
        let saved = Memo {
            version: Version::from_raw(4),
            ..m.clone()
        };
        let f = tab.finish(&SaveOutcome::Saved(saved));
        assert!(f.resave && f.forked.is_none());
        assert_eq!(tab.begin_save(), Some((m.id, Version::from_raw(4))));
    }

    #[test]
    fn fork_switches_the_tab() {
        let m = memo(1);
        let mut tab = MemoTab::opened(&m);
        tab.begin_save();
        let other = memo(0);
        let f = tab.finish(&SaveOutcome::Forked {
            memo: other.clone(),
            original_actual: Version::from_raw(5),
        });
        assert_eq!(f.forked.as_ref(), Some(&other));
        assert_eq!(tab.id(), &other.id);
        assert_eq!(tab.original_id(), Some(&m.id));
        assert!(!f.resave);
    }

    #[test]
    fn failure_allows_retry() {
        let mut tab = MemoTab::opened(&memo(0));
        tab.begin_save();
        assert!(!tab.finish_failed());
        assert!(tab.begin_save().is_some());
    }
}
