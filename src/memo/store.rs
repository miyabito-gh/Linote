//! メモの保存先の操作。
//!
//! # 保存形式（`memos/`）
//!
//! - `<id>.memo`: 1 メモ 1 ファイル。UTF-8 の JSON 1 個
//!   `{format:1, id, name, created_ms, updated_ms, version, body}`。原子的に置換する。
//! - `.lock`: 変更（作成・更新・名前変更・削除）の間だけ取る排他ロック。版の照合と
//!   書き込みは同じロックの中。読み取りはロックを取らない（置換が原子的なので
//!   古い内容か新しい内容のどちらかが読める）。ロックは消さない。
//! - 一時ファイル（[`crate::file::is_temp_file_name`]）と壊れたファイルは一覧で飛ばす。
//!
//! # 衝突の方針
//!
//! 版が食い違う更新は [`Error::VersionMismatch`] で拒否する。自動保存は
//! [`MemoStore::save_or_fork`] を使い、食い違ったら相手を上書きせず、
//! 自分の内容を「<名前>（競合）」という別のメモに残す。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::search::{self, MemoHit};
use crate::appdata::AppPaths;
use crate::core::{
    Budget, Completion, Error, MemoId, Outcome, Result, TargetRef, TaskContext, Version,
};
use crate::file::{is_temp_file_name, write_atomic, LockFile};

/// メモ 1 件。`created_ms`／`updated_ms` は UNIX 時刻（ミリ秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memo {
    pub id: MemoId,
    pub name: String,
    pub body: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// 保存された内容が変わるたびに 1 進む。新規は [`Version::INITIAL`]。
    pub version: Version,
}

/// 保存結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 前提の版と一致し、同じメモに保存した。
    Saved(Memo),
    /// 別のウィンドウ・プロセスが先に更新していた。相手のメモには触れず、
    /// この内容を別のメモ（名前に「（競合）」を付ける）として残した。
    /// `original_actual` は衝突したメモの現在の版。
    Forked {
        memo: Memo,
        original_actual: Version,
    },
}

const EXT: &str = "memo";
const LOCK_NAME: &str = ".lock";
const FORMAT: u64 = 1;
/// ロックが取れないときに待つ最長時間（他のウィンドウの書き込み 1 回分なので短い）。
const LOCK_WAIT: Duration = Duration::from_secs(3);
/// 1 メモの本文の上限（バイト）。
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// [`MemoStore::list`] の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// 更新の新しい順。
    pub memos: Vec<Memo>,
    /// 壊れていて読めなかったファイルの数。
    pub skipped: usize,
}

