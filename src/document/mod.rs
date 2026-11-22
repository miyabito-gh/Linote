//! 版付き文書モデル（タスク C0）。GUI に依存しない。
//!
//! - [`DocumentKind`] — 文書の種類（通常／巨大読み取り専用／新規未保存／メモ／クリップボード由来）
//! - [`Capability`]／[`Capabilities`] — 文書の機能（編集・Undo・保存などの可否）
//! - [`DocumentState`] — 開いている文書 1 つの状態（ID、版、保存済みの版、形式、パスなど）
//! - [`buffer`] — 本文へのアクセス（[`TextSource`]、[`EditTarget`]、位置と範囲）
//!
//! **本文はここに持たない。** 通常編集の本文は Scintilla が持ち、巨大モードの本文は
//! ファイル上にある。Rust 側は全文の写しを常時保持しない。バックグラウンド処理に渡す
//! ときだけ、必要な範囲の写しを一時的に作る。
//!
//! 版を進めるのは UI（タスク B）: Scintilla の `SCN_MODIFIED`
//! （`SC_MOD_INSERTTEXT`／`SC_MOD_DELETETEXT`）を受けるたびに
//! [`DocumentState::record_edit`] を呼ぶ。Undo/Redo による変更でも進む。

pub mod buffer;

pub use buffer::{EditTarget, LineCount, MemoryText, Selection, TextRange, TextSource};

use std::path::{Path, PathBuf};

use crate::core::{Actor, DocStamp, DocumentId, Error, MemoId, Result, TargetRef, Version};
use crate::file::FileStamp;
use crate::text::TextFormat;

/// 文書の種類。機能（[`Capabilities`]）の基本はここから決まる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentKind {
    /// 通常のファイル。Scintilla で編集する。
    File,
    /// 巨大ファイルの読み取り専用モード（UTF-8）。編集・保存・Undo はできない。
    LargeFile,
    /// 外部で変更されたファイルを確認するための、保存先を持たない読み取り専用の控え。
    SavedCopy(DocumentId),
    /// 新規作成した未保存の文書。保存するとパスが決まり [`File`](Self::File) になる。
    Untitled,
    /// 一時メモ。メモの保存領域に自動保存される。
    Memo(MemoId),
    /// グローバルショートカットでクリップボードから取り込んだ未保存の文書。
    /// 扱いは [`Untitled`](Self::Untitled) と同じで、由来の表示だけが違う。
    Clipboard,
}

impl DocumentKind {
    /// この種類の文書が基本として持つ機能。
    pub fn capabilities(&self) -> Capabilities {
        use Capability::*;
        let editing = Capabilities::of(&[Edit, Undo, SaveAs, Highlight, Wrap, Preview, Print]);
        match self {
            DocumentKind::File => editing.with(Save).with(Recover),
            DocumentKind::LargeFile => Capabilities::NONE,
            DocumentKind::SavedCopy(_) => Capabilities::of(&[Highlight, Wrap, Preview, Print]),
            DocumentKind::Untitled | DocumentKind::Clipboard => editing.with(Recover),
            DocumentKind::Memo(_) => editing.with(AutoSave),
        }
    }

    /// 記録・中継で使う安定した名前。
    pub const fn name(&self) -> &'static str {
        match self {
            DocumentKind::File => "file",
            DocumentKind::LargeFile => "large_file",
            DocumentKind::SavedCopy(_) => "saved_copy",
            DocumentKind::Untitled => "untitled",
            DocumentKind::Memo(_) => "memo",
            DocumentKind::Clipboard => "clipboard",
        }
    }
}

/// 文書の機能。コマンドの実行可能条件に使う。
///
/// 行ジャンプ、範囲コピー、検索は全種類の文書で使えるので機能として挙げない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Capability {
    /// 本文を変更できる（入力、貼り付け、置換、AI の編集）。
    Edit = 0,
    /// Undo／Redo ができる。
    Undo = 1,
    /// 現在のパスへ上書き保存できる。
    Save = 2,
    /// 別のパスへ書き出せる（名前を付けて保存、メモの書き出し）。
    SaveAs = 3,
    /// 未保存変更の復元データを書き出す対象である。
    Recover = 4,
    /// メモとして自動保存される。
    AutoSave = 5,
    /// 構文着色を使える。
    Highlight = 6,
    /// 折り返し表示を使える。
    Wrap = 7,
    /// Markdown プレビューの元にできる。
    Preview = 8,
    /// 印刷できる（編集欄で表示している文書。巨大ファイルは含まない）。
    Print = 9,
}

