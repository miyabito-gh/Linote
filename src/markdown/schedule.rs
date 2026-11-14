//! 変換の予定（入力が落ち着くまで待つ）と、古い結果の破棄。
//!
//! [`PreviewScheduler`] は時刻を引数で受け取る純粋な状態機械で、スレッドもタイマーも
//! 持たない。UI 側は次のように使う（待ち時間は `SetTimer` で管理する）。
//!
//! 1. 編集のたびに [`PreviewScheduler::note_change`]。変換中の古い版は中止される。
//!    プレビューを開いたときやタブを切り替えたときは [`PreviewScheduler::request_now`]。
//! 2. [`PreviewScheduler::next_due`] までの時間でタイマーを掛け直す。
//! 3. タイマーが来たら [`PreviewScheduler::take_due`]。券（[`RenderTicket`]）が返れば、
//!    本文を写して [`spawn_render`] でワーカーに渡す。
//! 4. 結果のメッセージを受けたら [`PreviewScheduler::accept`] に現在の版を渡す。
//!    `Some` が返ったときだけ表示する（右ペインと別ウィンドウの両方に同じ結果を使う）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::render::render_body;
use crate::core::{
    CancelToken, DocStamp, DocumentId, Error, Limits, UiSender, Version, WorkerPool,
};

/// ワーカーから UI へ送る変換の結果（`<body>` の中身。複製は安い）。
/// `ui::app` のメッセージ型に包んで送る。
#[derive(Debug)]
pub struct PreviewUpdate {
    pub stamp: DocStamp,
    pub result: Result<Arc<str>, Error>,
}

/// 変換を始めてよいという券。
#[derive(Debug, Clone)]
pub struct RenderTicket {
    pub stamp: DocStamp,
    pub cancel: CancelToken,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    stamp: DocStamp,
    due: Instant,
}

/// 変換の予定と照合の状態。プレビュー対象の文書 1 つにつき 1 つ（右ペインと
/// 別ウィンドウは同じものを共有する）。
#[derive(Debug)]
pub struct PreviewScheduler {
    base_delay: Duration,
    /// 対象の文書。別の文書の結果は捨てる。
    target: Option<DocumentId>,
    pending: Option<Pending>,
    running: Option<RenderTicket>,
    /// 最後に受け入れた結果の印。
    shown: Option<DocStamp>,
}

impl Default for PreviewScheduler {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DELAY)
    }
}

impl PreviewScheduler {
    /// 入力が止まってから変換を始めるまでの既定の待ち時間。
    pub const DEFAULT_DELAY: Duration = Duration::from_millis(250);
    /// 大きな文書で待ち時間を延ばす上限。
    pub const MAX_DELAY: Duration = Duration::from_millis(1500);

    pub fn new(base_delay: Duration) -> Self {
        Self {
            base_delay,
            target: None,
            pending: None,
            running: None,
            shown: None,
        }
    }

    /// 文書の大きさに応じた待ち時間。大きな文書では入力中に何度も変換しないよう、
    /// 1 MiB ごとに 100 ミリ秒延ばす（[`Self::MAX_DELAY`] まで）。
    pub fn delay_for(&self, text_len: usize) -> Duration {
        let extra = Duration::from_millis(100).saturating_mul((text_len >> 20) as u32);
        (self.base_delay + extra).min(Self::MAX_DELAY.max(self.base_delay))
    }

    /// 本文が変わった。待ち時間の後に変換する。変換中の古い版は中止する。
    pub fn note_change(&mut self, stamp: DocStamp, text_len: usize, now: Instant) {
        let due = now + self.delay_for(text_len);
        self.schedule(stamp, due);
    }

    /// すぐに変換する（プレビューを開いた、対象の文書を切り替えた、テーマが変わった等）。
    pub fn request_now(&mut self, stamp: DocStamp, now: Instant) {
        self.schedule(stamp, now);
    }

    fn schedule(&mut self, stamp: DocStamp, due: Instant) {
        if self.target != Some(stamp.document) {
            self.shown = None;
        }
        self.target = Some(stamp.document);
        if let Some(running) = &self.running {
            if running.stamp != stamp {
                running.cancel.cancel();
                self.running = None;
            }
        }
        self.pending = Some(Pending { stamp, due });
    }

    /// 次に [`take_due`](Self::take_due) を呼ぶべき時刻。予定がなければ `None`。
    pub fn next_due(&self) -> Option<Instant> {
        self.pending.map(|p| p.due)
    }

