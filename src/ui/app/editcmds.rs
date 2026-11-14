//! 編集コマンド（改善 U3）: 行の複製・削除・移動、大文字・小文字、行末の空白の削除、
//! タブ⇔スペース、日時の挿入、対応する括弧へ移動。
//!
//! 編集はどれも編集欄で 1 つの Undo のまとまりにする（計算は
//! [`crate::ui::editor::transform`]）。巨大ファイル（読み取り専用）では編集系は使えない。

use crate::command::{CommandDef, CommandId, Effect, Registry, TargetKind, Value};
use crate::core::Result;
use crate::document::{Capabilities, Capability};
use crate::ui::editor::transform::format_datetime;
use crate::ui::statusbar::MessageKind;

use super::App;

const EDIT: Capabilities = Capabilities::of(&[Capability::Edit]);

macro_rules! edit_command {
    ($name:ident, $id:literal, $title:literal, $desc:literal $(, $key:literal)?) => {
        pub const $name: CommandDef =
            CommandDef::new(CommandId::new($id), $title, $desc)
                .target(TargetKind::Document)
                .requires(EDIT)
                .effect(Effect::Edit)
                $(.default_key($key))?;
    };
}

edit_command!(
    EDIT_DUPLICATE_LINE,
    "edit.duplicate_line",
    "行を複製",
    "選択があれば選択を、なければカーソルのある行を、直後に複製する。",
    "Ctrl+D"
);
edit_command!(
    EDIT_DELETE_LINE,
    "edit.delete_line",
    "行を削除",
    "カーソルのある行（選択にかかる行すべて）を削除する。",
    "Ctrl+Shift+K"
);
edit_command!(
    EDIT_MOVE_LINE_UP,
    "edit.move_line_up",
    "行を上へ移動",
    "カーソルのある行（選択にかかる行すべて）を 1 行上へ移動する。",
    "Alt+Up"
);
edit_command!(
    EDIT_MOVE_LINE_DOWN,
    "edit.move_line_down",
    "行を下へ移動",
    "カーソルのある行（選択にかかる行すべて）を 1 行下へ移動する。",
    "Alt+Down"
);
edit_command!(
    EDIT_UPPERCASE,
    "edit.uppercase",
    "大文字にする",
    "選択範囲を大文字にする。"
);
edit_command!(
    EDIT_LOWERCASE,
    "edit.lowercase",
    "小文字にする",
    "選択範囲を小文字にする。"
);
edit_command!(
    EDIT_TRIM_TRAILING,
    "edit.trim_trailing_whitespace",
    "行末の空白を削除",
    "行末のスペースとタブを削除する（選択があればその行だけ、なければ文書全体）。"
);
edit_command!(
    EDIT_TABS_TO_SPACES,
    "edit.tabs_to_spaces",
    "タブをスペースに変換",
    "タブ文字を、タブ幅に合わせたスペースにする（選択があればその行だけ、なければ文書全体）。"
);
edit_command!(
    EDIT_SPACES_TO_TABS,
    "edit.spaces_to_tabs",
    "行頭のスペースをタブに変換",
    "行頭のインデントを、タブ幅ぶんのスペースをタブ 1 つにして書き直す（選択があればその行だけ、なければ文書全体）。行の途中の空白は変えない。"
);
edit_command!(
    EDIT_INSERT_DATETIME,
    "edit.insert_datetime",
    "日時を挿入",
    "現在のローカル日時（2026-11-14 09:05 の形式）をカーソル位置に挿入する（選択は置き換える）。"
);

pub const EDIT_GOTO_BRACE: CommandDef = CommandDef::new(
    CommandId::new("edit.goto_matching_brace"),
    "対応する括弧へ移動",
    "カーソルの位置（またはすぐ前）の括弧 ( ) [ ] { } に対応する括弧へ移動する。",
)
.target(TargetKind::Document)
.default_key("Ctrl+BracketRight");

