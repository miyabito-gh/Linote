//! コマンド: すべての操作の唯一の入口（タスク C0）。GUI に依存しない。
//!
//! メニュー、ショートカット、将来の MCP（AI）は、どれも [`Invocation`] を作って
//! [`Registry::execute`] を呼ぶ。実行の前に共通の検査（実行者、引数、対象、版の前提、
//! 文書の機能）を通し、AI の編集は Undo の 1 単位にまとめ、結果を記録する。
//!
//! - [`CommandDef`] — 機械可読な定義（名前、説明、引数、実行可能条件、結果）。
//!   静的なデータで、GUI を初期化しなくても一覧できる。
//! - [`Registry`] — 定義と処理（ハンドラー）の登録表。登録の呼び出しは統括が管理する。
//!   各モジュールは `pub fn register(registry: &mut Registry<H>)` の形で公開する。
//! - [`CommandHost`] — 登録表がアプリの状態を調べるための最小の窓口。GUI では `ui` の
//!   アプリ状態が実装する。
//! - [`keys`] — キー割り当ての表記（設定ファイルと既定値で共通）。
//!
//! 長時間かかるコマンド（Grep など）のハンドラーは、バックグラウンド処理を開始して
//! すぐ戻る。結果は [`crate::core::background`] の通路で UI に届く。

pub mod keys;

pub use keys::{Key, KeyChord, Modifiers};

use std::collections::BTreeMap;
use std::fmt;

use crate::core::{Actor, DocumentId, Error, Result, TargetRef, Version};
use crate::document::{Capabilities, Capability};

/// コマンドの引数と結果の値（JSON）。将来の MCP にそのまま載せられる形。
pub type Value = serde_json::Value;

/// コマンドの名前。`"file.save"` のように `領域.動作` の形で、英小文字・数字・`_`・`.`
/// だけを使う。設定ファイルのキー割り当てと将来の MCP の操作名にもなるので、
/// 公開後は変えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(&'static str);

impl CommandId {
    /// 形式が不正ならコンパイル時（定数の評価時）にパニックする。
    pub const fn new(name: &'static str) -> Self {
        assert!(Self::is_valid(name), "invalid command id");
        Self(name)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }

    const fn is_valid(name: &str) -> bool {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes[0] == b'.' || bytes[bytes.len() - 1] == b'.' {
            return false;
        }
        let mut dots = 0;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'.' {
                dots += 1;
                if bytes[i - 1] == b'.' {
                    return false;
                }
            } else if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
                return false;
            }
            i += 1;
        }
        dots >= 1
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// 引数の型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    String,
    /// 0 以上の整数（位置、行番号、件数）。
    Integer,
    Boolean,
    /// 列挙した文字列のいずれか。
    Enum(&'static [&'static str]),
}

/// 引数 1 つの定義。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    pub name: &'static str,
    pub ty: ParamType,
    pub required: bool,
    pub description: &'static str,
}

impl ParamDef {
    pub const fn required(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name,
            ty,
            required: true,
            description,
        }
    }

    pub const fn optional(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name,
            ty,
            required: false,
            description,
        }
    }

    fn check(&self, value: &Value) -> Result<()> {
        let ok = match self.ty {
            ParamType::String => value.is_string(),
            ParamType::Integer => value.is_u64(),
            ParamType::Boolean => value.is_boolean(),
            ParamType::Enum(choices) => value.as_str().is_some_and(|s| choices.contains(&s)),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::invalid_argument(self.name, self.description))
        }
    }

    fn schema(&self) -> Value {
        let mut schema = match self.ty {
            ParamType::String => serde_json::json!({ "type": "string" }),
            ParamType::Integer => serde_json::json!({ "type": "integer", "minimum": 0 }),
            ParamType::Boolean => serde_json::json!({ "type": "boolean" }),
            ParamType::Enum(choices) => serde_json::json!({ "type": "string", "enum": choices }),
        };
        schema["description"] = self.description.into();
        schema
    }
}

/// コマンドが対象にするもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// 特定の文書・メモを対象にしない（新規作成、ファイルを開く、Grep など）。
    None,
    /// 文書。利用者の操作では、指定がなければアクティブな文書。
    Document,
    /// メモ（保存領域上のメモ。開いているタブではない）。
    Memo,
}

/// コマンドが何を変えるか。AI に必要な許可範囲と、版の前提が必須かどうかを決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// 画面の状態だけを変える（ダイアログを開く、表示の切り替え、移動）。
    View,
    /// データを読み取って返す。
    Read,
    /// 文書またはメモの内容を変える。
    Edit,
    /// ディスク上のファイルを保存・削除する。AI からは操作ごとに GUI で確認する。
    Disk,
}

/// AI（将来の MCP）に必要な許可範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessScope {
    Read,
    Write,
}

impl AccessScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            AccessScope::Read => "read",
            AccessScope::Write => "write",
        }
    }
}