    /// 待ち時間が過ぎていれば、変換を始める券を返す。同じ版を変換中か表示済みなら
    /// 何もしない。
    pub fn take_due(&mut self, now: Instant) -> Option<RenderTicket> {
        let pending = self.pending?;
        if now < pending.due {
            return None;
        }
        self.pending = None;
        let same_running = self.running.as_ref().map(|r| r.stamp) == Some(pending.stamp);
        if same_running || self.shown == Some(pending.stamp) {
            return None;
        }
        let ticket = RenderTicket {
            stamp: pending.stamp,
            cancel: CancelToken::new(),
        };
        self.running = Some(ticket.clone());
        Some(ticket)
    }

    /// 結果を照合する。`current` は結果の文書の現在の版（閉じられていれば `None`）。
    ///
    /// 対象の文書で、版が現在のものと一致するときだけ `Some` を返す。古い版、
    /// 閉じた文書、対象でない文書の結果、中止による失敗は捨てる。
    pub fn accept(
        &mut self,
        update: PreviewUpdate,
        current: Option<Version>,
    ) -> Option<PreviewUpdate> {
        if self.running.as_ref().map(|r| r.stamp) == Some(update.stamp) {
            self.running = None;
        }
        if self.target != Some(update.stamp.document) || !update.stamp.is_current(current) {
            return None;
        }
        if matches!(&update.result, Err(e) if e.is_cancelled()) {
            return None;
        }
        self.shown = Some(update.stamp);
        Some(update)
    }

    /// 予定と変換中の処理をすべて取りやめる（プレビューを閉じた、文書を閉じた）。
    pub fn cancel_all(&mut self) {
        if let Some(running) = self.running.take() {
            running.cancel.cancel();
        }
        self.pending = None;
        self.target = None;
        self.shown = None;
    }

    /// 表示を作り直す必要がある（テーマの変更など）とき、表示済みの印を忘れる。
    /// 本文の変換結果は配色に依存しないので、通常は再変換せずに
    /// [`compose_document`](super::compose_document) だけやり直せばよい。
    pub fn invalidate_shown(&mut self) {
        self.shown = None;
    }

    /// 最後に受け入れた結果の印。
    pub fn shown(&self) -> Option<DocStamp> {
        self.shown
    }

    /// 変換中か。
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

/// 本文の写しをワーカーで変換し、結果を `wrap` で UI のメッセージに包んで送る。
///
/// 中止された場合は何も送らない。
pub fn spawn_render<M, F>(
    pool: &WorkerPool,
    ticket: RenderTicket,
    text: String,
    limits: Limits,
    sender: UiSender<M>,
    wrap: F,
) where
    M: Send + 'static,
    F: FnOnce(PreviewUpdate) -> M + Send + 'static,
{
    pool.spawn(move || {
        if ticket.cancel.is_cancelled() {
            return;
        }
        let result = render_body(&text, &ticket.cancel, limits).map(Arc::<str>::from);
        drop(text);
        if matches!(&result, Err(e) if e.is_cancelled()) {
            return;
        }
        sender.send(wrap(PreviewUpdate {
            stamp: ticket.stamp,
            result,
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ui_channel;
    use std::sync::mpsc;

    fn stamp(doc: u64, ver: u64) -> DocStamp {
        DocStamp::new(DocumentId::from_raw(doc), Version::from_raw(ver))
    }

    fn ok(stamp: DocStamp) -> PreviewUpdate {
        PreviewUpdate {
            stamp,
            result: Ok("<p>x</p>".into()),
        }
    }

    #[test]
    fn waits_until_input_settles() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::from_millis(100));
        s.note_change(stamp(1, 1), 10, t0);
        s.note_change(stamp(1, 2), 10, t0 + Duration::from_millis(50));
        assert_eq!(s.next_due(), Some(t0 + Duration::from_millis(150)));
        assert!(s.take_due(t0 + Duration::from_millis(120)).is_none());
        let ticket = s.take_due(t0 + Duration::from_millis(150)).unwrap();
        assert_eq!(ticket.stamp, stamp(1, 2));
        assert!(s.next_due().is_none());
        assert!(s.take_due(t0 + Duration::from_secs(9)).is_none());
    }

    #[test]
    fn delay_grows_with_document_size() {
        let s = PreviewScheduler::new(Duration::from_millis(250));
        assert_eq!(s.delay_for(1000), Duration::from_millis(250));
        assert_eq!(s.delay_for(3 << 20), Duration::from_millis(550));
        assert_eq!(s.delay_for(1 << 30), PreviewScheduler::MAX_DELAY);
    }

    #[test]
    fn new_version_cancels_running_render() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);
        s.request_now(stamp(1, 1), t0);
        let ticket = s.take_due(t0).unwrap();
        assert!(s.is_running());
        s.note_change(stamp(1, 2), 0, t0);
        assert!(ticket.cancel.is_cancelled());
        assert!(!s.is_running());
    }

    #[test]
    fn stale_results_are_discarded() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);
        s.request_now(stamp(1, 1), t0);
        s.take_due(t0).unwrap();
        s.note_change(stamp(1, 2), 0, t0);
        s.take_due(t0).unwrap();
        // 版 1 の結果が後から届いても捨てる。
        assert!(s
            .accept(ok(stamp(1, 1)), Some(Version::from_raw(2)))
            .is_none());
        assert!(s.is_running());
        // 版 2 の結果を受ける前に版 3 に進んでいたら、版 2 も捨てる。
        assert!(s
            .accept(ok(stamp(1, 2)), Some(Version::from_raw(3)))
            .is_none());
        assert_eq!(s.shown(), None);
    }

