//! バックグラウンド実行と、UI スレッドへ結果を戻す仕組み。
//!
//! スレッドモデル（詳細は `docs/architecture.md`）:
//!
//! 1. UI スレッドが処理を始めるとき、対象の [`DocStamp`](super::ids::DocStamp) と
//!    入力（必要な範囲の本文の写しなど）を用意し、[`WorkerPool::spawn`] に渡す。
//! 2. ワーカースレッドは [`TaskContext`] で中止・進捗・上限を扱い、結果を
//!    [`UiSender::send`] で送る。送信は [`UiWaker::wake`] を呼び、UI スレッドを起こす
//!    （Windows では `PostMessageW` でウィンドウメッセージを 1 つ投げる実装を `ui` が持つ）。
//! 3. UI スレッドは起こされたら [`UiReceiver::drain`] で結果を取り出し、
//!    **文書 ID と版を照合してから**反映する。食い違えば捨てる。
//!
//! このモジュールは OS に依存しない。スレッドは最初の処理まで 1 本も作らない。

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Duration;

use super::cancel::CancelToken;
use super::limits::Limits;
use super::progress::{NoProgress, Progress, ProgressSink};

/// バックグラウンド処理に渡す、中止・進捗・上限の一式。
///
/// 長時間かかりうる中核の関数（検索、Grep、索引作成、保存、変換）は
/// 引数に `&TaskContext` を取る。
#[derive(Clone)]
pub struct TaskContext {
    pub cancel: CancelToken,
    pub progress: Arc<dyn ProgressSink>,
    pub limits: Limits,
}

impl TaskContext {
    pub fn new(cancel: CancelToken, progress: Arc<dyn ProgressSink>, limits: Limits) -> Self {
        Self {
            cancel,
            progress,
            limits,
        }
    }

    /// 中止されず、進捗を捨て、上限のない一式。テストや短い同期処理向け。
    pub fn detached() -> Self {
        Self::new(CancelToken::new(), Arc::new(NoProgress), Limits::UNLIMITED)
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// 進捗を報告する。
    pub fn report(&self, done: u64, total: Option<u64>) {
        self.progress.report(Progress::new(done, total));
    }
}

impl std::fmt::Debug for TaskContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskContext")
            .field("cancel", &self.cancel)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// UI スレッドを起こす手段。`ui` が `PostMessageW` で実装する。
///
/// どのスレッドから呼ばれてもよく、ブロックしてはならない。続けて何度呼ばれても、
/// UI 側は 1 回の [`UiReceiver::drain`] でまとめて処理できる。
pub trait UiWaker: Send + Sync {
    fn wake(&self);
}

impl<F: Fn() + Send + Sync> UiWaker for F {
    fn wake(&self) {
        self()
    }
}

/// UI スレッド宛ての通路を作る。`M` はアプリが定義するメッセージ型
/// （「保存が終わった」「検索結果が届いた」など。文書に関するものは版を含める）。
pub fn ui_channel<M: Send>(waker: Arc<dyn UiWaker>) -> (UiSender<M>, UiReceiver<M>) {
    let (tx, rx) = mpsc::channel();
    (UiSender { tx, waker }, UiReceiver { rx })
}

/// バックグラウンド側の送信口。複製して各処理に渡す。
pub struct UiSender<M> {
    tx: mpsc::Sender<M>,
    waker: Arc<dyn UiWaker>,
}

impl<M> Clone for UiSender<M> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            waker: Arc::clone(&self.waker),
        }
    }
}

impl<M: Send> UiSender<M> {
    /// メッセージを送り、UI スレッドを起こす。UI が既に終了していれば `false`
    /// （処理側はそのまま静かに終わればよい）。
    pub fn send(&self, message: M) -> bool {
        if self.tx.send(message).is_err() {
            return false;
        }
        self.waker.wake();
        true
    }
}

/// UI スレッド側の受信口。
pub struct UiReceiver<M> {
    rx: mpsc::Receiver<M>,
}

