//! 中止の共通の仕組み。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::error::{Error, Result};

/// 中止の合図。複製はすべて同じ合図を共有する。
///
/// 処理を始める側が作って複製を渡し、中止したいときに [`cancel`](Self::cancel) を呼ぶ。
/// 処理する側はループの区切り（数十ミリ秒以内に一度が目安。巨大な 1 行の途中でも
/// 一定バイトごと）で [`check`](Self::check) を呼ぶ。
///
/// 文書を閉じたとき・版が進んで結果が不要になったときも、UI 側が中止を合図する。
/// ただし合図は間に合わないことがあるので、結果を反映する前の版の照合は省略しない。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 中止を合図する。何度呼んでもよい。取り消せない。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// 中止されていれば [`Error::Cancelled`] を返す。`?` で抜けるために使う。
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_signal() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(b.check().is_ok());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(matches!(b.check(), Err(Error::Cancelled)));
    }
}