    #[test]
    fn current_result_is_accepted_once_per_version() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);
        s.request_now(stamp(1, 5), t0);
        s.take_due(t0).unwrap();
        let got = s
            .accept(ok(stamp(1, 5)), Some(Version::from_raw(5)))
            .unwrap();
        assert_eq!(&*got.result.unwrap(), "<p>x</p>");
        assert_eq!(s.shown(), Some(stamp(1, 5)));
        assert!(!s.is_running());
        // 同じ版をもう一度求めても変換しない。テーマ変更などで忘れさせれば変換する。
        s.request_now(stamp(1, 5), t0);
        assert!(s.take_due(t0).is_none());
        s.invalidate_shown();
        s.request_now(stamp(1, 5), t0);
        assert!(s.take_due(t0).is_some());
    }

    #[test]
    fn closed_or_other_documents_are_discarded() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);
        s.request_now(stamp(1, 1), t0);
        s.take_due(t0).unwrap();
        // 閉じられた文書。
        assert!(s.accept(ok(stamp(1, 1)), None).is_none());
        // 対象を文書 2 に切り替えた後に届いた文書 1 の結果。
        s.request_now(stamp(2, 1), t0);
        assert!(s
            .accept(ok(stamp(1, 1)), Some(Version::from_raw(1)))
            .is_none());
        // 取りやめた後は何も受けない。
        s.cancel_all();
        assert!(s
            .accept(ok(stamp(2, 1)), Some(Version::from_raw(1)))
            .is_none());
        assert!(s.next_due().is_none());
    }

    #[test]
    fn errors_are_reported_but_cancellation_is_not() {
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);
        s.request_now(stamp(1, 1), t0);
        s.take_due(t0).unwrap();
        let cancelled = PreviewUpdate {
            stamp: stamp(1, 1),
            result: Err(Error::Cancelled),
        };
        assert!(s.accept(cancelled, Some(Version::from_raw(1))).is_none());
        let too_big = PreviewUpdate {
            stamp: stamp(1, 1),
            result: Err(Error::LimitExceeded {
                what: "x".into(),
                limit: 1,
            }),
        };
        assert!(s
            .accept(too_big, Some(Version::from_raw(1)))
            .unwrap()
            .result
            .is_err());
    }

    #[test]
    fn renders_in_the_background_and_reaches_the_ui() {
        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let wake_tx = std::sync::Mutex::new(wake_tx);
        let (tx, rx) = ui_channel::<PreviewUpdate>(Arc::new(move || {
            let _ = wake_tx.lock().unwrap().send(());
        }));
        let pool = WorkerPool::new(1);
        let t0 = Instant::now();
        let mut s = PreviewScheduler::new(Duration::ZERO);

        // 中止済みの券は何も送らない。
        s.request_now(stamp(7, 1), t0);
        let first = s.take_due(t0).unwrap();
        s.note_change(stamp(7, 2), 0, t0);
        spawn_render(
            &pool,
            first,
            "# old".into(),
            Limits::UNLIMITED,
            tx.clone(),
            |u| u,
        );

        let ticket = s.take_due(t0).unwrap();
        spawn_render(
            &pool,
            ticket,
            "# 新しい\n\n<b>x</b>".into(),
            Limits::UNLIMITED,
            tx,
            |u| u,
        );
        wake_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let updates: Vec<_> = rx.drain().collect();
        assert_eq!(updates.len(), 1);
        let update = updates.into_iter().next().unwrap();
        assert_eq!(update.stamp, stamp(7, 2));
        let got = s.accept(update, Some(Version::from_raw(2))).unwrap();
        let body = got.result.unwrap();
        assert!(body.contains("<h1>新しい</h1>"));
        assert!(body.contains("&lt;b&gt;x&lt;/b&gt;"));
    }
}