/// メモの保存先への窓口。軽い値で複製してよい。作るだけでは何も読み書きしない。
#[derive(Debug, Clone)]
pub struct MemoStore {
    paths: AppPaths,
    dir: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl MemoStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            dir: paths.memos_dir(),
            paths: paths.clone(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_of(&self, id: &MemoId) -> PathBuf {
        self.dir.join(format!("{}.{EXT}", id.as_str()))
    }

    fn lock(&self) -> Result<LockFile> {
        self.paths.ensure_dir(&self.dir)?;
        let path = self.dir.join(LOCK_NAME);
        let start = Instant::now();
        loop {
            match LockFile::try_acquire(&path) {
                Err(Error::Busy { .. }) if start.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => return other,
            }
        }
    }

    fn write(&self, memo: &Memo) -> Result<()> {
        let value = json!({
            "format": FORMAT,
            "id": memo.id.as_str(),
            "name": memo.name,
            "created_ms": memo.created_ms,
            "updated_ms": memo.updated_ms,
            "version": memo.version.as_u64(),
            "body": memo.body,
        });
        let bytes = serde_json::to_vec(&value).map_err(|e| Error::Internal(e.to_string()))?;
        write_atomic(&self.file_of(&memo.id), &bytes, &TaskContext::detached())
    }

    /// 新しいメモを作る。ファイルは「新規作成でのみ成功する」: 既にあれば ID を作り直す。
    pub fn create(&self, name: &str, body: &str) -> Result<Memo> {
        check_body(body)?;
        let _lock = self.lock()?;
        self.create_locked(name, body)
    }

    fn create_locked(&self, name: &str, body: &str) -> Result<Memo> {
        let now = now_ms();
        for _ in 0..8 {
            let id = MemoId::generate();
            if self.file_of(&id).exists() {
                continue;
            }
            let memo = Memo {
                id,
                name: name.to_owned(),
                body: body.to_owned(),
                created_ms: now,
                updated_ms: now,
                version: Version::INITIAL,
            };
            self.write(&memo)?;
            return Ok(memo);
        }
        Err(Error::Internal("メモ ID が衝突し続けた".into()))
    }

    /// 1 件読む。なければ `NotFound`、壊れていれば `Corrupt`。
    pub fn load(&self, id: &MemoId) -> Result<Memo> {
        let path = self.file_of(id);
        let bytes = fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::not_found(format!("メモ {id}"))
            } else {
                Error::io("メモの読み込み", &path, e)
            }
        })?;
        parse(id, &bytes)
    }

    /// 全メモを更新の新しい順に読む。壊れたファイルは飛ばして `skipped` に数える。
    /// 初めて一覧を開くときに呼ぶ（起動時には呼ばない）。ディレクトリがなければ空。
    pub fn list(&self, ctx: &TaskContext) -> Result<Outcome<Listing>> {
        let mut listing = Listing::default();
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Outcome::complete(listing))
            }
            Err(e) => return Err(Error::io("メモの一覧", &self.dir, e)),
        };
        let mut budget = Budget::new(ctx.limits);
        let mut completion = Completion::Complete;
        for entry in entries.flatten() {
            if ctx.cancel.is_cancelled() {
                completion = Completion::Cancelled;
                break;
            }
            let path = entry.path();
            if is_temp_file_name(&entry.file_name()) || path.extension().is_none_or(|e| e != EXT) {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(MemoId::parse)
            else {
                continue;
            };
            match self.load(&id) {
                Ok(memo) => {
                    if let Err(c) = budget.try_add(memo.body.len() as u64) {
                        completion = c;
                        break;
                    }
                    listing.memos.push(memo);
                }
                Err(Error::NotFound { .. }) => {}
                Err(_) => listing.skipped += 1,
            }
        }
        listing
            .memos
            .sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then(a.id.cmp(&b.id)));
        Ok(Outcome::new(listing, completion))
    }

    /// 本文を更新する。`expected` が現在の版と違えば `VersionMismatch`（黙って上書きしない）。
    /// 内容が同じなら書かず版も進めない。
    pub fn update(&self, id: &MemoId, expected: Version, body: &str) -> Result<Memo> {
        check_body(body)?;
        let _lock = self.lock()?;
        let memo = self.load_checked(id, expected)?;
        self.apply(memo, None, Some(body))
    }

    /// 自動保存の入口。衝突したら相手を上書きせず、この内容を別のメモとして残す。
    pub fn save_or_fork(&self, id: &MemoId, expected: Version, body: &str) -> Result<SaveOutcome> {
        check_body(body)?;
        let _lock = self.lock()?;
        match self.load_checked(id, expected) {
            Ok(memo) => Ok(SaveOutcome::Saved(self.apply(memo, None, Some(body))?)),
            Err(Error::VersionMismatch { actual, .. }) => {
                let name = format!("{}（競合）", self.load(id)?.name);
                let memo = self.create_locked(&name, body)?;
                Ok(SaveOutcome::Forked {
                    memo,
                    original_actual: actual,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// 名前を変える。版の扱いは [`update`](Self::update) と同じ。
    pub fn rename(&self, id: &MemoId, expected: Version, name: &str) -> Result<Memo> {
        let _lock = self.lock()?;
        let memo = self.load_checked(id, expected)?;
        self.apply(memo, Some(name), None)
    }

    /// 削除する。版の扱いは [`update`](Self::update) と同じ。
    pub fn delete(&self, id: &MemoId, expected: Version) -> Result<()> {
        let _lock = self.lock()?;
        self.load_checked(id, expected)?;
        let path = self.file_of(id);
        fs::remove_file(&path).map_err(|e| Error::io("メモの削除", &path, e))
    }

    fn apply(&self, mut memo: Memo, name: Option<&str>, body: Option<&str>) -> Result<Memo> {
        let mut changed = false;
        if let Some(name) = name.filter(|n| *n != memo.name) {
            memo.name = name.to_owned();
            changed = true;
        }
        if let Some(body) = body.filter(|b| *b != memo.body) {
            memo.body = body.to_owned();
            changed = true;
        }
        if changed {
            memo.updated_ms = now_ms();
            memo.version = memo.version.next();
            self.write(&memo)?;
        }
        Ok(memo)
    }

    fn load_checked(&self, id: &MemoId, expected: Version) -> Result<Memo> {
        let memo = self.load(id)?;
        if memo.version != expected {
            return Err(Error::VersionMismatch {
                target: TargetRef::Memo(id.clone()),
                expected,
                actual: memo.version,
            });
        }
        Ok(memo)
    }

    /// 保存をバックグラウンドで行う（UI スレッドを止めない）。結果は `done` に渡す。
    /// `done` は作業スレッドで呼ばれるので、UI へは `ui_channel` で送ること。
    pub fn save_in_background(
        &self,
        id: MemoId,
        expected: Version,
        body: String,
        done: impl FnOnce(Result<SaveOutcome>) + Send + 'static,
    ) {
        let store = self.clone();
        crate::core::WorkerPool::global().spawn(move || {
            done(store.save_or_fork(&id, expected, &body));
        });
    }

    /// 名前・本文で検索する（取り決めは `search` モジュールの冒頭）。
    pub fn search(&self, query: &str, ctx: &TaskContext) -> Result<Outcome<Vec<MemoHit>>> {
        let listing = self.list(ctx)?;
        let list_completion = listing.completion;
        let hits = search::search(&listing.value.memos, query, ctx);
        let completion = if list_completion.is_complete() {
            hits.completion
        } else {
            list_completion
        };
        Ok(Outcome::new(hits.value, completion))
    }
}

fn check_body(body: &str) -> Result<()> {
    if body.len() > MAX_BODY_BYTES {
        return Err(Error::LimitExceeded {
            what: "メモの本文".into(),
            limit: MAX_BODY_BYTES as u64,
        });
    }
    Ok(())
}

fn parse(id: &MemoId, bytes: &[u8]) -> Result<Memo> {
    let corrupt = |reason: &str| Error::corrupt(format!("メモ {id}"), reason);
    let v: Value = serde_json::from_slice(bytes).map_err(|e| corrupt(&e.to_string()))?;
    if v["format"].as_u64() != Some(FORMAT) {
        return Err(corrupt("未知の形式"));
    }
    if v["id"].as_str() != Some(id.as_str()) {
        return Err(corrupt("ID が一致しない"));
    }
    let text = |k: &str| v[k].as_str().map(str::to_owned);
    match (
        text("name"),
        text("body"),
        v["created_ms"].as_u64(),
        v["updated_ms"].as_u64(),
        v["version"].as_u64(),
    ) {
        (Some(name), Some(body), Some(created_ms), Some(updated_ms), Some(version)) => Ok(Memo {
            id: id.clone(),
            name,
            body,
            created_ms,
            updated_ms,
            version: Version::from_raw(version),
        }),
        _ => Err(corrupt("項目が足りない")),
    }
}
