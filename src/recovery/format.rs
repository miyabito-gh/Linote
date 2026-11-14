//! 復元データ 1 件のファイル形式。
//!
//! ```text
//! {ヘッダー: 1 行のコンパクトな JSON}\n
//! {本文: UTF-8 のバイト列。ヘッダーの body_len バイト}
//! ```
//!
//! 1 ファイルに収めるので、原子的な置換だけで本文と付随情報の整合が保てる。
//! 本文の長さと FNV-1a 64 ビットのハッシュをヘッダーに持ち、途中で切れたものや
//! 壊れたものを読み込み時に見分ける（暗号学的な強さは要らない）。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::core::Version;
use crate::file::FileStamp;
use crate::text::{Encoding, Eol, TextFormat};

/// 形式の版。ヘッダーの `version`。互換のない変更で上げる。
pub const FORMAT_VERSION: u64 = 1;
const FORMAT_NAME: &str = "linote-recovery";

/// 復元データの永続的な識別子（ファイル名の幹。英小文字・数字・`-`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecoveryId(String);

impl RecoveryId {
    /// このプロセスで新しい識別子を作る（プロセス ID・時刻・連番）。
    pub(crate) fn generate() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        RecoveryId(format!(
            "{:x}-{:x}-{:x}",
            std::process::id(),
            ms,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// 文字列から。ファイル名に使える形（1〜64 文字の英小文字・数字・`-`）だけ受ける。
    pub fn parse(s: &str) -> Option<Self> {
        let ok = !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-');
        ok.then(|| RecoveryId(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecoveryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 復元データの元になった文書の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryKind {
    File,
    Untitled,
    Clipboard,
    /// AI が変更した文書（次の版）。
    Ai,
}

impl RecoveryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            RecoveryKind::File => "file",
            RecoveryKind::Untitled => "untitled",
            RecoveryKind::Clipboard => "clipboard",
            RecoveryKind::Ai => "ai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::File, Self::Untitled, Self::Clipboard, Self::Ai]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// 書き出す内容。UI が UI スレッドで作り、ワーカーへ渡す（本文は写し）。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub kind: RecoveryKind,
    /// 元ファイルのパス（未保存の新規などは `None`）。
    pub source_path: Option<PathBuf>,
    pub format: TextFormat,
    /// 開いたとき（保存したとき）の元ファイルの状態。復元時の外部変更の検知に使う。
    pub file_stamp: Option<FileStamp>,
    pub title: String,
    /// この本文の版（書き出しの新旧の照合に使う）。
    pub version: Version,
    pub text: String,
}

/// 復元データの付随情報（本文を除く）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub id: RecoveryId,
    pub kind: RecoveryKind,
    pub source_path: Option<PathBuf>,
    pub format: TextFormat,
    pub file_stamp: Option<FileStamp>,
    pub title: String,
    /// 書き出したプロセスでの版。他のプロセスでは意味を持たず、参考情報。
    pub doc_version: Version,
    /// 最初の書き出しの時刻（UNIX エポックからのミリ秒）。
    pub created_ms: u64,
    /// 最後の書き出しの時刻（同上）。
    pub updated_ms: u64,
    /// 書き出したプロセスの ID（参考情報。生死の判定はロックで行う）。
    pub owner_pid: u32,
    pub body_len: u64,
}

/// 読み込みの失敗の分類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 壊れている（途中で切れた、検査が合わない、形が違う）。
    Corrupt(String),
    /// 知らない版（新しい Linote が書いたものなど）。
    UnknownVersion(u64),
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn time_to_ns(t: SystemTime) -> Option<i64> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).ok(),
        Err(e) => i64::try_from(e.duration().as_nanos()).ok().map(|n| -n),
    }
}

fn ns_to_time(ns: i64) -> SystemTime {
    if ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(ns as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos(ns.unsigned_abs())
    }
}

/// ファイルの内容（ヘッダー + 本文）を作る。`meta.body_len` は無視して本文から決める。
pub(crate) fn encode(meta: &Meta, text: &str) -> Vec<u8> {
    let stamp = match &meta.file_stamp {
        None => Value::Null,
        Some(s) => json!({
            "len": s.len,
            "modified_ns": s.modified.and_then(time_to_ns),
        }),
    };
    let header = json!({
        "format": FORMAT_NAME,
        "version": FORMAT_VERSION,
        "id": meta.id.as_str(),
        "kind": meta.kind.as_str(),
        "source_path": meta.source_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "encoding": meta.format.encoding.name(),
        "bom": meta.format.bom,
        "eol": meta.format.eol.label(),
        "file_stamp": stamp,
        "title": meta.title,
        "doc_version": meta.doc_version.as_u64(),
        "created_ms": meta.created_ms,
        "updated_ms": meta.updated_ms,
        "owner_pid": meta.owner_pid,
        "body_len": text.len() as u64,
        "body_fnv": format!("{:016x}", fnv1a(text.as_bytes())),
    });
    let mut out = header.to_string().into_bytes();
    out.push(b'\n');
    out.extend_from_slice(text.as_bytes());
    out
}

fn corrupt(msg: impl Into<String>) -> DecodeError {
    DecodeError::Corrupt(msg.into())
}

fn get_u64(h: &Map<String, Value>, key: &str) -> Result<u64, DecodeError> {
    h.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| corrupt(format!("`{key}` がない・不正")))
}

