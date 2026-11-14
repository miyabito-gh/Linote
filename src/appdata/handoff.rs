//! 文書の受け渡し（U4）: タブを別ウィンドウ（別プロセス）へ移すときの受け渡しファイル。
//!
//! GUI 非依存。ファイルは `%LOCALAPPDATA%\Linote\handoff\`（[`AppPaths::handoff_dir`]）に
//! 原子的に書き、`linote.exe --adopt <ファイル>` で起動した新しいプロセスが読む。
//!
//! # 受け取りの確認（プロトコル）
//!
//! 元のプロセス（送り側）は、新しいプロセス（受け側）が文書を取り込んだと確認できてから
//! 元のタブを閉じる。確認はファイルの有無で行う。
//!
//! 1. 送り側が `handoff-<pid>-<時刻>.json` を [`write`] で書き、受け側を起動する。
//! 2. 受け側は [`read`] で読み、文書を開いてから [`claim`] する。`claim` は
//!    `.json` を `.adopted` へ改名してから消す。改名は原子的なので、成功するのは
//!    1 回だけ（送り側の [`cancel`] と競合しても、どちらか一方だけが勝つ）。
//! 3. 送り側は `.json` が無くなったら「受け取られた」とみなして元のタブを閉じる
//!    （[`is_pending`]）。待つ時間は [`CONFIRM_TIMEOUT`]。超えたら [`cancel`]（`.json` を
//!    `.cancelled` へ改名して消す）を試み、勝てば元のタブを残して失敗を知らせる。
//!    負けた（受け側が先に `claim` した）なら、受け取られたものとして扱う。
//! 4. 受け側は `claim` に負けたら（送り側が取り消した）、開いた文書を捨てる。
//!
//! 受け側が壊れた・古い・読めないファイルを受け取った場合は、何も取り込まず、
//! ファイルも `claim` しない（送り側は時間切れで元のタブを残す）。
//! 残ったファイルは [`sweep`] が一定時間後に片付ける。
//!
//! # ファイル形式
//!
//! UTF-8 の JSON 1 個。`version`（現在 1）、`created_ms`（Unix 時刻のミリ秒）、
//! `kind`（`file` / `memo` / `text`）と、種類ごとの項目。
//! - `file`: `path`（保存済みのファイル。通常・巨大とも。受け側が自分で読む）
//! - `memo`: `memo_id`（送り側が保存領域に書き終えている。受け側が読む）
//! - `text`: `doc_kind`（`untitled` / `clipboard` / `file`）、`path`（なければ null）、
//!   `title`、`encoding`、`bom`、`eol`、`dirty`、`disk`（ファイルの印 `{len, modified_ns}` か null）、
//!   `text`（本文。編集欄の内容そのまま。JSON の文字列なので Unicode は損なわない）

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::appdata::AppPaths;
use crate::core::{Error, MemoId, Result, TaskContext};
use crate::file::{write_atomic, FileStamp};
use crate::text::{Encoding, Eol, TextFormat};

/// 現在の形式の版。
pub const FORMAT_VERSION: u64 = 1;
/// 受け側が起動して取り込むまで待つ時間。
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
/// これより古い受け渡しファイルは受け取らない（古い起動の残り）。
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// [`sweep`] がこれより古いファイルを消す。
pub const SWEEP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

const EXT_JSON: &str = "json";

/// 本文つきで渡すときの文書の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Untitled,
    Clipboard,
    File,
}

impl TextKind {
    fn name(self) -> &'static str {
        match self {
            TextKind::Untitled => "untitled",
            TextKind::Clipboard => "clipboard",
            TextKind::File => "file",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "untitled" => TextKind::Untitled,
            "clipboard" => TextKind::Clipboard,
            "file" => TextKind::File,
            _ => return None,
        })
    }
}

/// 受け渡す文書。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffDoc {
    /// 保存済みのファイル（巨大ファイルを含む）。受け側がパスから開く。
    File { path: PathBuf },
    /// メモ。受け側が同じメモを保存領域から開く。
    Memo { id: MemoId },
    /// 本文を渡す（未保存の変更がある文書、無題、クリップボード由来）。
    Text(TextDoc),
}

/// [`HandoffDoc::Text`] の中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDoc {
    pub kind: TextKind,
    pub path: Option<PathBuf>,
    pub title: String,
    pub format: TextFormat,
    /// 未保存として扱うか（`true` なら受け側は復元データの対象にする）。
    pub dirty: bool,
    /// 元のファイルの印（受け側が外部での変更を調べる）。
    pub disk: Option<FileStamp>,
    pub text: String,
}