impl Capability {
    pub const ALL: [Capability; 10] = [
        Capability::Edit,
        Capability::Undo,
        Capability::Save,
        Capability::SaveAs,
        Capability::Recover,
        Capability::AutoSave,
        Capability::Highlight,
        Capability::Wrap,
        Capability::Preview,
        Capability::Print,
    ];

    /// 機械可読な安定した名前。
    pub const fn name(self) -> &'static str {
        match self {
            Capability::Edit => "edit",
            Capability::Undo => "undo",
            Capability::Save => "save",
            Capability::SaveAs => "save_as",
            Capability::Recover => "recover",
            Capability::AutoSave => "auto_save",
            Capability::Highlight => "highlight",
            Capability::Wrap => "wrap",
            Capability::Preview => "preview",
            Capability::Print => "print",
        }
    }
}

/// [`Capability`] の集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Capabilities(u16);

impl Capabilities {
    pub const NONE: Capabilities = Capabilities(0);

    /// 定数として書ける構築。例: `Capabilities::of(&[Capability::Edit])`。
    pub const fn of(list: &[Capability]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < list.len() {
            bits |= 1 << list[i] as u8;
            i += 1;
        }
        Capabilities(bits)
    }

    #[must_use]
    pub const fn with(self, capability: Capability) -> Self {
        Capabilities(self.0 | 1 << capability as u8)
    }

    #[must_use]
    pub const fn without(self, capability: Capability) -> Self {
        Capabilities(self.0 & !(1 << capability as u8))
    }

    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & (1 << capability as u8) != 0
    }

    /// `required` をすべて含むか。
    pub const fn contains_all(self, required: Capabilities) -> bool {
        self.0 & required.0 == required.0
    }

    /// `required` のうち、この集合に欠けている最初の機能。
    pub fn first_missing(self, required: Capabilities) -> Option<Capability> {
        Capability::ALL
            .into_iter()
            .find(|&c| required.contains(c) && !self.contains(c))
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |&c| self.contains(c))
    }
}

/// 開いている文書 1 つの状態。UI スレッドが所有し、UI スレッドだけが変更する。
///
/// バックグラウンド処理には、この構造体ではなく [`DocStamp`] と必要な値の写しを渡す。
#[derive(Debug, Clone)]
pub struct DocumentState {
    id: DocumentId,
    kind: DocumentKind,
    path: Option<PathBuf>,
    version: Version,
    saved_version: Option<Version>,
    disk: Option<FileStamp>,
    external_change: bool,
    last_actor: Actor,
    ai_edited: bool,
    read_only: bool,
    /// ファイルの形式。再読み込みや利用者の指定で変わる（本文の版は進めない。
    /// 未保存扱いにしたい場合は呼び出し側が [`record_edit`](Self::record_edit) を呼ぶ）。
    pub format: TextFormat,
}

impl DocumentState {
    /// ディスク上のファイルを開いた文書（通常または巨大モード）。
    /// 開いた直後は未変更（保存済みの版 = 現在の版）。
    pub fn open_file(path: PathBuf, large: bool, format: TextFormat, disk: FileStamp) -> Self {
        let kind = if large {
            DocumentKind::LargeFile
        } else {
            DocumentKind::File
        };
        let mut state = Self::new(kind, format);
        state.path = Some(path);
        state.saved_version = Some(Version::INITIAL);
        state.disk = Some(disk);
        state
    }

    /// ファイルに保存されている内容を別タブで確認するための控え。
    pub fn saved_copy(source: DocumentId, format: TextFormat, disk: FileStamp) -> Self {
        let mut state = Self::new(DocumentKind::SavedCopy(source), format);
        state.saved_version = Some(Version::INITIAL);
        state.disk = Some(disk);
        state.read_only = true;
        state
    }

    /// 空の新規文書。空のまま閉じても確認がいらないよう、作成直後は未変更とする。
    pub fn untitled() -> Self {
        let mut state = Self::new(DocumentKind::Untitled, TextFormat::DEFAULT);
        state.saved_version = Some(Version::INITIAL);
        state
    }