/// 次のマウス選択・矢印キーによる選択を矩形にする切り替え（Alt＋ドラッグの代わり）。
pub const EDIT_RECTANGULAR_SELECT: CommandDef = CommandDef::new(
    CommandId::new("edit.rectangular_select"),
    "矩形選択",
    "次のマウス選択・矢印キーによる選択を矩形にする状態を切り替える（もう一度で解除）。",
)
.target(TargetKind::Document);

impl App {
    /// 行末の空白などの変換の結果を知らせる。
    fn report_edit(&mut self, count: usize, done: &str, none: &str) {
        let text = if count == 0 {
            none.to_owned()
        } else {
            format!("{count} {done}（「元に戻す」1 回で元に戻ります）")
        };
        self.set_status(&text, MessageKind::Info);
    }
}

/// 編集コマンドを登録する。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(EDIT_RECTANGULAR_SELECT, |app, call| {
        app.with_text(call.document()?, |ed| ed.toggle_rectangular_select())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_DUPLICATE_LINE, |app, call| {
        app.with_text(call.document()?, |ed| ed.duplicate_line_or_selection())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_DELETE_LINE, |app, call| {
        app.with_text(call.document()?, |ed| ed.delete_lines())?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_MOVE_LINE_UP, |app, call| {
        app.with_text(call.document()?, |ed| ed.move_lines(true))?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_MOVE_LINE_DOWN, |app, call| {
        app.with_text(call.document()?, |ed| ed.move_lines(false))?;
        Ok(Value::Null)
    })?;
    registry.register(EDIT_UPPERCASE, |app, call| {
        case_command(app, call.document()?, true)
    })?;
    registry.register(EDIT_LOWERCASE, |app, call| {
        case_command(app, call.document()?, false)
    })?;
    registry.register(EDIT_TRIM_TRAILING, |app, call| {
        let n = app.with_text(call.document()?, |ed| ed.trim_trailing_whitespace())?;
        app.report_edit(
            n,
            "か所の行末の空白を削除しました",
            "行末の空白はありません",
        );
        Ok(serde_json::json!({ "changed": n }))
    })?;
    registry.register(EDIT_TABS_TO_SPACES, |app, call| {
        let n = app.with_text(call.document()?, |ed| ed.tabs_to_spaces())?;
        app.report_edit(n, "個のタブをスペースにしました", "タブはありません");
        Ok(serde_json::json!({ "changed": n }))
    })?;
    registry.register(EDIT_SPACES_TO_TABS, |app, call| {
        let n = app.with_text(call.document()?, |ed| ed.spaces_to_tabs())?;
        app.report_edit(
            n,
            "行のインデントをタブにしました",
            "タブにできるインデントはありません",
        );
        Ok(serde_json::json!({ "changed": n }))
    })?;
    registry.register(EDIT_INSERT_DATETIME, |app, call| {
        let (y, mo, d, h, mi) = crate::platform::system::local_time();
        let text = format_datetime(y, mo, d, h, mi);
        let inserted = text.clone();
        app.with_text(call.document()?, move |ed| ed.replace_selection_text(&text))?;
        Ok(serde_json::json!({ "text": inserted }))
    })?;
    registry.register(EDIT_GOTO_BRACE, |app, call| {
        let id = call.document()?;
        if app.bigviews.contains_key(&id) {
            app.set_status(
                "巨大ファイルでは括弧への移動は使えません",
                MessageKind::Warning,
            );
            return Ok(serde_json::json!({ "moved": false }));
        }
        let moved = app.with_text(id, |ed| ed.goto_matching_brace())?;
        if !moved {
            app.set_status(
                "対応する括弧がありません（カーソルを括弧の上か直後に置いてください）",
                MessageKind::Warning,
            );
        }
        Ok(serde_json::json!({ "moved": moved }))
    })?;
    Ok(())
}

fn case_command(app: &mut App, id: crate::core::DocumentId, upper: bool) -> Result<Value> {
    let done = app.with_text(id, |ed| ed.convert_case(upper))?;
    if !done {
        app.set_status("変換する範囲を選択してください", MessageKind::Warning);
    }
    Ok(serde_json::json!({ "changed": done }))
}