/// 受け渡しファイル 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// 作った時刻（Unix 時刻のミリ秒）。
    pub created_ms: u64,
    pub doc: HandoffDoc,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn corrupt(reason: impl Into<String>) -> Error {
    Error::corrupt("受け渡しファイル", reason)
}

fn time_to_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

fn ns_to_time(ns: i64) -> SystemTime {
    if ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(ns as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos(ns.unsigned_abs())
    }
}

impl Handoff {
    pub fn new(doc: HandoffDoc) -> Handoff {
        Handoff {
            created_ms: now_ms(),
            doc,
        }
    }

    /// ファイルの内容にする。
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Map::new();
        o.insert("version".into(), json!(FORMAT_VERSION));
        o.insert("created_ms".into(), json!(self.created_ms));
        match &self.doc {
            HandoffDoc::File { path } => {
                o.insert("kind".into(), json!("file"));
                o.insert("path".into(), json!(path.to_string_lossy()));
            }
            HandoffDoc::Memo { id } => {
                o.insert("kind".into(), json!("memo"));
                o.insert("memo_id".into(), json!(id.as_str()));
            }
            HandoffDoc::Text(t) => {
                o.insert("kind".into(), json!("text"));
                o.insert("doc_kind".into(), json!(t.kind.name()));
                o.insert(
                    "path".into(),
                    t.path
                        .as_ref()
                        .map_or(Value::Null, |p| json!(p.to_string_lossy())),
                );
                o.insert("title".into(), json!(t.title));
                o.insert("encoding".into(), json!(t.format.encoding.name()));
                o.insert("bom".into(), json!(t.format.bom));
                o.insert("eol".into(), json!(t.format.eol.label()));
                o.insert("dirty".into(), json!(t.dirty));
                o.insert(
                    "disk".into(),
                    t.disk.map_or(Value::Null, |s| {
                        json!({
                            "len": s.len,
                            "modified_ns": s.modified.map(time_to_ns),
                        })
                    }),
                );
                o.insert("text".into(), json!(t.text));
            }
        }
        serde_json::to_vec(&Value::Object(o)).expect("JSON にできる")
    }

    /// ファイルの内容から読む。壊れていれば [`Error::Corrupt`]。新しさは確かめない
    /// （[`check_fresh`](Self::check_fresh)）。
    pub fn decode(bytes: &[u8]) -> Result<Handoff> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| corrupt(e.to_string()))?;
        let o = v
            .as_object()
            .ok_or_else(|| corrupt("オブジェクトではない"))?;
        let version = get_u64(o, "version")?;
        if version != FORMAT_VERSION {
            return Err(Error::unsupported(format!(
                "受け渡しファイルの版 {version}（この Linote は {FORMAT_VERSION} を読みます）"
            )));
        }
        let created_ms = get_u64(o, "created_ms")?;
        let doc = match get_str(o, "kind")? {
            "file" => HandoffDoc::File {
                path: PathBuf::from(get_str(o, "path")?),
            },
            "memo" => HandoffDoc::Memo {
                id: MemoId::parse(get_str(o, "memo_id")?)
                    .ok_or_else(|| corrupt("`memo_id` が不正"))?,
            },
            "text" => {
                let kind = TextKind::from_name(get_str(o, "doc_kind")?)
                    .ok_or_else(|| corrupt("`doc_kind` が不正"))?;
                let path = match o.get("path") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(PathBuf::from(s)),
                    Some(_) => return Err(corrupt("`path` が不正")),
                };
                if kind == TextKind::File && path.is_none() {
                    return Err(corrupt("ファイルの文書に `path` がない"));
                }
                let encoding = Encoding::from_name(get_str(o, "encoding")?)
                    .ok_or_else(|| corrupt("`encoding` が不正"))?;
                let eol =
                    Eol::from_label(get_str(o, "eol")?).ok_or_else(|| corrupt("`eol` が不正"))?;
                let bom = o
                    .get("bom")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| corrupt("`bom` が不正"))?;
                let dirty = o
                    .get("dirty")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| corrupt("`dirty` が不正"))?;
                let disk = match o.get("disk") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(d)) => Some(FileStamp {
                        len: get_u64(d, "len")?,
                        modified: d.get("modified_ns").and_then(Value::as_i64).map(ns_to_time),
                    }),
                    Some(_) => return Err(corrupt("`disk` が不正")),
                };
                HandoffDoc::Text(TextDoc {
                    kind,
                    path,
                    title: get_str(o, "title")?.to_owned(),
                    format: TextFormat { encoding, bom, eol },
                    dirty,
                    disk,
                    text: get_str(o, "text")?.to_owned(),
                })
            }
            other => return Err(corrupt(format!("`kind` が不明: {other}"))),
        };
        Ok(Handoff { created_ms, doc })
    }

    /// 古すぎる（[`MAX_AGE`] を超えた）ファイルは断る。時計が戻った場合（未来の時刻）は
    /// 古いとは見なさない。
    pub fn check_fresh(&self, now_ms: u64) -> Result<()> {
        let age = now_ms.saturating_sub(self.created_ms);
        if age > MAX_AGE.as_millis() as u64 {
            return Err(Error::unavailable(
                "受け渡しファイル",
                format!(
                    "{} 分前のものなので取り込みません（古い起動の残りです）",
                    age / 60_000
                ),
            ));
        }
        Ok(())
    }
}