    /// クリップボードから取り込んだ文書。内容がどこにも保存されていないので、
    /// 作成直後から未保存として扱う。
    pub fn clipboard() -> Self {
        Self::new(DocumentKind::Clipboard, TextFormat::DEFAULT)
    }

    /// メモを開いた文書。開いた直後は未変更。
    pub fn memo(id: MemoId) -> Self {
        let mut state = Self::new(DocumentKind::Memo(id), TextFormat::DEFAULT);
        state.saved_version = Some(Version::INITIAL);
        state
    }

    /// 無題・クリップボード由来の文書を、保存したメモ（`id`）のタブに切り替える。`version` は
    /// メモに書いた時点の本文の版（それまでを保存済みとする）。ファイルなど、ほかの種類の
    /// 文書では何もせず `false`。
    pub fn become_memo(&mut self, id: MemoId, version: Version) -> bool {
        if !matches!(self.kind, DocumentKind::Untitled | DocumentKind::Clipboard) {
            return false;
        }
        debug_assert!(version <= self.version);
        self.kind = DocumentKind::Memo(id);
        self.path = None;
        self.disk = None;
        self.external_change = false;
        self.saved_version = Some(version);
        true
    }

    /// 復元データから開いた文書に使う: 現在の内容はどこにも保存されていない。
    pub fn mark_unsaved(&mut self) {
        self.saved_version = None;
    }