fn get_str<'a>(h: &'a Map<String, Value>, key: &str) -> Result<&'a str, DecodeError> {
    h.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| corrupt(format!("`{key}` がない・不正")))
}

/// ファイルの内容を検査して読む。本文は UTF-8 として検証済みの `String`。
pub(crate) fn decode(bytes: &[u8]) -> Result<(Meta, String), DecodeError> {
    let nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| corrupt("ヘッダーの終わりがない"))?;
    let header: Value =
        serde_json::from_slice(&bytes[..nl]).map_err(|e| corrupt(format!("ヘッダー: {e}")))?;
    let h = header
        .as_object()
        .ok_or_else(|| corrupt("ヘッダーが JSON オブジェクトでない"))?;
    if h.get("format").and_then(Value::as_str) != Some(FORMAT_NAME) {
        return Err(corrupt("復元データの形式ではない"));
    }
    let version = get_u64(h, "version")?;
    if version != FORMAT_VERSION {
        return Err(DecodeError::UnknownVersion(version));
    }
    let body = &bytes[nl + 1..];
    let body_len = get_u64(h, "body_len")?;
    if body.len() as u64 != body_len {
        return Err(corrupt(format!(
            "本文の長さが合わない（期待 {body_len}、実際 {}）",
            body.len()
        )));
    }
    if format!("{:016x}", fnv1a(body)) != get_str(h, "body_fnv")? {
        return Err(corrupt("本文の検査値が合わない"));
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| corrupt("本文が UTF-8 でない"))?
        .to_owned();

    let id = RecoveryId::parse(get_str(h, "id")?).ok_or_else(|| corrupt("`id` が不正"))?;
    let kind = RecoveryKind::parse(get_str(h, "kind")?).ok_or_else(|| corrupt("`kind` が不正"))?;
    let encoding =
        Encoding::from_name(get_str(h, "encoding")?).ok_or_else(|| corrupt("`encoding` が不正"))?;
    let eol = Eol::from_label(get_str(h, "eol")?).ok_or_else(|| corrupt("`eol` が不正"))?;
    let bom = h
        .get("bom")
        .and_then(Value::as_bool)
        .ok_or_else(|| corrupt("`bom` が不正"))?;
    let source_path = match h.get("source_path") {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => Some(Path::new(s).to_path_buf()),
        Some(_) => return Err(corrupt("`source_path` が不正")),
    };
    let file_stamp = match h.get("file_stamp") {
        Some(Value::Null) | None => None,
        Some(Value::Object(o)) => Some(FileStamp {
            len: get_u64(o, "len")?,
            modified: o.get("modified_ns").and_then(Value::as_i64).map(ns_to_time),
        }),
        Some(_) => return Err(corrupt("`file_stamp` が不正")),
    };
    let meta = Meta {
        id,
        kind,
        source_path,
        format: TextFormat { encoding, bom, eol },
        file_stamp,
        title: get_str(h, "title")?.to_owned(),
        doc_version: Version::from_raw(get_u64(h, "doc_version")?),
        created_ms: get_u64(h, "created_ms")?,
        updated_ms: get_u64(h, "updated_ms")?,
        owner_pid: u32::try_from(get_u64(h, "owner_pid")?).map_err(|_| corrupt("`owner_pid`"))?,
        body_len,
    };
    Ok((meta, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta(id: &str) -> Meta {
        Meta {
            id: RecoveryId::parse(id).unwrap(),
            kind: RecoveryKind::File,
            source_path: Some(PathBuf::from("C:\\work\\メモ.txt")),
            format: TextFormat {
                encoding: Encoding::Cp932,
                bom: false,
                eol: Eol::Lf,
            },
            file_stamp: Some(FileStamp {
                len: 12,
                modified: Some(UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_700)),
            }),
            title: "メモ.txt".into(),
            doc_version: Version::from_raw(7),
            created_ms: 10,
            updated_ms: 20,
            owner_pid: 99,
            body_len: 0,
        }
    }

    #[test]
    fn round_trip_keeps_everything() {
        let text = "日本語\nline2\r\n\u{1F600}";
        let mut meta = sample_meta("a-1-0");
        let bytes = encode(&meta, text);
        meta.body_len = text.len() as u64;
        let (got, body) = decode(&bytes).unwrap();
        assert_eq!(got, meta);
        assert_eq!(body, text);
    }

    #[test]
    fn empty_body_and_no_stamp() {
        let mut meta = sample_meta("a-1-1");
        meta.file_stamp = None;
        meta.source_path = None;
        let (got, body) = decode(&encode(&meta, "")).unwrap();
        assert_eq!(body, "");
        assert_eq!(got.file_stamp, None);
        assert_eq!(got.source_path, None);
    }

    #[test]
    fn truncation_at_every_point_is_detected() {
        let bytes = encode(&sample_meta("a-1-2"), "hello\nworld");
        for cut in 0..bytes.len() {
            assert!(
                matches!(decode(&bytes[..cut]), Err(DecodeError::Corrupt(_))),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn flipped_body_byte_is_detected() {
        let mut bytes = encode(&sample_meta("a-1-3"), "hello");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(matches!(decode(&bytes), Err(DecodeError::Corrupt(_))));
    }

    #[test]
    fn unknown_version_is_reported_as_such() {
        let bytes = encode(&sample_meta("a-1-4"), "x");
        let s = String::from_utf8(bytes)
            .unwrap()
            .replacen("\"version\":1", "\"version\":2", 1);
        assert_eq!(decode(s.as_bytes()), Err(DecodeError::UnknownVersion(2)));
    }

    #[test]
    fn garbage_is_corrupt() {
        for g in [&b""[..], b"hello", b"{}\n", b"[1]\nx", b"\xff\xfe\n"] {
            assert!(matches!(decode(g), Err(DecodeError::Corrupt(_))));
        }
    }

    #[test]
    fn ids_are_file_name_safe() {
        assert!(RecoveryId::parse("1f2-abc-0").is_some());
        for bad in ["", "A", "a/b", "a.b", "a b", "..", &"a".repeat(65)] {
            assert!(RecoveryId::parse(bad).is_none(), "{bad}");
        }
        let a = RecoveryId::generate();
        assert_ne!(a, RecoveryId::generate());
        assert!(RecoveryId::parse(a.as_str()).is_some());
    }
}