/// 機械可読なコマンド定義。`const` で書けるよう、すべて静的なデータにしてある。
///
/// ```
/// use linote::command::*;
/// use linote::document::{Capabilities, Capability};
///
/// pub const GOTO_LINE: CommandDef = CommandDef::new(
///     CommandId::new("edit.goto_line"),
///     "行へ移動",
///     "指定した行（1 始まり）へカーソルを移動する。",
/// )
/// .target(TargetKind::Document)
/// .params(&[ParamDef::required("line", ParamType::Integer, "行番号（1 始まり）")])
/// .default_key("Ctrl+G");
/// assert!(GOTO_LINE.requires.contains_all(Capabilities::NONE));
/// assert!(!GOTO_LINE.requires.contains(Capability::Edit));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub id: CommandId,
    /// メニューなどに出す短い名前（日本語）。
    pub title: &'static str,
    /// 何をするかの説明。AI にも渡すので、引数と結果の意味が分かるように書く。
    pub description: &'static str,
    pub params: &'static [ParamDef],
    /// 結果（JSON）の形の説明。結果がなければ空。
    pub result: &'static str,
    pub target: TargetKind,
    /// 対象の文書に必要な機能。欠けていれば実行できない（メニューは無効表示）。
    pub requires: Capabilities,
    pub effect: Effect,
    /// AI から呼べるか。`false` なら利用者専用（ダイアログを開くだけの操作など）。
    pub ai_callable: bool,
    /// 利用者が実行したときも、1 回の実行を Undo の 1 単位にまとめるか
    /// （「すべて置換」など）。AI の編集は、この値によらず常に 1 単位にまとめる。
    pub single_undo: bool,
    /// 既定のキー割り当て（[`KeyChord`] の表記）。なければ `None`。
    /// 設定ファイルの割り当てが優先する。
    pub default_key: Option<&'static str>,
}

impl CommandDef {
    /// 既定値: 引数なし、対象なし、必要な機能なし、効果は [`Effect::View`]、
    /// 利用者専用、キー割り当てなし。
    pub const fn new(id: CommandId, title: &'static str, description: &'static str) -> Self {
        Self {
            id,
            title,
            description,
            params: &[],
            result: "",
            target: TargetKind::None,
            requires: Capabilities::NONE,
            effect: Effect::View,
            ai_callable: false,
            single_undo: false,
            default_key: None,
        }
    }

    pub const fn params(mut self, params: &'static [ParamDef]) -> Self {
        self.params = params;
        self
    }

    pub const fn result(mut self, result: &'static str) -> Self {
        self.result = result;
        self
    }

    pub const fn target(mut self, target: TargetKind) -> Self {
        self.target = target;
        self
    }

    pub const fn requires(mut self, requires: Capabilities) -> Self {
        self.requires = requires;
        self
    }

    pub const fn effect(mut self, effect: Effect) -> Self {
        self.effect = effect;
        self
    }

    pub const fn ai_callable(mut self) -> Self {
        self.ai_callable = true;
        self
    }

    pub const fn single_undo(mut self) -> Self {
        self.single_undo = true;
        self
    }

    pub const fn default_key(mut self, key: &'static str) -> Self {
        self.default_key = Some(key);
        self
    }

    /// AI がこのコマンドを呼ぶのに必要な許可範囲。AI から呼べなければ `None`。
    pub fn ai_scope(&self) -> Option<AccessScope> {
        self.ai_callable.then_some(match self.effect {
            Effect::View | Effect::Read => AccessScope::Read,
            Effect::Edit | Effect::Disk => AccessScope::Write,
        })
    }

    /// AI が呼んだとき、操作ごとに GUI で利用者の確認を取る必要があるか。
    pub fn needs_confirmation_from_ai(&self) -> bool {
        self.effect == Effect::Disk
    }

    /// AI が呼ぶとき、版の前提（[`Invocation::expected_version`]）が必須か。
    /// 文書・メモを対象にする書き込みはすべて必須。
    pub fn needs_version_from_ai(&self) -> bool {
        self.target != TargetKind::None && matches!(self.effect, Effect::Edit | Effect::Disk)
    }

    /// 引数を定義と照らして検査する。`null` は引数なしとして扱う。
    /// 定義にない引数、必須の引数の欠落、型の違いは [`Error::InvalidArgument`]。
    pub fn validate_args(&self, args: &Value) -> Result<()> {
        let empty = serde_json::Map::new();
        let map = match args {
            Value::Null => &empty,
            Value::Object(map) => map,
            _ => {
                return Err(Error::invalid_argument(
                    "args",
                    "オブジェクトではありません",
                ))
            }
        };
        for key in map.keys() {
            if !self.params.iter().any(|p| p.name == key) {
                return Err(Error::invalid_argument(key.as_str(), "定義にない引数です"));
            }
        }
        for param in self.params {
            match map.get(param.name) {
                Some(value) => param.check(value)?,
                None if param.required => {
                    return Err(Error::invalid_argument(
                        param.name,
                        "必須の引数がありません",
                    ))
                }
                None => {}
            }
        }
        Ok(())
    }

