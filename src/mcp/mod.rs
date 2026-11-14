//! 将来の MCP 連携（要件 C）の中継口の型。**初版は型だけで、通信は実装しない。**
//!
//! 次の版（タスク P1〜P3）の構成:
//! - 同じ EXE を `--mcp-stdio` で起動した MCP プロセスが、AI クライアントと
//!   MCP stdio（JSON-RPC 2.0、改行区切り）で話す。GUI は初期化しない。
//! - GUI が起動中なら、MCP プロセスは現在ユーザー専用の名前付きパイプで
//!   [`RelayRequest`] を GUI に送り、GUI が UI スレッドで
//!   [`Registry::execute`](crate::command::Registry::execute) を呼んで
//!   [`RelayResponse`] を返す。データを変更する窓口は GUI だけ。
//! - GUI が起動していなければ、メモの読み書きとファイルの読み取りだけを MCP プロセスが
//!   中核モジュール（`memo`、`file`、`text`）で直接行う。
//!
//! 守ること:
//! - このモジュールは GUI に依存しない。通常の GUI 起動経路から呼ばない
//!   （MCP が無効なら、パイプもスレッドも認可ファイルの読み込みも発生させない）。
//! - AI の操作は必ず [`Actor::Ai`] の [`Invocation`] にして、利用者と同じ入口を通す。
//!   文書・メモを変える操作は版の前提が必須（登録表が検査する）。
//! - 接続は同時に 1 つ。入力の大きさは [`MAX_REQUEST_BYTES`] まで。

use crate::command::{CommandHost, CommandOutput, Invocation, Registry, Value};
use crate::core::{Actor, Error, Result, TargetRef, Version};

/// 1 件の要求の大きさの上限（バイト）。超える入力は読み捨てて拒否する。
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

/// MCP プロセスから GUI への要求。
#[derive(Debug, Clone, PartialEq)]
pub struct RelayRequest {
    /// 応答との対応付けに使う番号（接続内で一意）。
    pub id: u64,
    pub body: RelayRequestBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelayRequestBody {
    /// AI から呼べるコマンドの定義の一覧
    /// （[`CommandDef::describe`](crate::command::CommandDef::describe) の配列）。
    ListCommands,
    /// コマンドの呼び出し。
    Call(CommandCall),
}

/// 外部から届いたコマンド呼び出し。名前はまだ解決していない。
#[derive(Debug, Clone, PartialEq)]
pub struct CommandCall {
    pub command: String,
    pub target: Option<TargetRef>,
    pub expected_version: Option<Version>,
    pub args: Value,
}

impl CommandCall {
    /// 登録表で名前を解決し、AI を実行者とする [`Invocation`] にする。
    /// 知らないコマンドは [`Error::NotFound`]。
    pub fn into_invocation<H: CommandHost>(self, registry: &Registry<H>) -> Result<Invocation> {
        let def = registry
            .find(&self.command)
            .ok_or_else(|| Error::not_found(format!("コマンド {}", self.command)))?;
        Ok(Invocation {
            command: def.id,
            actor: Actor::Ai,
            target: self.target,
            expected_version: self.expected_version,
            args: self.args,
        })
    }
}

/// GUI から MCP プロセスへの応答。
#[derive(Debug, Clone, PartialEq)]
pub struct RelayResponse {
    pub id: u64,
    pub result: std::result::Result<RelayOutput, RelayError>,
}

/// 成功した応答の中身。
#[derive(Debug, Clone, PartialEq)]
pub enum RelayOutput {
    Commands(Vec<Value>),
    Call(CommandOutput),
}

/// 失敗した応答の中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayError {
    /// [`Error::code`] の値。
    pub code: String,
    /// 人が読む説明。
    pub message: String,
    /// 版の食い違いで拒否した場合の、対象の最新の版。AI はこの版で読み直してやり直す。
    pub current_version: Option<Version>,
}

impl From<&Error> for RelayError {
    fn from(error: &Error) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            current_version: match error {
                Error::VersionMismatch { actual, .. } => Some(*actual),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DocumentId;

    #[test]
    fn version_mismatch_carries_the_latest_version() {
        let error = Error::VersionMismatch {
            target: TargetRef::Document(DocumentId::from_raw(1)),
            expected: Version::from_raw(2),
            actual: Version::from_raw(5),
        };
        let relayed = RelayError::from(&error);
        assert_eq!(relayed.code, "version_mismatch");
        assert_eq!(relayed.current_version, Some(Version::from_raw(5)));
        assert_eq!(RelayError::from(&Error::Cancelled).current_version, None);
    }
}
