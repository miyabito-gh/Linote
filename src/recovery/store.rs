//! 復元データの保存先の操作。所有・排他・孤児の検出・隔離。
//!
//! 保存先 `recovery/` の中（形式は `docs/architecture.md` §7）:
//! - `<id>.rec`: 復元データ本体（[`super::format`]）。原子的に置換する。
//! - `<id>.lock`: 所有を示すロックファイル。所有プロセスが [`RecoverySlot`] を持つ間
//!   ずっと排他ロックを保持する。プロセスが落ちれば OS が解放する。
//! - `quarantine/`: 壊れた・未知の版のデータの隔離先。
//!
//! `<id>` はプロセス ID・時刻・連番から作るので、別のプロセスと衝突しない。
//! 各プロセスは自分の `<id>` だけを書くので、同じ文書を複数ウィンドウで開いても
//! 互いに上書きしない。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::appdata::AppPaths;
use crate::core::{Error, Result, TaskContext, Version};
use crate::file::{classify, is_temp_file_name, write_atomic, IoFailure, LockFile};

use super::format::{self, DecodeError, Meta, RecoveryId, Snapshot};

const DATA_EXT: &str = "rec";
const LOCK_EXT: &str = "lock";
const QUARANTINE_DIR: &str = "quarantine";
/// これより古い一時ファイル・データのないロックファイルは、強制終了の残りとして消す。
const STALE_AGE: Duration = Duration::from_secs(3600);

/// 保存先への窓口。軽い値で、複製してよい。
#[derive(Debug, Clone)]
pub struct RecoveryStore {
    paths: AppPaths,
    dir: PathBuf,
}

