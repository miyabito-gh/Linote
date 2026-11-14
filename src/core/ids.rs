//! 識別子と版。

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 開いている文書（タブ）の識別子。
///
/// プロセス内で一意で、再利用しない。プロセスをまたいで保存しない
/// （復元データの永続的な識別子はタスク D が `recovery` で定義する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u64);

static NEXT_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);

impl DocumentId {
    /// 新しい一意の ID を払い出す。どのスレッドから呼んでもよい。
    pub fn allocate() -> Self {
        Self(NEXT_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// 外部（将来の MCP など）から受け取った数値を ID に戻す。
    /// 実在するかどうかは呼び出し側が文書一覧で確かめる。
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "doc:{}", self.0)
    }
}

/// 文書またはメモの内容の版。内容が変わるたびに単調に増える。戻らない。
///
/// - 文書: 利用者の編集・AI の編集・Undo/Redo のいずれでも進む
///   （UI が Scintilla の `SCN_MODIFIED` を受けて進める）。
/// - メモ: 保存された内容が変わるたびに進む（タスク E）。
///
/// バックグラウンド処理の結果や書き込み要求は、この版を照合してから反映する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Version(u64);

impl Version {
    /// 開いた直後・作成直後の版。
    pub const INITIAL: Version = Version(0);

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// 次の版。
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// 「どの文書の、どの版か」。バックグラウンド処理を始めるときに控えておき、
/// 結果を反映する直前に現在の状態と照合する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocStamp {
    pub document: DocumentId,
    pub version: Version,
}

impl DocStamp {
    pub const fn new(document: DocumentId, version: Version) -> Self {
        Self { document, version }
    }

    /// `current` は、その文書の現在の版（閉じられていれば `None`）。
    /// 文書が開いたままで版も同じときだけ `true`。
    pub fn is_current(&self, current: Option<Version>) -> bool {
        current == Some(self.version)
    }
}

/// 一時メモの識別子。プロセスをまたいで永続する。
///
/// ファイル名にそのまま使えるよう、英小文字・数字・`-` だけの 1〜64 文字に限る。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemoId(String);

static MEMO_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

impl MemoId {
    pub const MAX_LEN: usize = 64;

    /// 文字列を検証して ID にする。使えない文字や長さなら `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let ok = !s.is_empty()
            && s.len() <= Self::MAX_LEN
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| Self(s.to_owned()))
    }

    /// 新しい ID を作る。時刻（ナノ秒）・プロセス ID・プロセス内の連番から作るので、
    /// 別プロセスが同時に作っても実用上衝突しない。それでも保存側（タスク E）は
    /// 「新規作成でのみ成功する」方法でファイルを作り、衝突を検出すること。
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = MEMO_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("{nanos:x}-{:x}-{seq:x}", std::process::id()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 版を持つ対象（文書またはメモ）への参照。コマンドの対象と前提条件に使う。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetRef {
    Document(DocumentId),
    Memo(MemoId),
}

impl fmt::Display for TargetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetRef::Document(id) => write!(f, "{id}"),
            TargetRef::Memo(id) => write!(f, "memo:{id}"),
        }
    }
}

/// 操作を行った主体。コマンドの実行と文書の変更に必ず付けて記録する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Actor {
    /// 利用者（メニュー、ショートカット、キー入力）。
    User,
    /// AI クライアント（将来の MCP 経由）。
    Ai,
}

impl Actor {
    /// 記録・中継で使う安定した名前。
    pub const fn as_str(self) -> &'static str {
        match self {
            Actor::User => "user",
            Actor::Ai => "ai",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_unique_and_increasing() {
        let a = DocumentId::allocate();
        let b = DocumentId::allocate();
        assert!(b > a);
        assert_eq!(DocumentId::from_raw(a.as_u64()), a);
    }

    #[test]
    fn version_only_moves_forward() {
        let v = Version::INITIAL;
        assert!(v.next() > v);
        assert_eq!(v.next().next().as_u64(), 2);
        assert_eq!(v.next().to_string(), "v1");
    }

    #[test]
    fn stamp_matches_only_same_open_version() {
        let stamp = DocStamp::new(DocumentId::allocate(), Version::from_raw(3));
        assert!(stamp.is_current(Some(Version::from_raw(3))));
        assert!(!stamp.is_current(Some(Version::from_raw(4))));
        assert!(!stamp.is_current(None));
    }

    #[test]
    fn memo_id_accepts_only_file_name_safe_text() {
        assert!(MemoId::parse("abc-123").is_some());
        for bad in ["", "A", "a/b", "a.b", "..", "a b", "メモ", &"a".repeat(65)] {
            assert!(MemoId::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn generated_memo_ids_are_valid_and_distinct() {
        let a = MemoId::generate();
        let b = MemoId::generate();
        assert_ne!(a, b);
        assert_eq!(MemoId::parse(a.as_str()), Some(a));
    }
}