    /// 引数の JSON Schema。
    pub fn input_schema(&self) -> Value {
        let properties: serde_json::Map<String, Value> = self
            .params
            .iter()
            .map(|p| (p.name.to_owned(), p.schema()))
            .collect();
        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name)
            .collect();
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// 定義全体の機械可読な表現（将来の MCP の操作一覧、ヘルプ、検査用）。
    pub fn describe(&self) -> Value {
        serde_json::json!({
            "name": self.id.as_str(),
            "title": self.title,
            "description": self.description,
            "inputSchema": self.input_schema(),
            "result": self.result,
            "target": match self.target {
                TargetKind::None => "none",
                TargetKind::Document => "document",
                TargetKind::Memo => "memo",
            },
            "requires": self.requires.iter().map(Capability::name).collect::<Vec<_>>(),
            "effect": match self.effect {
                Effect::View => "view",
                Effect::Read => "read",
                Effect::Edit => "edit",
                Effect::Disk => "disk",
            },
            "aiScope": self.ai_scope().map(AccessScope::as_str),
            "needsVersion": self.needs_version_from_ai(),
            "needsConfirmation": self.needs_confirmation_from_ai(),
        })
    }
}

/// コマンドの実行要求。メニュー・ショートカット・AI のどれも、これを作って
/// [`Registry::execute`] に渡す。
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub command: CommandId,
    pub actor: Actor,
    /// 対象。利用者の操作で文書を対象にするコマンドは、`None` ならアクティブな文書。
    /// AI は必ず指定する。
    pub target: Option<TargetRef>,
    /// 版の前提。指定されていれば、対象の現在の版と同じときだけ実行する。
    /// AI の書き込みでは必須（[`CommandDef::needs_version_from_ai`]）。
    pub expected_version: Option<Version>,
    pub args: Value,
}

impl Invocation {
    /// 利用者の操作（メニュー、ショートカット）。引数なし、対象はアクティブな文書。
    pub fn user(command: CommandId) -> Self {
        Self {
            command,
            actor: Actor::User,
            target: None,
            expected_version: None,
            args: Value::Null,
        }
    }

    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    pub fn with_target(mut self, target: TargetRef, expected_version: Option<Version>) -> Self {
        self.target = Some(target);
        self.expected_version = expected_version;
        self
    }
}

/// ハンドラーに渡す、検査済みの呼び出し。
#[derive(Debug)]
pub struct Call<'a> {
    pub def: &'a CommandDef,
    pub actor: Actor,
    /// 解決済みの対象（アクティブな文書への読み替えを済ませたもの）。
    pub target: Option<TargetRef>,
    args: &'a Value,
}

impl<'a> Call<'a> {
    /// 対象の文書。定義の対象が [`TargetKind::Document`] なら必ず `Ok`。
    pub fn document(&self) -> Result<DocumentId> {
        match &self.target {
            Some(TargetRef::Document(id)) => Ok(*id),
            _ => Err(Error::Internal(format!(
                "{} は文書を対象にしていません",
                self.def.id
            ))),
        }
    }

    /// 対象のメモ。定義の対象が [`TargetKind::Memo`] なら必ず `Ok`。
    pub fn memo(&self) -> Result<&crate::core::MemoId> {
        match &self.target {
            Some(TargetRef::Memo(id)) => Ok(id),
            _ => Err(Error::Internal(format!(
                "{} はメモを対象にしていません",
                self.def.id
            ))),
        }
    }

    pub fn arg(&self, name: &str) -> Option<&'a Value> {
        self.args.get(name)
    }

    /// 必須の文字列引数。
    pub fn str(&self, name: &str) -> Result<&'a str> {
        self.opt_str(name)
            .ok_or_else(|| Error::invalid_argument(name, "必須の引数がありません"))
    }

    pub fn opt_str(&self, name: &str) -> Option<&'a str> {
        self.arg(name).and_then(Value::as_str)
    }

    /// 必須の整数引数。
    pub fn u64(&self, name: &str) -> Result<u64> {
        self.opt_u64(name)
            .ok_or_else(|| Error::invalid_argument(name, "必須の引数がありません"))
    }

    pub fn opt_u64(&self, name: &str) -> Option<u64> {
        self.arg(name).and_then(Value::as_u64)
    }

    /// 真偽値の引数。省略時は `default`。
    pub fn bool_or(&self, name: &str, default: bool) -> bool {
        self.arg(name).and_then(Value::as_bool).unwrap_or(default)
    }
}

/// コマンドの結果。
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    /// 結果の値（[`CommandDef::result`] に形を記述する）。なければ `null`。
    pub value: Value,
    /// 解決済みの対象。
    pub target: Option<TargetRef>,
    /// 実行後の対象の版。AI は次の書き込みの前提にこの版を使う。
    pub version: Option<Version>,
}

