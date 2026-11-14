//! 印刷のコマンド（U5）: 印刷、ページ設定、印刷の選択肢の切り替え。
//!
//! 実際の印刷は [`crate::ui::print`]。ここはコマンドの定義、設定の保存、状態表示を受け持つ。
//! 巨大ファイル（読み取り専用モード）は印刷の対象外で、`Capability::Print` がないので
//! メニューは無効になり、キーで呼ぶと案内を出す。

use serde_json::Value;

use crate::appdata::settings::PrintSettings;
use crate::command::{
    CommandDef, CommandId, ParamDef, ParamType, Registry, TargetKind, Value as CmdValue,
};
use crate::core::{DocumentId, Result};
use crate::document::{Capabilities, Capability};
use crate::ui::print::{print_with_dialog, PrintOutcome, PrintRequest};
use crate::ui::statusbar::MessageKind;

use super::App;

/// 巨大ファイルを印刷しようとしたときの案内。
pub const LARGE_FILE_GUIDANCE: &str =
    "巨大ファイル（読み取り専用）は印刷できません。必要な範囲を新しい文書にコピーして印刷してください";

pub const FILE_PRINT: CommandDef = CommandDef::new(
    CommandId::new("file.print"),
    "印刷...",
    "印刷ダイアログを開いて文書を印刷する（プリンター、部数、ページ範囲、選択範囲のみ）。巨大ファイルは印刷できない。",
)
.target(TargetKind::Document)
.requires(Capabilities::of(&[Capability::Print]))
.default_key("Ctrl+P");

pub const FILE_PAGE_SETUP: CommandDef = CommandDef::new(
    CommandId::new("file.page_setup"),
    "ページ設定...",
    "用紙、向き、余白を設定する。設定は保存され、次の印刷から使われる。",
);

const ENABLED: &[ParamDef] = &[ParamDef::optional(
    "enabled",
    ParamType::Boolean,
    "オンにするか（省略すると今と反対にする）",
)];

macro_rules! print_toggle {
    ($name:ident, $id:literal, $title:literal, $desc:literal) => {
        pub const $name: CommandDef = CommandDef::new(CommandId::new($id), $title, $desc)
            .params(ENABLED)
            .result("{\"enabled\": 切り替えた後の状態}");
    };
}

print_toggle!(
    PRINT_TOGGLE_COLOR,
    "print.toggle_color",
    "色分けして印刷",
    "構文の色を付けて印刷するか（オフなら白黒）を切り替えて保存する。背景は常に白。"
);
print_toggle!(
    PRINT_TOGGLE_LINE_NUMBERS,
    "print.toggle_line_numbers",
    "行番号を印刷",
    "行番号を印刷するかを切り替えて保存する。"
);
print_toggle!(
    PRINT_TOGGLE_HEADER,
    "print.toggle_header",
    "ヘッダーを印刷",
    "ヘッダー（ファイル名またはタブ名と印刷日時）を印刷するかを切り替えて保存する。"
);
print_toggle!(
    PRINT_TOGGLE_FOOTER,
    "print.toggle_footer",
    "フッターを印刷",
    "フッター（ページ番号「n / m」）を印刷するかを切り替えて保存する。"
);

fn register_toggle(
    registry: &mut Registry<App>,
    def: CommandDef,
    label: &'static str,
    field: fn(&mut PrintSettings) -> &mut bool,
) -> Result<()> {
    registry.register(def, move |app, call| {
        let mut next = app.settings.print.clone();
        let current = *field(&mut next);
        let enabled = call.bool_or("enabled", !current);
        *field(&mut next) = enabled;
        if next != app.settings.print {
            app.settings.print = next;
            app.persist_settings();
        }
        app.set_status(
            &format!("{label}: {}", if enabled { "オン" } else { "オフ" }),
            MessageKind::Info,
        );
        Ok(serde_json::json!({ "enabled": enabled }))
    })
}

/// 印刷のコマンドを登録する。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(FILE_PRINT, |app, call| {
        let pages = app.print_document(call.document()?)?;
        Ok(serde_json::json!({ "pages": pages }))
    })?;
    registry.register(FILE_PAGE_SETUP, |app, _| {
        app.page_setup()?;
        Ok(CmdValue::Null)
    })?;
    register_toggle(registry, PRINT_TOGGLE_COLOR, "色分けして印刷", |p| {
        &mut p.color
    })?;
    register_toggle(
        registry,
        PRINT_TOGGLE_LINE_NUMBERS,
        "行番号を印刷",
        |p| &mut p.line_numbers,
    )?;
    register_toggle(
        registry,
        PRINT_TOGGLE_HEADER,
        "ヘッダーを印刷",
        |p| &mut p.header,
    )?;
    register_toggle(
        registry,
        PRINT_TOGGLE_FOOTER,
        "フッターを印刷",
        |p| &mut p.footer,
    )?;
    Ok(())
}

impl App {
    /// アクティブな文書が印刷できるか（巨大ファイルは不可）。
    pub(super) fn active_can_print(&self) -> bool {
        self.active.is_some_and(|i| {
            self.docs[i]
                .state
                .capabilities()
                .contains(Capability::Print)
        })
    }

    /// 文書を印刷する（印刷ダイアログを開く）。送ったページ数（取り消しは `null`）。
    fn print_document(&mut self, id: DocumentId) -> Result<Value> {
        let i = self.index_of(id)?;
        let title = self.docs[i].title.clone();
        let settings = self.settings.print.clone();
        let owner = self.hwnd;
        let outcome = self.with_text(id, |editor| {
            let sel = editor.selection();
            let selection = (sel.anchor != sel.caret)
                .then(|| (sel.anchor.min(sel.caret), sel.anchor.max(sel.caret)));
            print_with_dialog(
                owner,
                editor,
                &PrintRequest {
                    title: &title,
                    settings: &settings,
                    selection,
                    doc_len: editor.length(),
                },
            )
        })??;
        Ok(match outcome {
            PrintOutcome::Cancelled => Value::Null,
            PrintOutcome::Printed { pages } => {
                self.set_status(
                    &format!("{pages} ページを印刷に送りました"),
                    MessageKind::Info,
                );
                Value::from(pages)
            }
        })
    }

    /// ページ設定のダイアログを開き、変更があれば保存する。
    fn page_setup(&mut self) -> Result<()> {
        if let Some(next) = crate::ui::print::page_setup(self.hwnd, &self.settings.print)? {
            self.settings.print = next;
            self.persist_settings();
        }
        Ok(())
    }

    /// 印刷の切り替え項目のチェック状態（メニュー用）。
    pub(super) fn print_menu_checked(&self, command: CommandId) -> Option<bool> {
        let p = &self.settings.print;
        Some(match command.as_str() {
            "print.toggle_color" => p.color,
            "print.toggle_line_numbers" => p.line_numbers,
            "print.toggle_header" => p.header,
            "print.toggle_footer" => p.footer,
            _ => return None,
        })
    }
}
