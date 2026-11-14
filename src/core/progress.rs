//! 進捗通知の共通の仕組み。

/// 進捗。単位（バイト、ファイル数など）は処理ごとに決めて文書化する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// 済んだ量。
    pub done: u64,
    /// 全体の量。分からなければ `None`（Grep のファイル数など）。
    pub total: Option<u64>,
}

impl Progress {
    pub const fn new(done: u64, total: Option<u64>) -> Self {
        Self { done, total }
    }

    /// 0〜100 の割合。全体が分からなければ `None`。
    pub fn percent(&self) -> Option<u8> {
        let total = self.total?;
        if total == 0 {
            return Some(100);
        }
        Some((self.done.min(total) as u128 * 100 / total as u128) as u8)
    }
}

/// 進捗の受け取り口。バックグラウンドのスレッドから呼ばれる。
///
/// 実装は軽くすること（UI への通知は間引く）。報告する側も、1 件ごとではなく
/// まとまった単位（例: 1 MiB ごと、ファイル 1 個ごと）で呼ぶ。
pub trait ProgressSink: Send + Sync {
    fn report(&self, progress: Progress);
}

/// 進捗を捨てる受け取り口。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _progress: Progress) {}
}

impl<F: Fn(Progress) + Send + Sync> ProgressSink for F {
    fn report(&self, progress: Progress) {
        self(progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn percent_handles_unknown_zero_and_overshoot() {
        assert_eq!(Progress::new(5, None).percent(), None);
        assert_eq!(Progress::new(0, Some(0)).percent(), Some(100));
        assert_eq!(Progress::new(1, Some(4)).percent(), Some(25));
        assert_eq!(Progress::new(9, Some(4)).percent(), Some(100));
        assert_eq!(Progress::new(u64::MAX, Some(u64::MAX)).percent(), Some(100));
    }

    #[test]
    fn closures_are_sinks() {
        let seen = Mutex::new(Vec::new());
        let sink = |p: Progress| seen.lock().unwrap().push(p.done);
        let dynamic: &dyn ProgressSink = &sink;
        dynamic.report(Progress::new(1, None));
        dynamic.report(Progress::new(2, None));
        assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    }
}