impl RecoveryStore {
    /// 作るだけでは何もしない（ディレクトリは最初の書き出しで作る）。
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            dir: paths.recovery_dir(),
            paths: paths.clone(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 1 つの文書のための復元データの枠を作り、所有のロックを取る。
    /// 文書（タブ）ごとに 1 つ持ち、文書を閉じるまで保持する。
    /// ファイルは最初の [`RecoverySlot::write`] まで作らない。
    pub fn open_slot(&self) -> Result<RecoverySlot> {
        self.paths.ensure_dir(&self.dir)?;
        let id = RecoveryId::generate();
        let lock = LockFile::try_acquire(&lock_path(&self.dir, &id))?;
        Ok(RecoverySlot::new(
            self.paths.clone(),
            self.dir.clone(),
            id,
            lock,
            None,
        ))
    }

    /// 起動時の走査。所有者のいない（ロックを取れる）復元データを [`Candidate`] として
    /// 引き取る。引き取った間、他のプロセスはそれを復元・削除できない。生きている
    /// プロセスが所有するものは触らず [`ScanReport::live`] に載せる。
    /// 壊れたもの・未知の版のものは、引き取れた場合に限り隔離する。
    ///
    /// 別プロセス（将来の MCP）が同時に呼んでもよい（ロックで互いに排他される）。
    /// 引き取りたくないだけなら、返った `candidates` を捨てれば（Drop）解放される。
    pub fn scan(&self) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(Error::io("復元データの列挙", &self.dir, e)),
        };
        let mut data_ids = Vec::new();
        let mut lock_ids = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if is_temp_file_name(&name) {
                remove_if_stale_temp(&entry.path());
                continue;
            }
            let path = PathBuf::from(&name);
            let (Some(stem), Some(ext)) = (
                path.file_stem().and_then(|s| s.to_str()),
                path.extension().and_then(|s| s.to_str()),
            ) else {
                continue;
            };
            let Some(id) = RecoveryId::parse(stem) else {
                continue;
            };
            match ext {
                DATA_EXT => data_ids.push(id),
                LOCK_EXT => lock_ids.push(id),
                _ => {}
            }
        }
        data_ids.sort();

        // データのない孤立したロックファイル（書き出し前に落ちた、削除の途中）を片付ける。
        // 取れたら持ち主はいない。ただし、作られた直後のロックファイルは消さない: 作成と
        // ロックの取得の間に消すと、持ち主が消えたファイルをロックすることになる
        // （ロックファイルを消してはいけない理由と同じ競合）。古いものだけを対象にする。
        for id in lock_ids.iter().filter(|id| !data_ids.contains(id)) {
            let lock_file = lock_path(&self.dir, id);
            if !is_stale(&lock_file) {
                continue;
            }
            if let Ok(lock) = LockFile::try_acquire(&lock_file) {
                drop(lock);
                let _ = fs::remove_file(&lock_file);
            }
        }

        for id in data_ids {
            let lock = match LockFile::try_acquire(&lock_path(&self.dir, &id)) {
                Ok(lock) => lock,
                Err(Error::Busy { .. }) => {
                    // 生きている持ち主（または同時に引き取った別の走査）。読むだけ。
                    let meta = fs::read(data_path(&self.dir, &id))
                        .ok()
                        .and_then(|b| format::decode(&b).ok())
                        .map(|(m, _)| m);
                    report.live.push(LiveEntry { id, meta });
                    continue;
                }
                Err(e) => {
                    report.warnings.push(format!("{id}: {e}"));
                    continue;
                }
            };
            let path = data_path(&self.dir, &id);
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                // 引き取る前に持ち主が削除した。
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    report.warnings.push(format!("{id}: {e}"));
                    continue;
                }
            };
            match format::decode(&bytes) {
                Ok((meta, _text)) if meta.id == id => report.candidates.push(Candidate {
                    meta,
                    store: self.clone(),
                    lock,
                }),
                Ok(_) => {
                    let reason = QuarantineReason::Corrupt("ファイル名と id が違う".into());
                    self.quarantine(id, lock, reason, &mut report);
                }
                Err(DecodeError::Corrupt(why)) => {
                    self.quarantine(id, lock, QuarantineReason::Corrupt(why), &mut report)
                }
                Err(DecodeError::UnknownVersion(v)) => {
                    self.quarantine(id, lock, QuarantineReason::UnknownVersion(v), &mut report)
                }
            }
        }
        Ok(report.sort())
    }

    /// 引き取った（ロックを持つ）復元データを `quarantine/` へ移す。削除はしない。
    fn quarantine(
        &self,
        id: RecoveryId,
        lock: LockFile,
        reason: QuarantineReason,
        report: &mut ScanReport,
    ) {
        let tag = match reason {
            QuarantineReason::Corrupt(_) => "corrupt",
            QuarantineReason::UnknownVersion(_) => "unknown-version",
        };
        let qdir = self.dir.join(QUARANTINE_DIR);
        let dest = qdir.join(format!("{id}.{tag}.{DATA_EXT}"));
        let moved = fs::create_dir_all(&qdir)
            .and_then(|()| fs::rename(data_path(&self.dir, &id), &dest))
            .map_err(|e| Error::io("復元データの隔離", &dest, e));
        match moved {
            Ok(()) => {
                let lock_file = lock.path().to_path_buf();
                drop(lock);
                let _ = fs::remove_file(lock_file);
                report.quarantined.push(Quarantined {
                    id,
                    reason,
                    path: dest,
                });
            }
            Err(e) => report.warnings.push(format!("{id}: {e}")),
        }
    }
}

fn data_path(dir: &Path, id: &RecoveryId) -> PathBuf {
    dir.join(format!("{id}.{DATA_EXT}"))
}

fn lock_path(dir: &Path, id: &RecoveryId) -> PathBuf {
    dir.join(format!("{id}.{LOCK_EXT}"))
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > STALE_AGE)
}

fn remove_if_stale_temp(path: &Path) {
    if is_stale(path) {
        let _ = fs::remove_file(path);
    }
}