impl<M> UiReceiver<M> {
    /// 届いているメッセージを、待たずにすべて取り出す。
    pub fn drain(&self) -> impl Iterator<Item = M> + '_ {
        self.rx.try_iter()
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// 小さなワーカースレッドの集まり。
///
/// - スレッドは必要になるまで作らない（起動時間と常駐メモリのため）。
/// - 同時に動くのは `max_threads` 本まで。それ以上の処理は順番を待つ。
/// - 一定時間仕事がないスレッドは終了する。
///
/// 待ち時間の長い専用スレッド（将来の MCP の待ち受けなど）には使わない。
pub struct WorkerPool {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wakeup: Condvar,
    max_threads: usize,
    idle_timeout: Duration,
}

struct State {
    jobs: VecDeque<Job>,
    threads: usize,
    idle: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // 処理はロックの外で実行するので、ロックを持ったままパニックすることはない。
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl WorkerPool {
    /// 既定の同時実行数の上限。
    pub const DEFAULT_MAX_THREADS: usize = 4;
    const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(max_threads: usize) -> Self {
        Self::with_idle_timeout(max_threads, Self::IDLE_TIMEOUT)
    }

    fn with_idle_timeout(max_threads: usize, idle_timeout: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    jobs: VecDeque::new(),
                    threads: 0,
                    idle: 0,
                }),
                wakeup: Condvar::new(),
                max_threads: max_threads.max(1),
                idle_timeout,
            }),
        }
    }

    /// プロセス全体で共有する集まり。最初に呼ばれたときに作る（スレッドはまだ作らない）。
    pub fn global() -> &'static WorkerPool {
        static POOL: OnceLock<WorkerPool> = OnceLock::new();
        POOL.get_or_init(|| {
            let cpus = thread::available_parallelism().map_or(2, |n| n.get());
            WorkerPool::new(cpus.clamp(2, Self::DEFAULT_MAX_THREADS))
        })
    }

    /// 処理をバックグラウンドで実行する。すぐ戻る。
    ///
    /// 処理の中で結果を [`UiSender`] で送ること。処理は中止に応じること
    /// （[`TaskContext::cancel`]）。
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        let mut state = self.shared.lock();
        state.jobs.push_back(Box::new(job));
        if state.jobs.len() > state.idle && state.threads < self.shared.max_threads {
            let shared = Arc::clone(&self.shared);
            let spawned = thread::Builder::new()
                .name("linote-worker".into())
                .spawn(move || worker_loop(shared));
            // スレッドを作れなくても、既存のスレッドがあればいずれ処理される。
            if spawned.is_ok() {
                state.threads += 1;
            }
        }
        drop(state);
        self.shared.wakeup.notify_one();
    }
}

/// 処理がパニックしてスレッドが終わるときに本数を戻す。
/// 通常の終了では、ロックを持ったまま本数を戻してから `disarmed` にする
/// （ロックを離してから戻すと、その隙間に積まれた処理が誰にも拾われない）。
struct ThreadSlot {
    shared: Arc<Shared>,
    disarmed: bool,
}

impl Drop for ThreadSlot {
    fn drop(&mut self) {
        if !self.disarmed {
            self.shared.lock().threads -= 1;
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    let mut slot = ThreadSlot {
        shared: Arc::clone(&shared),
        disarmed: false,
    };
    let mut state = shared.lock();
    loop {
        if let Some(job) = state.jobs.pop_front() {
            drop(state);
            job();
            state = shared.lock();
            continue;
        }
        state.idle += 1;
        let (guard, timeout) = shared
            .wakeup
            .wait_timeout(state, shared.idle_timeout)
            .unwrap_or_else(|e| e.into_inner());
        state = guard;
        state.idle -= 1;
        if timeout.timed_out() && state.jobs.is_empty() {
            state.threads -= 1;
            slot.disarmed = true;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn results_reach_the_ui_side_and_wake_it() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&wakes);
        let waker: Arc<dyn UiWaker> = Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let (tx, rx) = ui_channel::<u32>(waker);
        let (done_tx, done_rx) = mpsc::channel();
        let pool = WorkerPool::new(2);
        for i in 0..5 {
            let tx = tx.clone();
            let done_tx = done_tx.clone();
            pool.spawn(move || {
                assert!(tx.send(i));
                done_tx.send(()).unwrap();
            });
        }
        for _ in 0..5 {
            done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        let mut got: Vec<u32> = rx.drain().collect();
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
        assert_eq!(wakes.load(Ordering::SeqCst), 5);
        assert_eq!(rx.drain().count(), 0);
    }

    #[test]
    fn send_reports_a_closed_ui() {
        let (tx, rx) = ui_channel::<u32>(Arc::new(|| {}));
        drop(rx);
        assert!(!tx.send(1));
    }

    #[test]
    fn pool_never_exceeds_its_thread_limit() {
        let pool = WorkerPool::new(2);
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (done_tx, done_rx) = mpsc::channel();
        for _ in 0..8 {
            let (running, peak, done_tx) = (running.clone(), peak.clone(), done_tx.clone());
            pool.spawn(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(20));
                running.fetch_sub(1, Ordering::SeqCst);
                done_tx.send(()).unwrap();
            });
        }
        for _ in 0..8 {
            done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert!(pool.shared.lock().threads <= 2);
    }

    #[test]
    fn idle_threads_exit_and_pool_restarts_on_demand() {
        let pool = WorkerPool::with_idle_timeout(1, Duration::from_millis(20));
        let (done_tx, done_rx) = mpsc::channel();
        let tx = done_tx.clone();
        pool.spawn(move || tx.send(1).unwrap());
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while pool.shared.lock().threads != 0 {
            assert!(std::time::Instant::now() < deadline, "thread did not exit");
            thread::sleep(Duration::from_millis(5));
        }
        pool.spawn(move || done_tx.send(2).unwrap());
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap(), 2);
    }

    #[test]
    fn task_context_carries_cancel_progress_and_limits() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        let ctx = TaskContext::new(
            CancelToken::new(),
            Arc::new(move |p: Progress| s.lock().unwrap().push(p)),
            Limits::items(3),
        );
        let clone = ctx.clone();
        clone.report(1, Some(2));
        ctx.cancel.cancel();
        assert!(clone.cancel.is_cancelled());
        assert_eq!(*seen.lock().unwrap(), vec![Progress::new(1, Some(2))]);
        assert_eq!(clone.limits.max_items, Some(3));
        assert!(TaskContext::detached().cancel.check().is_ok());
    }
}
