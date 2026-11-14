//! 件数・バイト数の上限と、途中で終わった結果の表現。

/// 結果の上限。`None` は無制限。
///
/// 検索・Grep・範囲読み取りなど、結果が際限なく増えうる処理は必ず受け取る。
/// 既定値は各モジュールが実測で決め、定数として公開する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    /// 結果の件数の上限（ヒット数、ファイル数など）。
    pub max_items: Option<u64>,
    /// 結果として保持・返却するバイト数の上限。
    pub max_bytes: Option<u64>,
}

impl Limits {
    pub const UNLIMITED: Limits = Limits {
        max_items: None,
        max_bytes: None,
    };

    pub const fn items(max_items: u64) -> Self {
        Limits {
            max_items: Some(max_items),
            max_bytes: None,
        }
    }
}

/// 処理がどう終わったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// 最後まで終わった。
    Complete,
    /// 件数の上限で打ち切った。結果は上限までの分。
    ItemLimit,
    /// バイト数の上限で打ち切った。
    ByteLimit,
    /// 中止された。結果は中止までの分。
    Cancelled,
}

impl Completion {
    pub fn is_complete(self) -> bool {
        self == Completion::Complete
    }

    /// 機械可読な安定した名前。
    pub const fn as_str(self) -> &'static str {
        match self {
            Completion::Complete => "complete",
            Completion::ItemLimit => "item_limit",
            Completion::ByteLimit => "byte_limit",
            Completion::Cancelled => "cancelled",
        }
    }
}

/// 途中で終わっても意味のある結果（検索のヒット一覧など）。
///
/// 上限や中止で打ち切った場合もエラーにせず、そこまでの結果と終わり方を返す。
/// 画面には「上限に達した」「中止した」ことを必ず表示する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome<T> {
    pub value: T,
    pub completion: Completion,
}

impl<T> Outcome<T> {
    pub fn complete(value: T) -> Self {
        Self {
            value,
            completion: Completion::Complete,
        }
    }

    pub fn new(value: T, completion: Completion) -> Self {
        Self { value, completion }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        Outcome {
            value: f(self.value),
            completion: self.completion,
        }
    }
}

/// 上限に対する消費量の集計。結果を 1 件足すたびに [`try_add`](Self::try_add) を呼ぶ。
#[derive(Debug, Clone)]
pub struct Budget {
    limits: Limits,
    items: u64,
    bytes: u64,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            items: 0,
            bytes: 0,
        }
    }

    /// `bytes` バイトの結果を 1 件足せるか確かめ、足せれば計上する。
    /// 足せなければ何も計上せず、打ち切りの理由を返す。
    pub fn try_add(&mut self, bytes: u64) -> std::result::Result<(), Completion> {
        if self.limits.max_items.is_some_and(|max| self.items >= max) {
            return Err(Completion::ItemLimit);
        }
        let new_bytes = self.bytes.saturating_add(bytes);
        if self.limits.max_bytes.is_some_and(|max| new_bytes > max) {
            return Err(Completion::ByteLimit);
        }
        self.items += 1;
        self.bytes = new_bytes;
        Ok(())
    }

    pub fn items(&self) -> u64 {
        self.items
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_stops_at_item_limit() {
        let mut b = Budget::new(Limits::items(2));
        assert!(b.try_add(10).is_ok());
        assert!(b.try_add(10).is_ok());
        assert_eq!(b.try_add(10), Err(Completion::ItemLimit));
        assert_eq!((b.items(), b.bytes()), (2, 20));
    }

    #[test]
    fn budget_stops_at_byte_limit_without_charging() {
        let mut b = Budget::new(Limits {
            max_items: None,
            max_bytes: Some(15),
        });
        assert!(b.try_add(10).is_ok());
        assert_eq!(b.try_add(6), Err(Completion::ByteLimit));
        assert!(b.try_add(5).is_ok());
        assert_eq!(b.bytes(), 15);
        assert!(Budget::new(Limits::UNLIMITED).try_add(u64::MAX).is_ok());
    }

    #[test]
    fn outcome_keeps_completion_through_map() {
        let o = Outcome::new(vec![1, 2], Completion::Cancelled).map(|v| v.len());
        assert_eq!(o.value, 2);
        assert!(!o.completion.is_complete());
        assert_eq!(o.completion.as_str(), "cancelled");
    }
}
