//! 書き出しの間隔の方針。

use std::time::{Duration, Instant};

/// いつ復元データを書き出すか。値は設定で変えられるようにする（設定項目への
/// 追加は統括の判断。既定値はここの [`Default`]）。
///
/// 方針: 最後の編集から `idle` 経ったら書く（入力が落ち着いた時点）。ただし
/// 書き出されていない編集が `max_interval` を超えて溜まらないよう、入力が続いても
/// 最初の未書き出しの編集から `max_interval` で書く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPolicy {
    pub idle: Duration,
    pub max_interval: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(2),
            max_interval: Duration::from_secs(30),
        }
    }
}

impl RecoveryPolicy {
    /// 次に書き出す時刻。`first_unsaved_edit` は、前回の書き出し以降で最初の編集、
    /// `last_edit` は最後の編集の時刻。UI のタイマーがこの時刻に書き出しを起動する。
    pub fn next_due(&self, first_unsaved_edit: Instant, last_edit: Instant) -> Instant {
        (last_edit + self.idle).min(first_unsaved_edit + self.max_interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_then_cap() {
        let p = RecoveryPolicy::default();
        let t0 = Instant::now();
        // 1 回だけ編集: idle 後。
        assert_eq!(p.next_due(t0, t0), t0 + p.idle);
        // 入力が続いて last_edit が進んでも、上限を超えない。
        let late = t0 + Duration::from_secs(29);
        assert_eq!(p.next_due(t0, late), t0 + p.max_interval);
    }
}