/// コマンド実行の記録 1 件。AI の操作はローカルの操作記録に残す
/// （`<データ>/mcp/operations.jsonl`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    pub command: CommandId,
    pub actor: Actor,
    pub target: Option<TargetRef>,
    pub version_before: Option<Version>,
    pub version_after: Option<Version>,
    /// 失敗した場合の [`Error::code`]。
    pub error: Option<&'static str>,
}

/// 登録表がアプリの状態を調べ、Undo をまとめ、記録を残すための窓口。
///
/// GUI では `ui` のアプリ状態が実装する。テストではメモリ上の状態が実装する。
pub trait CommandHost {
    /// アクティブな（前面のタブの）文書。
    fn active_document(&self) -> Option<DocumentId>;

    /// 対象の現在の版。存在しなければ [`Error::NotFound`]。
    fn target_version(&self, target: &TargetRef) -> Result<Version>;

    /// 文書が現在持つ機能（[`crate::document::DocumentState::capabilities`]）。
    fn document_capabilities(&self, document: DocumentId) -> Result<Capabilities>;

    /// 文書への変更を Undo の 1 単位にまとめ始める／終える。
    /// また、この間の変更は `actor` によるものとして版に記録する
    /// （[`crate::document::DocumentState::record_edit`] に渡す実行者）。
    fn begin_edit(&mut self, document: DocumentId, actor: Actor, single_undo: bool);

    fn end_edit(&mut self, document: DocumentId);

    /// 実行の記録。既定では何もしない。
    fn record(&mut self, _record: &CommandRecord) {}
}