    fn new(kind: DocumentKind, format: TextFormat) -> Self {
        Self {
            id: DocumentId::allocate(),
            kind,
            path: None,
            version: Version::INITIAL,
            saved_version: None,
            disk: None,
            external_change: false,
            last_actor: Actor::User,
            ai_edited: false,
            read_only: false,
            format,
        }
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn kind(&self) -> &DocumentKind {
        &self.kind
    }

    /// ディスク上のパス。未保存の文書とメモでは `None`。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// 現在の「文書 ID＋版」。バックグラウンド処理を始めるときに控える。
    pub fn stamp(&self) -> DocStamp {
        DocStamp::new(self.id, self.version)
    }

    /// 最後に保存した（または開いた）ときのディスク上の状態。
    pub fn disk_stamp(&self) -> Option<FileStamp> {
        self.disk
    }

    /// 本文が変わったことを記録し、版を進める。新しい版を返す。
    ///
    /// UI が Scintilla の変更通知ごとに呼ぶ。`actor` は、その変更を起こした
    /// コマンドの実行者（コマンド実行中でなければ [`Actor::User`]）。
    pub fn record_edit(&mut self, actor: Actor) -> Version {
        self.version = self.version.next();
        self.last_actor = actor;
        if actor == Actor::Ai {
            self.ai_edited = true;
        }
        self.version
    }

    /// 最後に本文を変更した主体。
    pub fn last_actor(&self) -> Actor {
        self.last_actor
    }

    /// 利用者がまだ確認していない AI の変更があるか（タブ・ステータスの表示用）。
    pub fn has_ai_edits(&self) -> bool {
        self.ai_edited
    }

    /// 利用者が AI の変更を確認した（表示を消す）。
    pub fn acknowledge_ai_edits(&mut self) {
        self.ai_edited = false;
    }

    /// 未保存の変更があるか。
    pub fn is_dirty(&self) -> bool {
        self.saved_version != Some(self.version)
    }

    /// `version` の内容を保存し終えたことを記録する。
    ///
    /// 保存はバックグラウンドで行うので、終わった時点で版が進んでいることがある。
    /// その場合も `version` を保存済みとして記録し、文書は未保存のままになる。
    /// 未保存の文書を保存した場合は種類が [`DocumentKind::File`] になる。
    /// メモの自動保存では `path`・`disk` に `None` を渡す。
    pub fn record_saved(
        &mut self,
        version: Version,
        path: Option<PathBuf>,
        disk: Option<FileStamp>,
    ) {
        debug_assert!(version <= self.version);
        self.saved_version = Some(version);
        if let Some(path) = path {
            self.path = Some(path);
            self.disk = disk;
            self.external_change = false;
            if matches!(self.kind, DocumentKind::Untitled | DocumentKind::Clipboard) {
                self.kind = DocumentKind::File;
            }
        }
    }

    /// 現在の内容が保存済みの内容と同じになった（Undo で保存時点に戻った等。
    /// Scintilla の `SCN_SAVEPOINTREACHED`）。版は戻さず、現在の版を保存済みとする。
    pub fn record_unmodified(&mut self) {
        self.saved_version = Some(self.version);
    }

    /// 元ファイルの外部変更を検知した。解消するまで上書き保存を止める。
    pub fn record_external_change(&mut self) {
        self.external_change = true;
    }

    /// 外部変更を利用者が解消した（再読み込みした、または上書きを明示的に選んだ）。
    /// `disk` は解消時点のディスク上の状態。
    pub fn resolve_external_change(&mut self, disk: Option<FileStamp>) {
        self.external_change = false;
        self.disk = disk;
    }

    pub fn has_external_change(&self) -> bool {
        self.external_change
    }

    /// 読み取り専用で開いた文書か（起動オプション `-r`、V8）。
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// 読み取り専用にする（編集・Undo・上書き保存ができなくなる。別名での保存はできる）。
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// 現在使える機能。種類の基本の機能から、状態による制限を引いたもの。
    ///
    /// - 外部変更を検知している間は上書き保存（`Save`）ができない
    ///   （別名での保存はできる）。
    /// - 読み取り専用の間は、編集・Undo・上書き保存ができない。
    pub fn capabilities(&self) -> Capabilities {
        let mut caps = self.kind.capabilities();
        if self.read_only {
            caps = caps
                .without(Capability::Edit)
                .without(Capability::Undo)
                .without(Capability::Save);
        }
        if self.external_change {
            caps = caps.without(Capability::Save);
        }
        caps
    }

    /// 機能がなければ [`Error::Unsupported`]。
    pub fn require(&self, capability: Capability) -> Result<()> {
        if self.capabilities().contains(capability) {
            Ok(())
        } else {
            Err(Error::unsupported(capability.name()))
        }
    }

    /// 前提の版が現在の版と同じか確かめる。違えば現在の版を載せた
    /// [`Error::VersionMismatch`]。
    pub fn check_version(&self, expected: Version) -> Result<()> {
        if expected == self.version {
            Ok(())
        } else {
            Err(Error::VersionMismatch {
                target: TargetRef::Document(self.id),
                expected,
                actual: self.version,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(len: u64) -> FileStamp {
        FileStamp {
            len,
            modified: None,
            id: None,
        }
    }

    #[test]
    fn capabilities_follow_the_kind() {
        use Capability::*;
        let file = DocumentKind::File.capabilities();
        assert!(file.contains_all(Capabilities::of(&[Edit, Undo, Save, SaveAs, Recover])));
        assert!(!file.contains(AutoSave));

        assert_eq!(DocumentKind::LargeFile.capabilities(), Capabilities::NONE);
        assert!(!DocumentKind::LargeFile.capabilities().contains(Print));
        assert!(file.contains(Print));

        for kind in [DocumentKind::Untitled, DocumentKind::Clipboard] {
            let caps = kind.capabilities();
            assert!(caps.contains_all(Capabilities::of(&[Edit, Undo, SaveAs, Recover])));
            assert!(!caps.contains(Save));
        }

        let memo = DocumentKind::Memo(MemoId::generate()).capabilities();
        assert!(memo.contains_all(Capabilities::of(&[Edit, Undo, AutoSave, SaveAs])));
        assert!(!memo.contains(Save) && !memo.contains(Recover));
    }

    #[test]
    fn read_only_removes_edit_undo_and_save_but_keeps_save_as() {
        use Capability::*;
        let mut doc =
            DocumentState::open_file("a.txt".into(), false, TextFormat::DEFAULT, stamp(1));
        assert!(doc
            .capabilities()
            .contains_all(Capabilities::of(&[Edit, Undo, Save])));
        doc.set_read_only(true);
        assert!(doc.is_read_only());
        let caps = doc.capabilities();
        assert!(!caps.contains(Edit) && !caps.contains(Undo) && !caps.contains(Save));
        assert!(caps.contains(SaveAs) && caps.contains(Print));
        doc.set_read_only(false);
        assert!(doc.capabilities().contains(Edit));
    }

    #[test]
    fn capability_set_operations() {
        use Capability::*;
        let set = Capabilities::of(&[Edit, Save]);
        assert_eq!(
            set.first_missing(Capabilities::of(&[Edit, Undo, Save])),
            Some(Undo)
        );
        assert_eq!(set.first_missing(Capabilities::of(&[Edit])), None);
        assert_eq!(set.without(Save).iter().collect::<Vec<_>>(), vec![Edit]);
        assert!(Capabilities::NONE.contains_all(Capabilities::NONE));
    }

    #[test]
    fn edits_advance_the_version_and_make_the_document_dirty() {
        let mut doc =
            DocumentState::open_file("a.txt".into(), false, TextFormat::DEFAULT, stamp(1));
        assert!(!doc.is_dirty());
        let before = doc.stamp();
        let v1 = doc.record_edit(Actor::User);
        assert!(v1 > before.version);
        assert!(doc.is_dirty());
        assert!(!before.is_current(Some(doc.version())));
        assert!(doc.check_version(v1).is_ok());
        match doc.check_version(before.version) {
            Err(Error::VersionMismatch {
                expected,
                actual,
                target,
            }) => {
                assert_eq!((expected, actual), (before.version, v1));
                assert_eq!(target, TargetRef::Document(doc.id()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_save_that_finishes_after_more_edits_leaves_the_document_dirty() {
        let mut doc =
            DocumentState::open_file("a.txt".into(), false, TextFormat::DEFAULT, stamp(1));
        let saving = doc.record_edit(Actor::User);
        doc.record_edit(Actor::User);
        doc.record_saved(saving, Some("a.txt".into()), Some(stamp(2)));
        assert!(doc.is_dirty());
        assert_eq!(doc.disk_stamp(), Some(stamp(2)));
        doc.record_saved(doc.version(), Some("a.txt".into()), Some(stamp(3)));
        assert!(!doc.is_dirty());
        // Undo で保存時点に戻った場合も、版は進んだまま未変更になる。
        let v = doc.record_edit(Actor::User);
        doc.record_unmodified();
        assert!(!doc.is_dirty());
        assert_eq!(doc.version(), v);
    }

    #[test]
    fn saving_an_unsaved_document_turns_it_into_a_file() {
        let empty = DocumentState::untitled();
        assert!(!empty.is_dirty());

        let mut doc = DocumentState::clipboard();
        assert!(doc.is_dirty());
        assert!(doc.require(Capability::Save).is_err());
        doc.record_saved(doc.version(), Some("b.txt".into()), Some(stamp(5)));
        assert_eq!(doc.kind(), &DocumentKind::File);
        assert_eq!(doc.path(), Some(Path::new("b.txt")));
        assert!(doc.require(Capability::Save).is_ok());
        assert!(!doc.is_dirty());
    }

    #[test]
    fn memo_autosave_keeps_the_kind() {
        let id = MemoId::generate();
        let mut doc = DocumentState::memo(id.clone());
        let v = doc.record_edit(Actor::User);
        doc.record_saved(v, None, None);
        assert!(!doc.is_dirty());
        assert_eq!(doc.kind(), &DocumentKind::Memo(id));
        assert_eq!(doc.path(), None);
    }

    #[test]
    fn external_change_blocks_overwrite_until_resolved() {
        let mut doc =
            DocumentState::open_file("a.txt".into(), false, TextFormat::DEFAULT, stamp(1));
        doc.record_external_change();
        assert!(matches!(
            doc.require(Capability::Save),
            Err(Error::Unsupported { .. })
        ));
        assert!(doc.require(Capability::SaveAs).is_ok());
        assert!(doc.require(Capability::Edit).is_ok());
        doc.resolve_external_change(Some(stamp(9)));
        assert!(doc.require(Capability::Save).is_ok());
        assert_eq!(doc.disk_stamp(), Some(stamp(9)));
    }

    #[test]
    fn large_files_are_read_only() {
        let doc = DocumentState::open_file("big.log".into(), true, TextFormat::DEFAULT, stamp(1));
        for cap in Capability::ALL {
            assert!(doc.require(cap).is_err(), "{cap:?}");
        }
    }

    #[test]
    fn ai_edits_are_tracked_until_acknowledged() {
        let mut doc = DocumentState::untitled();
        doc.record_edit(Actor::Ai);
        doc.record_edit(Actor::User);
        assert_eq!(doc.last_actor(), Actor::User);
        assert!(doc.has_ai_edits());
        doc.acknowledge_ai_edits();
        assert!(!doc.has_ai_edits());
        let mut restored =
            DocumentState::open_file("a".into(), false, TextFormat::DEFAULT, stamp(0));
        restored.mark_unsaved();
        assert!(restored.is_dirty());
    }
}