fn get_u64(o: &Map<String, Value>, key: &str) -> Result<u64> {
    o.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| corrupt(format!("`{key}` が不正")))
}

fn get_str<'a>(o: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    o.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| corrupt(format!("`{key}` が不正")))
}

/// 受け渡しファイルの名前（送り側のプロセス ID と時刻から。同じ時刻でも `seq` で分ける）。
pub fn file_name(pid: u32, created_ms: u64, seq: u32) -> String {
    format!("handoff-{pid}-{created_ms}-{seq}.{EXT_JSON}")
}

/// 受け渡しファイルを書く。フォルダーは必要なら作る。書いたパスを返す。
pub fn write(paths: &AppPaths, handoff: &Handoff, seq: u32) -> Result<PathBuf> {
    let dir = paths.handoff_dir();
    paths.ensure_dir(&dir)?;
    let path = dir.join(file_name(std::process::id(), handoff.created_ms, seq));
    write_atomic(&path, &handoff.encode(), &TaskContext::detached())?;
    Ok(path)
}

/// 受け渡しファイルを読む（新しさも確かめる）。
pub fn read(path: &Path, now_ms: u64) -> Result<Handoff> {
    let bytes = std::fs::read(path).map_err(|e| Error::io("読み込み", path, e))?;
    let handoff = Handoff::decode(&bytes)?;
    handoff.check_fresh(now_ms)?;
    Ok(handoff)
}

fn renamed(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// 受け側: 取り込んだことを知らせる（ファイルを消す）。送り側が先に取り消していて
/// 取れなかったら `false`（受け側は取り込んだ文書を捨てる）。
pub fn claim(path: &Path) -> bool {
    take(path, "adopted")
}

/// 送り側: 時間切れで取り消す。受け側より先に取れたら `true`
/// （元のタブを残す）。`false` なら受け側が先に取り込んでいる。
pub fn cancel(path: &Path) -> bool {
    take(path, "cancelled")
}

fn take(path: &Path, ext: &str) -> bool {
    let to = renamed(path, ext);
    if std::fs::rename(path, &to).is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&to);
    true
}

/// ファイルがまだ残っている（受け側が取り込んでいない）。
pub fn is_pending(path: &Path) -> bool {
    path.exists()
}