/// コマンドの処理。`Call` は検査済み。
pub type Handler<H> = Box<dyn Fn(&mut H, &Call<'_>) -> Result<Value>>;

struct Entry<H> {
    def: CommandDef,
    handler: Handler<H>,
}

/// コマンドの登録表。`H` はアプリの状態の型。
pub struct Registry<H> {
    entries: BTreeMap<&'static str, Entry<H>>,
}

impl<H> Default for Registry<H> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<H: CommandHost> Registry<H> {
    pub fn new() -> Self {
        Self::default()
    }

    /// コマンドを登録する。同じ名前の二重登録、既定のキー表記の誤りは
    /// [`Error::Internal`]（起動時とテストで検出する）。
    pub fn register(
        &mut self,
        def: CommandDef,
        handler: impl Fn(&mut H, &Call<'_>) -> Result<Value> + 'static,
    ) -> Result<()> {
        if self.entries.contains_key(def.id.as_str()) {
            return Err(Error::Internal(format!("コマンド {} の二重登録", def.id)));
        }
        if let Some(key) = def.default_key {
            KeyChord::parse(key).map_err(|e| {
                Error::Internal(format!("コマンド {} の既定のキーが不正: {e}", def.id))
            })?;
        }
        self.entries.insert(
            def.id.as_str(),
            Entry {
                def,
                handler: Box::new(handler),
            },
        );
        Ok(())
    }

    /// 名前から定義を引く（外部から届いた名前の解決にも使う）。
    pub fn find(&self, name: &str) -> Option<&CommandDef> {
        self.entries.get(name).map(|e| &e.def)
    }

    /// 登録済みの定義を名前順に返す。
    pub fn definitions(&self) -> impl Iterator<Item = &CommandDef> {
        self.entries.values().map(|e| &e.def)
    }

    /// 今この呼び出しを実行できるか（メニューの有効・無効の表示にも使う）。
    /// 実行できなければ、その理由のエラーを返す。引数は検査しない。
    pub fn check_available(&self, host: &H, invocation: &Invocation) -> Result<()> {
        let entry = self.entry(invocation.command)?;
        self.resolve(&entry.def, host, invocation).map(|_| ())
    }

    /// コマンドを実行する。すべての操作がここを通る。
    ///
    /// 1. 実行者: AI から呼べないコマンドは [`Error::Denied`]。
    /// 2. 対象の解決（利用者の操作ではアクティブな文書）。
    /// 3. 版の前提の照合。AI の書き込みで前提がなければ [`Error::InvalidArgument`]、
    ///    食い違えば現在の版を載せた [`Error::VersionMismatch`]。
    /// 4. 文書の機能の検査（[`Error::Unsupported`]）。
    /// 5. 引数の検査。
    /// 6. 文書の編集なら [`CommandHost::begin_edit`]〜[`end_edit`](CommandHost::end_edit)
    ///    で囲んでハンドラーを呼ぶ（失敗しても必ず閉じる）。
    /// 7. 記録（[`CommandHost::record`]）。失敗も記録する。
    pub fn execute(&self, host: &mut H, invocation: &Invocation) -> Result<CommandOutput> {
        let entry = self.entry(invocation.command)?;
        let def = &entry.def;
        let (target, version_before) = match self.resolve(def, host, invocation) {
            Ok(resolved) => resolved,
            Err(e) => {
                host.record(&CommandRecord {
                    command: def.id,
                    actor: invocation.actor,
                    target: invocation.target.clone(),
                    version_before: None,
                    version_after: None,
                    error: Some(e.code()),
                });
                return Err(e);
            }
        };
        let call = Call {
            def,
            actor: invocation.actor,
            target: target.clone(),
            args: &invocation.args,
        };
        let result =
            def.validate_args(&invocation.args)
                .and_then(|()| match (&target, def.effect) {
                    (Some(TargetRef::Document(document)), Effect::Edit) => {
                        let single_undo = def.single_undo || invocation.actor == Actor::Ai;
                        host.begin_edit(*document, invocation.actor, single_undo);
                        let result = (entry.handler)(host, &call);
                        host.end_edit(*document);
                        result
                    }
                    _ => (entry.handler)(host, &call),
                });
        // 実行後の版。コマンドが対象を閉じた・削除した場合は `None`。
        let version_after = target.as_ref().and_then(|t| host.target_version(t).ok());
        host.record(&CommandRecord {
            command: def.id,
            actor: invocation.actor,
            target: target.clone(),
            version_before,
            version_after,
            error: result.as_ref().err().map(Error::code),
        });
        result.map(|value| CommandOutput {
            value,
            target,
            version: version_after,
        })
    }

    fn entry(&self, command: CommandId) -> Result<&Entry<H>> {
        self.entries
            .get(command.as_str())
            .ok_or_else(|| Error::not_found(format!("コマンド {command}")))
    }

    /// 実行者・対象・版・機能を検査し、解決済みの対象と現在の版を返す。
    fn resolve(
        &self,
        def: &CommandDef,
        host: &H,
        invocation: &Invocation,
    ) -> Result<(Option<TargetRef>, Option<Version>)> {
        let by_ai = invocation.actor == Actor::Ai;
        if by_ai && !def.ai_callable {
            return Err(Error::denied(format!("{} は利用者専用の操作です", def.id)));
        }
        let target = match (def.target, &invocation.target) {
            (TargetKind::None, None) => None,
            (TargetKind::Document, Some(t @ TargetRef::Document(_)))
            | (TargetKind::Memo, Some(t @ TargetRef::Memo(_))) => Some(t.clone()),
            (TargetKind::Document, None) if !by_ai => Some(TargetRef::Document(
                host.active_document()
                    .ok_or_else(|| Error::not_found("対象の文書"))?,
            )),
            (TargetKind::Document | TargetKind::Memo, None) => {
                return Err(Error::invalid_argument("target", "対象の指定が必要です"))
            }
            _ => {
                return Err(Error::invalid_argument(
                    "target",
                    "このコマンドの対象の種類と合いません",
                ))
            }
        };
        let Some(target) = target else {
            return Ok((None, None));
        };
        let actual = host.target_version(&target)?;
        match invocation.expected_version {
            Some(expected) if expected != actual => {
                return Err(Error::VersionMismatch {
                    target,
                    expected,
                    actual,
                })
            }
            None if by_ai && def.needs_version_from_ai() => {
                return Err(Error::invalid_argument(
                    "expected_version",
                    "書き込みには前提の版が必要です",
                ))
            }
            _ => {}
        }
        if let TargetRef::Document(document) = &target {
            let capabilities = host.document_capabilities(*document)?;
            if let Some(missing) = capabilities.first_missing(def.requires) {
                return Err(Error::unsupported(missing.name()));
            }
        }
        Ok((Some(target), Some(actual)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MemoId;
    use crate::document::{DocumentState, EditTarget, MemoryText, TextRange, TextSource};
    use serde_json::json;

    /// 文書 1 つとメモ 1 つを持つ、テスト用のアプリ状態。
    struct TestHost {
        state: DocumentState,
        text: MemoryText,
        editing_as: Option<Actor>,
        memo: (MemoId, Version),
        records: Vec<CommandRecord>,
        no_active: bool,
    }

    impl TestHost {
        fn new(state: DocumentState, text: &str) -> Self {
            Self {
                state,
                text: MemoryText::new(text),
                editing_as: None,
                memo: (MemoId::generate(), Version::from_raw(4)),
                records: Vec::new(),
                no_active: false,
            }
        }

        fn doc(&self) -> TargetRef {
            TargetRef::Document(self.state.id())
        }

        /// Scintilla の変更通知に相当: 本文を変え、版を進める。
        fn replace(&mut self, range: TextRange, text: &str) -> Result<()> {
            self.text.replace(range, text)?;
            self.state
                .record_edit(self.editing_as.unwrap_or(Actor::User));
            Ok(())
        }
    }

    impl CommandHost for TestHost {
        fn active_document(&self) -> Option<DocumentId> {
            (!self.no_active).then(|| self.state.id())
        }

        fn target_version(&self, target: &TargetRef) -> Result<Version> {
            match target {
                TargetRef::Document(id) if *id == self.state.id() => Ok(self.state.version()),
                TargetRef::Memo(id) if *id == self.memo.0 => Ok(self.memo.1),
                other => Err(Error::not_found(other.to_string())),
            }
        }

        fn document_capabilities(&self, _document: DocumentId) -> Result<Capabilities> {
            Ok(self.state.capabilities())
        }

        fn begin_edit(&mut self, _document: DocumentId, actor: Actor, single_undo: bool) {
            self.editing_as = Some(actor);
            if single_undo {
                self.text.begin_undo_group();
            }
        }

        fn end_edit(&mut self, _document: DocumentId) {
            self.editing_as = None;
            self.text.end_undo_group();
        }

        fn record(&mut self, record: &CommandRecord) {
            self.records.push(record.clone());
        }
    }

    const INSERT: CommandDef = CommandDef::new(
        CommandId::new("edit.insert"),
        "挿入",
        "指定した位置に文字列を挿入する。",
    )
    .target(TargetKind::Document)
    .requires(Capabilities::of(&[Capability::Edit]))
    .effect(Effect::Edit)
    .ai_callable()
    .params(&[
        ParamDef::required("position", ParamType::Integer, "挿入位置（バイト）"),
        ParamDef::required("text", ParamType::String, "挿入する文字列"),
        ParamDef::optional("twice", ParamType::Boolean, "2 回挿入する"),
    ]);

    const LENGTH: CommandDef = CommandDef::new(
        CommandId::new("document.length"),
        "長さ",
        "本文の長さ（バイト）を返す。",
    )
    .target(TargetKind::Document)
    .effect(Effect::Read)
    .ai_callable()
    .result("{ length: integer }");

    const SAVE: CommandDef =
        CommandDef::new(CommandId::new("file.save"), "上書き保存", "保存する。")
            .target(TargetKind::Document)
            .requires(Capabilities::of(&[Capability::Save]))
            .effect(Effect::Disk)
            .ai_callable()
            .default_key("Ctrl+S");

    const FIND_DIALOG: CommandDef =
        CommandDef::new(CommandId::new("search.show"), "検索", "検索欄を開く。")
            .default_key("Ctrl+F");

    const MEMO_RENAME: CommandDef = CommandDef::new(
        CommandId::new("memo.rename"),
        "名前変更",
        "メモの名前を変える。",
    )
    .target(TargetKind::Memo)
    .effect(Effect::Edit)
    .ai_callable()
    .params(&[ParamDef::required("name", ParamType::String, "新しい名前")]);

    fn registry() -> Registry<TestHost> {
        let mut r = Registry::new();
        r.register(INSERT, |host: &mut TestHost, call| {
            let position = call.u64("position")?;
            let text = call.str("text")?;
            assert_eq!(call.document()?, host.state.id());
            host.replace(TextRange::at(position), text)?;
            if call.bool_or("twice", false) {
                host.replace(TextRange::at(position), text)?;
            }
            Ok(Value::Null)
        })
        .unwrap();
        r.register(LENGTH, |host: &mut TestHost, _| {
            Ok(json!({ "length": host.text.len() }))
        })
        .unwrap();
        r.register(SAVE, |host: &mut TestHost, _| {
            let v = host.state.version();
            host.state.record_saved(v, None, None);
            Ok(Value::Null)
        })
        .unwrap();
        r.register(FIND_DIALOG, |_, _| Ok(Value::Null)).unwrap();
        r.register(MEMO_RENAME, |host: &mut TestHost, call| {
            assert_eq!(call.memo()?, &host.memo.0);
            host.memo.1 = host.memo.1.next();
            Ok(Value::Null)
        })
        .unwrap();
        r
    }

    fn file_host() -> TestHost {
        let state = DocumentState::open_file(
            "a.txt".into(),
            false,
            Default::default(),
            crate::file::FileStamp {
                len: 0,
                modified: None,
            },
        );
        TestHost::new(state, "abc")
    }

    fn ai(
        command: CommandId,
        target: TargetRef,
        version: Option<Version>,
        args: Value,
    ) -> Invocation {
        Invocation {
            command,
            actor: Actor::Ai,
            target: Some(target),
            expected_version: version,
            args,
        }
    }

    #[test]
    fn command_ids_must_be_dotted_lowercase() {
        for ok in ["file.save", "edit.goto_line", "a.b.c2"] {
            assert!(CommandId::is_valid(ok), "{ok}");
        }
        for bad in [
            "",
            "save",
            ".save",
            "file.",
            "file..save",
            "File.Save",
            "file save",
            "ファイル.保存",
        ] {
            assert!(!CommandId::is_valid(bad), "{bad}");
        }
    }

    #[test]
    fn user_command_targets_the_active_document() {
        let r = registry();
        let mut host = file_host();
        let inv = Invocation::user(INSERT.id).with_args(json!({ "position": 3, "text": "日本語" }));
        let out = r.execute(&mut host, &inv).unwrap();
        assert_eq!(host.text.as_str(), "abc日本語");
        assert_eq!(out.target, Some(host.doc()));
        assert_eq!(out.version, Some(Version::from_raw(1)));
        assert_eq!(host.state.last_actor(), Actor::User);
        assert_eq!(host.text.open_undo_groups(), 0);

        host.no_active = true;
        assert!(matches!(
            r.execute(&mut host, &inv),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn ai_write_requires_the_current_version() {
        let r = registry();
        let mut host = file_host();
        let args = json!({ "position": 0, "text": "x", "twice": true });
        let doc = host.doc();

        // 前提の版がない書き込みは拒否する。
        let e = r.execute(&mut host, &ai(INSERT.id, doc.clone(), None, args.clone()));
        assert!(
            matches!(e, Err(Error::InvalidArgument { ref name, .. }) if name == "expected_version")
        );
        // 対象の指定がない書き込みも拒否する。
        let mut no_target = ai(INSERT.id, doc.clone(), Some(Version::INITIAL), args.clone());
        no_target.target = None;
        assert!(matches!(
            r.execute(&mut host, &no_target),
            Err(Error::InvalidArgument { ref name, .. }) if name == "target"
        ));
        assert_eq!(host.text.as_str(), "abc");

        // 版が合えば実行し、変更は AI のものとして 1 つの Undo 単位で記録される。
        let out = r
            .execute(
                &mut host,
                &ai(INSERT.id, doc.clone(), Some(Version::INITIAL), args.clone()),
            )
            .unwrap();
        assert_eq!(host.text.as_str(), "xxabc");
        assert_eq!(out.version, Some(Version::from_raw(2)));
        assert!(host.state.has_ai_edits());
        assert_eq!(host.text.open_undo_groups(), 0);

        // 利用者が先に編集していたら、古い版を前提にした書き込みは拒否して最新の版を返す。
        host.replace(TextRange::at(0), "u").unwrap();
        match r.execute(&mut host, &ai(INSERT.id, doc, out.version, args)) {
            Err(Error::VersionMismatch {
                expected, actual, ..
            }) => {
                assert_eq!(expected, Version::from_raw(2));
                assert_eq!(actual, Version::from_raw(3));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(host.text.as_str(), "uxxabc");
    }

    #[test]
    fn ai_read_needs_no_version_but_honours_one() {
        let r = registry();
        let mut host = file_host();
        let doc = host.doc();
        let out = r
            .execute(&mut host, &ai(LENGTH.id, doc.clone(), None, Value::Null))
            .unwrap();
        assert_eq!(out.value, json!({ "length": 3 }));
        assert_eq!(out.version, Some(Version::INITIAL));
        let stale = ai(LENGTH.id, doc, Some(Version::from_raw(9)), Value::Null);
        assert!(matches!(
            r.execute(&mut host, &stale),
            Err(Error::VersionMismatch { .. })
        ));
    }

    #[test]
    fn user_only_commands_are_denied_to_ai() {
        let r = registry();
        let mut host = file_host();
        let mut inv = Invocation::user(FIND_DIALOG.id);
        assert!(r.execute(&mut host, &inv).is_ok());
        inv.actor = Actor::Ai;
        assert!(matches!(
            r.execute(&mut host, &inv),
            Err(Error::Denied { .. })
        ));
        assert_eq!(FIND_DIALOG.ai_scope(), None);
        assert_eq!(LENGTH.ai_scope(), Some(AccessScope::Read));
        assert_eq!(INSERT.ai_scope(), Some(AccessScope::Write));
        assert!(SAVE.needs_confirmation_from_ai() && !INSERT.needs_confirmation_from_ai());
    }

    #[test]
    fn capabilities_gate_commands() {
        let r = registry();
        let state = DocumentState::open_file(
            "big.log".into(),
            true,
            Default::default(),
            crate::file::FileStamp {
                len: 0,
                modified: None,
            },
        );
        let mut host = TestHost::new(state, "abc");
        let insert = Invocation::user(INSERT.id).with_args(json!({ "position": 0, "text": "x" }));
        assert!(matches!(
            r.check_available(&host, &insert),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            r.execute(&mut host, &insert),
            Err(Error::Unsupported { .. })
        ));
        assert_eq!(host.text.as_str(), "abc");
        assert!(r
            .check_available(&host, &Invocation::user(LENGTH.id))
            .is_ok());

        // 外部変更を検知した文書は上書き保存できない。
        let mut host = file_host();
        assert!(r.check_available(&host, &Invocation::user(SAVE.id)).is_ok());
        host.state.record_external_change();
        assert!(matches!(
            r.execute(&mut host, &Invocation::user(SAVE.id)),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn arguments_are_validated_before_the_handler_runs() {
        let r = registry();
        let mut host = file_host();
        for bad in [
            json!({ "position": 0 }),
            json!({ "position": "0", "text": "x" }),
            json!({ "position": -1, "text": "x" }),
            json!({ "position": 0, "text": "x", "extra": 1 }),
            json!([0, "x"]),
            Value::Null,
        ] {
            let inv = Invocation::user(INSERT.id).with_args(bad.clone());
            assert!(
                matches!(
                    r.execute(&mut host, &inv),
                    Err(Error::InvalidArgument { .. })
                ),
                "{bad}"
            );
        }
        assert_eq!(host.text.as_str(), "abc");
        assert_eq!(host.state.version(), Version::INITIAL);
    }

    #[test]
    fn handler_errors_still_close_the_undo_group_and_are_recorded() {
        let r = registry();
        let mut host = file_host();
        let doc = host.doc();
        let inv = ai(
            INSERT.id,
            doc.clone(),
            Some(Version::INITIAL),
            json!({ "position": 99, "text": "x" }),
        );
        assert!(matches!(
            r.execute(&mut host, &inv),
            Err(Error::InvalidArgument { .. })
        ));
        assert_eq!(host.text.open_undo_groups(), 0);
        assert!(host.editing_as.is_none());
        assert_eq!(
            host.records,
            vec![CommandRecord {
                command: INSERT.id,
                actor: Actor::Ai,
                target: Some(doc),
                version_before: Some(Version::INITIAL),
                version_after: Some(Version::INITIAL),
                error: Some("invalid_argument"),
            }]
        );
    }

    #[test]
    fn memo_targets_use_the_same_version_check() {
        let r = registry();
        let mut host = file_host();
        let memo = TargetRef::Memo(host.memo.0.clone());
        let args = json!({ "name": "新しい名前" });
        let stale = ai(
            MEMO_RENAME.id,
            memo.clone(),
            Some(Version::from_raw(3)),
            args.clone(),
        );
        assert!(matches!(
            r.execute(&mut host, &stale),
            Err(Error::VersionMismatch { actual, .. }) if actual == Version::from_raw(4)
        ));
        let out = r
            .execute(
                &mut host,
                &ai(
                    MEMO_RENAME.id,
                    memo,
                    Some(Version::from_raw(4)),
                    args.clone(),
                ),
            )
            .unwrap();
        assert_eq!(out.version, Some(Version::from_raw(5)));
        // 文書を対象に指定したら種類の不一致で拒否する。
        let wrong = ai(MEMO_RENAME.id, host.doc(), Some(Version::INITIAL), args);
        assert!(matches!(
            r.execute(&mut host, &wrong),
            Err(Error::InvalidArgument { .. })
        ));
        let unknown = TargetRef::Memo(MemoId::generate());
        let missing = ai(
            MEMO_RENAME.id,
            unknown,
            Some(Version::INITIAL),
            json!({ "name": "x" }),
        );
        assert!(matches!(
            r.execute(&mut host, &missing),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn registry_rejects_duplicates_and_bad_default_keys() {
        let mut r = registry();
        assert!(r.register(SAVE, |_, _| Ok(Value::Null)).is_err());
        let bad = CommandDef::new(CommandId::new("x.y"), "x", "x").default_key("Ctrl+Nope");
        assert!(r.register(bad, |_, _| Ok(Value::Null)).is_err());
        assert!(r.find("x.y").is_none());
        assert_eq!(r.find("file.save").map(|d| d.id), Some(SAVE.id));
        let names: Vec<_> = r.definitions().map(|d| d.id.as_str()).collect();
        assert_eq!(
            names,
            [
                "document.length",
                "edit.insert",
                "file.save",
                "memo.rename",
                "search.show"
            ]
        );
        let unknown = Invocation::user(CommandId::new("no.such"));
        assert!(matches!(
            r.execute(&mut file_host(), &unknown),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn definitions_are_machine_readable() {
        let d = INSERT.describe();
        assert_eq!(d["name"], "edit.insert");
        assert_eq!(d["aiScope"], "write");
        assert_eq!(d["needsVersion"], true);
        assert_eq!(d["requires"], json!(["edit"]));
        assert_eq!(
            d["inputSchema"],
            json!({
                "type": "object",
                "properties": {
                    "position": { "type": "integer", "minimum": 0, "description": "挿入位置（バイト）" },
                    "text": { "type": "string", "description": "挿入する文字列" },
                    "twice": { "type": "boolean", "description": "2 回挿入する" },
                },
                "required": ["position", "text"],
                "additionalProperties": false,
            })
        );
        assert_eq!(FIND_DIALOG.describe()["aiScope"], Value::Null);
        let e = ParamDef::required("mode", ParamType::Enum(&["text", "regex"]), "検索の種類");
        assert!(e.check(&json!("regex")).is_ok());
        assert!(e.check(&json!("glob")).is_err());
        assert_eq!(e.schema()["enum"], json!(["text", "regex"]));
    }
}