/// 走査の結果。
#[derive(Debug, Default)]
pub struct ScanReport {
    /// 復元の候補（孤児）。持ち主が落ちたもので、この走査が引き取った。更新が新しい順。
    pub candidates: Vec<Candidate>,
    /// 生きているプロセスが所有しているもの（触っていない）。
    pub live: Vec<LiveEntry>,
    /// 隔離したもの（壊れた・未知の版）。利用者に報告する。
    pub quarantined: Vec<Quarantined>,
    /// 個別に失敗して飛ばしたもの（権限など）。
    pub warnings: Vec<String>,
}

impl ScanReport {
    fn sort(mut self) -> Self {
        self.candidates
            .sort_by_key(|c| std::cmp::Reverse(c.meta.updated_ms));
        self
    }
}

/// 生きているプロセスが所有する復元データ。
#[derive(Debug, Clone)]
pub struct LiveEntry {
    pub id: RecoveryId,
    /// 読めた場合の付随情報。
    pub meta: Option<Meta>,
}

/// 隔離の理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineReason {
    Corrupt(String),
    UnknownVersion(u64),
}

/// 隔離した復元データ。
#[derive(Debug, Clone)]
pub struct Quarantined {
    pub id: RecoveryId,
    pub reason: QuarantineReason,
    /// 隔離後のファイルの場所。
    pub path: PathBuf,
}

/// 引き取った孤児の復元データ。ロックを保持するので、他のプロセスは触れない。
/// 何もせず捨てると解放され、孤児のまま残る。
#[derive(Debug)]
pub struct Candidate {
    pub meta: Meta,
    store: RecoveryStore,
    lock: LockFile,
}

impl Candidate {
    /// 本文を読む（検査つき）。
    pub fn read_text(&self) -> Result<String> {
        let path = data_path(&self.store.dir, &self.meta.id);
        let bytes = fs::read(&path).map_err(|e| Error::io("復元データの読み込み", &path, e))?;
        format::decode(&bytes)
            .map(|(_, text)| text)
            .map_err(|e| match e {
                DecodeError::Corrupt(why) => Error::corrupt(path.display().to_string(), why),
                DecodeError::UnknownVersion(v) => {
                    Error::corrupt(path.display().to_string(), format!("未知の版 {v}"))
                }
            })
    }

    /// 復元した文書が、この復元データを引き継ぐ。以後、同じ id・同じロックで書き出し、
    /// 保存・破棄で削除する（復元した内容は、利用者が保存するまで元ファイルに触れない）。
    pub fn into_slot(self) -> RecoverySlot {
        let id = self.meta.id.clone();
        RecoverySlot::new(
            self.store.paths,
            self.store.dir,
            id,
            self.lock,
            Some(self.meta.created_ms),
        )
    }

    /// 利用者が復元を断った。復元データを削除する。
    pub fn discard(self) -> Result<()> {
        self.into_slot().discard()
    }
}

/// [`RecoverySlot::write`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// 書き出した。
    Written,
    /// 既により新しい版を書き出した、または保存済みの版以前なので書かなかった。
    Stale,
}

#[derive(Debug)]
struct SlotState {
    created_ms: Option<u64>,
    /// 最後に書き出した版（ファイルがあるとき）。
    last_written: Option<Version>,
    /// 保存済みの版。これ以前の書き出しは受け付けない。
    saved_floor: Option<Version>,
}

/// 1 つの文書の復元データの所有者。所有のロックを保持し続け、書き出しと削除を行う。
/// 複数のスレッドから呼んでよい（内部で直列化する）。
#[derive(Debug)]
pub struct RecoverySlot {
    paths: AppPaths,
    dir: PathBuf,
    id: RecoveryId,
    state: Mutex<SlotState>,
    /// 所有を示す。`discard` で解放し、ファイルも消す。
    lock: LockFile,
}

impl RecoverySlot {
    fn new(
        paths: AppPaths,
        dir: PathBuf,
        id: RecoveryId,
        lock: LockFile,
        created_ms: Option<u64>,
    ) -> Self {
        Self {
            paths,
            dir,
            id,
            state: Mutex::new(SlotState {
                created_ms,
                last_written: None,
                saved_floor: None,
            }),
            lock,
        }
    }