/// 受け渡しフォルダーの、[`SWEEP_AGE`] より古いファイルを消す（強制終了の残り）。
/// 消した数を返す。
pub fn sweep(paths: &AppPaths, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(paths.handoff_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name.to_str().is_some_and(|n| n.starts_with("handoff-"))
            || crate::file::is_temp_file_name(&name);
        if !ours {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > SWEEP_AGE);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ------------------------------------------------------------------ 新しいウィンドウの位置

/// コマンド引数・`--pos` 引数の位置（画面座標。負の値もある）を文字列にする。
pub fn format_position(pos: (i32, i32)) -> String {
    format!("{},{}", pos.0, pos.1)
}

/// [`format_position`] の逆。
pub fn parse_position(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// 離した位置 `drop`（画面座標）に新しいウィンドウ（大きさ `size`）を置くときの左上の位置。
/// 離したところにタブバーが来るよう、左へ 1/4 幅、上へ `title_offset` ずらし、作業領域
/// `work`（左, 上, 右, 下）からはみ出さないように収める。
pub fn place_window(
    drop: (i32, i32),
    size: (i32, i32),
    work: (i32, i32, i32, i32),
    title_offset: i32,
) -> (i32, i32) {
    let fit = |want: i32, len: i32, lo: i32, hi: i32| {
        if len >= hi - lo {
            lo
        } else {
            want.clamp(lo, hi - len)
        }
    };
    (
        fit(drop.0 - size.0 / 4, size.0, work.0, work.2),
        fit(drop.1 - title_offset, size.1, work.1, work.3),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_doc() -> TextDoc {
        TextDoc {
            kind: TextKind::File,
            path: Some(PathBuf::from(r"C:\メモ\あ い.txt")),
            title: "あ い.txt".into(),
            format: TextFormat {
                encoding: Encoding::Cp932,
                bom: false,
                eol: Eol::Cr,
            },
            dirty: true,
            disk: Some(FileStamp {
                len: 42,
                modified: Some(UNIX_EPOCH + Duration::new(1_700_000_000, 123)),
            }),
            text: "一行目\r二行目 \"引用\" \\ \u{1F600}\n\0末尾".into(),
        }
    }

    #[test]
    fn text_round_trips_with_format_and_dirty_flag() {
        for (kind, path, enc, bom, eol, dirty) in [
            (
                TextKind::File,
                Some("a.txt"),
                Encoding::Utf16Le,
                true,
                Eol::CrLf,
                true,
            ),
            (
                TextKind::Untitled,
                None,
                Encoding::Utf8,
                false,
                Eol::Lf,
                true,
            ),
            (
                TextKind::Clipboard,
                None,
                Encoding::Utf8,
                true,
                Eol::Lf,
                true,
            ),
            (
                TextKind::Untitled,
                None,
                Encoding::Utf16Be,
                true,
                Eol::Cr,
                false,
            ),
        ] {
            let mut t = text_doc();
            t.kind = kind;
            t.path = path.map(PathBuf::from);
            t.format = TextFormat {
                encoding: enc,
                bom,
                eol,
            };
            t.dirty = dirty;
            if path.is_none() {
                t.disk = None;
            }
            let h = Handoff::new(HandoffDoc::Text(t));
            assert_eq!(Handoff::decode(&h.encode()).unwrap(), h);
        }
        let h = Handoff::new(HandoffDoc::Text(text_doc()));
        assert_eq!(Handoff::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn file_and_memo_round_trip() {
        for doc in [
            HandoffDoc::File {
                path: PathBuf::from(r"D:\big\巨大.log"),
            },
            HandoffDoc::Memo {
                id: MemoId::parse("abc-123").unwrap(),
            },
        ] {
            let h = Handoff::new(doc);
            assert_eq!(Handoff::decode(&h.encode()).unwrap(), h);
        }
    }

    #[test]
    fn corrupt_and_unknown_files_are_rejected() {
        let good = Handoff::new(HandoffDoc::Text(text_doc())).encode();
        assert!(matches!(
            Handoff::decode(&good[..good.len() / 2]),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(Handoff::decode(b""), Err(Error::Corrupt { .. })));
        assert!(matches!(
            Handoff::decode(b"[1]"),
            Err(Error::Corrupt { .. })
        ));
        let bad = |edit: &dyn Fn(&mut Value)| {
            let mut v: Value = serde_json::from_slice(&good).unwrap();
            edit(&mut v);
            Handoff::decode(&serde_json::to_vec(&v).unwrap())
        };
        assert!(matches!(
            bad(&|v| v["version"] = json!(2)),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            bad(&|v| v["encoding"] = json!("latin1")),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            bad(&|v| v["eol"] = json!("??")),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            bad(&|v| v["dirty"] = json!("yes")),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            bad(&|v| v["kind"] = json!("zip")),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            bad(&|v| v["text"] = json!(5)),
            Err(Error::Corrupt { .. })
        ));
        assert!(matches!(
            bad(&|v| v["path"] = Value::Null),
            Err(Error::Corrupt { .. })
        ));
        let memo = Handoff::new(HandoffDoc::Memo {
            id: MemoId::parse("m1").unwrap(),
        })
        .encode();
        let mut v: Value = serde_json::from_slice(&memo).unwrap();
        v["memo_id"] = json!("../evil");
        assert!(Handoff::decode(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    #[test]
    fn stale_files_are_refused_but_clock_skew_is_not() {
        let mut h = Handoff::new(HandoffDoc::Memo {
            id: MemoId::parse("m").unwrap(),
        });
        h.created_ms = 1_000_000;
        assert!(h.check_fresh(1_000_000 + 60_000).is_ok());
        assert!(h
            .check_fresh(1_000_000 + MAX_AGE.as_millis() as u64 + 1)
            .is_err());
        assert!(h.check_fresh(500).is_ok());
    }

    fn temp_paths(tag: &str) -> AppPaths {
        let dir = std::env::temp_dir().join(format!("linote-handoff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppPaths::with_root(dir)
    }

    #[test]
    fn write_read_claim_protocol() {
        let paths = temp_paths("claim");
        let h = Handoff::new(HandoffDoc::Text(text_doc()));
        let file = write(&paths, &h, 1).unwrap();
        assert!(file.starts_with(paths.handoff_dir()));
        assert!(is_pending(&file));
        assert_eq!(read(&file, now_ms()).unwrap(), h);
        assert!(claim(&file));
        assert!(!is_pending(&file));
        // 2 回目は取れない。取り消しも負ける。
        assert!(!claim(&file));
        assert!(!cancel(&file));
        // 残りのファイルはない（`.adopted` も消えている）。
        assert_eq!(std::fs::read_dir(paths.handoff_dir()).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn cancel_wins_once_and_blocks_the_receiver() {
        let paths = temp_paths("cancel");
        let h = Handoff::new(HandoffDoc::Memo {
            id: MemoId::parse("m").unwrap(),
        });
        let file = write(&paths, &h, 2).unwrap();
        assert!(cancel(&file));
        assert!(!claim(&file));
        assert!(read(&file, now_ms()).is_err());
        assert_eq!(std::fs::read_dir(paths.handoff_dir()).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn reading_corrupt_or_stale_files_does_not_remove_them() {
        let paths = temp_paths("bad");
        paths.ensure_dir(&paths.handoff_dir()).unwrap();
        let bad = paths.handoff_dir().join("handoff-1-1-1.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert!(matches!(read(&bad, now_ms()), Err(Error::Corrupt { .. })));
        assert!(bad.exists());
        let mut h = Handoff::new(HandoffDoc::Memo {
            id: MemoId::parse("m").unwrap(),
        });
        h.created_ms = 1;
        let stale = paths.handoff_dir().join("handoff-1-2-1.json");
        std::fs::write(&stale, h.encode()).unwrap();
        assert!(read(&stale, now_ms()).is_err());
        // 掃除は古いものだけ。
        assert_eq!(sweep(&paths, SystemTime::now()), 0);
        assert_eq!(sweep(&paths, SystemTime::now() + SWEEP_AGE * 2), 2);
        assert!(!bad.exists());
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let paths = temp_paths("missing");
        assert!(matches!(
            read(&paths.handoff_dir().join("none.json"), now_ms()),
            Err(Error::Io { .. })
        ));
    }

    #[test]
    fn positions_round_trip_and_reject_garbage() {
        for p in [(0, 0), (-1920, 40), (3000, -5)] {
            assert_eq!(parse_position(&format_position(p)), Some(p));
        }
        assert_eq!(parse_position(" 5 , 7 "), Some((5, 7)));
        for bad in ["", "1", "a,b", "1,2,3", "1;2", ","] {
            assert_eq!(parse_position(bad), None, "{bad}");
        }
    }

    #[test]
    fn window_placement_stays_in_the_work_area() {
        let work = (0, 0, 1920, 1040);
        // 左へ 1/4 幅、上へ 20。
        assert_eq!(place_window((800, 300), (1000, 700), work, 20), (550, 280));
        // 右下の外れは収める。
        assert_eq!(
            place_window((1900, 1000), (1000, 700), work, 20),
            (920, 340)
        );
        // 左上の外れ。
        assert_eq!(place_window((-50, -50), (1000, 700), work, 20), (0, 0));
        // 左のモニター（負の座標）。
        let left = (-1920, 0, 0, 1040);
        assert_eq!(
            place_window((-1000, 100), (800, 600), left, 20),
            (-1200, 80)
        );
        assert_eq!(place_window((50, 100), (800, 600), left, 20), (-800, 80));
        // 作業領域より大きいウィンドウは左上に合わせる。
        assert_eq!(place_window((500, 500), (3000, 2000), work, 20), (0, 0));
    }
}