    pub fn id(&self) -> &RecoveryId {
        &self.id
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 復元データを原子的に書き出す。UI スレッドで呼ばない（ワーカーで呼ぶ）。
    ///
    /// `snapshot.version` が、既に書き出した版より古い、または保存済みの版以前なら
    /// 書かずに [`WriteOutcome::Stale`] を返す（古い版が新しい復元データを上書き
    /// しない）。同じ版は書き直す（タイトルなどの更新を反映するため）。
    /// 完了の通知には、呼び出し側が持つ `DocStamp` を使う。
    pub fn write(&self, snapshot: &Snapshot, ctx: &TaskContext) -> Result<WriteOutcome> {
        let mut state = self.state();
        if state.saved_floor.is_some_and(|f| snapshot.version <= f)
            || state.last_written.is_some_and(|w| snapshot.version < w)
        {
            return Ok(WriteOutcome::Stale);
        }
        self.paths.ensure_dir(&self.dir)?;
        let now = format::now_ms();
        let created_ms = *state.created_ms.get_or_insert(now);
        let meta = Meta {
            id: self.id.clone(),
            kind: snapshot.kind,
            source_path: snapshot.source_path.clone(),
            format: snapshot.format,
            file_stamp: snapshot.file_stamp,
            lossy: snapshot.lossy,
            title: snapshot.title.clone(),
            doc_version: snapshot.version,
            created_ms,
            updated_ms: now,
            owner_pid: std::process::id(),
            body_len: 0,
        };
        let bytes = format::encode(&meta, &snapshot.text);
        write_replacing(&data_path(&self.dir, &self.id), &bytes, ctx)?;
        state.last_written = Some(snapshot.version);
        Ok(WriteOutcome::Written)
    }

    /// 元ファイルへの保存が成功した。`saved` 以前の版の復元データを削除する。
    /// 保存中に編集が進んで、より新しい版を書き出し済みなら残す。
    /// 以後 `saved` 以前の版の書き出しは受け付けない。
    pub fn saved(&self, saved: Version) -> Result<()> {
        let mut state = self.state();
        state.saved_floor = Some(state.saved_floor.map_or(saved, |f| f.max(saved)));
        if state.last_written.is_none_or(|w| w <= saved) {
            remove_data(&data_path(&self.dir, &self.id))?;
            state.last_written = None;
        }
        Ok(())
    }

    /// 利用者が変更を破棄した（文書を閉じた）。復元データとロックファイルを削除し、
    /// 所有を手放す。
    pub fn discard(self) -> Result<()> {
        remove_data(&data_path(&self.dir, &self.id))?;
        let lock_file = self.lock.path().to_path_buf();
        drop(self.lock);
        let _ = fs::remove_file(lock_file);
        Ok(())
    }

    /// 復元データを残したまま所有を手放す（異常終了に近い終わり方の模擬・テスト用）。
    /// 通常は [`discard`](Self::discard) を使う。
    pub fn release_keeping_data(self) {
        drop(self.lock);
    }
}

/// 原子的に書く。別のプロセスが走査で読んでいる最中の置換は、Windows では
/// 共有違反・アクセス拒否になりうるので、短い間隔で数回やり直す（ワーカーで呼ぶ前提）。
fn write_replacing(path: &Path, bytes: &[u8], ctx: &TaskContext) -> Result<()> {
    const ATTEMPTS: u32 = 40;
    let mut attempt = 0;
    loop {
        match write_atomic(path, bytes, ctx) {
            Err(e)
                if attempt + 1 < ATTEMPTS
                    && matches!(
                        classify(&e),
                        Some(IoFailure::InUse | IoFailure::PermissionDenied)
                    ) =>
            {
                ctx.cancel.check()?;
                attempt += 1;
                std::thread::sleep(Duration::from_millis(5 * u64::from(attempt.min(10))));
            }
            result => return result,
        }
    }
}

fn remove_data(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io("復元データの削除", path, e)),
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
