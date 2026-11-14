//! 表示のコマンド（折り返し、書体、拡大・縮小、空白・改行・行番号の表示、現在行・括弧の
//! 強調、折り返し桁のガイド線、タブ幅、自動インデント）と、設定への保存・全タブへの反映。
//!
//! どのコマンドも [`App::update_view`] で設定（`settings.view`）を書き換える。
//! それが編集欄と巨大ファイルのビューに反映し、設定ファイルに保存する。

use crate::appdata::settings::{
    ViewSettings, WrapMode, FONT_SIZE_DEFAULT, FONT_SIZE_MAX, FONT_SIZE_MIN, WRAP_COLUMN_MAX,
    WRAP_COLUMN_MIN, ZOOM_MAX, ZOOM_MIN,
};
use crate::command::{Call, CommandDef, CommandId, ParamDef, ParamType, Registry, Value};
use crate::core::{Error, Result};
use crate::ui::editor::Language;
use crate::ui::statusbar::MessageKind;

use super::{App, DocView};

const WRAP_MODES: &[&str] = &["none", "word", "char", "column"];
const TAB_WIDTHS: &[&str] = &["2", "4", "8"];

pub const VIEW_WRAP: CommandDef = CommandDef::new(
    CommandId::new("view.wrap"),
    "折り返し",
    "行の折り返し方法を変えて保存する（none＝しない、word＝ウィンドウ幅・単語単位、char＝ウィンドウ幅・文字単位、column＝指定した桁）。column で column を省くと入力欄を出す。巨大ファイルのビューは対象外。",
)
.params(&[
    ParamDef::required(
        "mode",
        ParamType::Enum(WRAP_MODES),
        "折り返しの方法（none、word、char、column）",
    ),
    ParamDef::optional(
        "column",
        ParamType::Integer,
        "mode が column のときの桁数（10〜500）",
    ),
])
.result("{\"mode\": 方法, \"column\": 桁数}");

pub const VIEW_FONT: CommandDef = CommandDef::new(
    CommandId::new("view.font"),
    "フォント...",
    "本文の書体と文字サイズを変えて保存する（font も size も省くとフォント選択ダイアログを出す。font を空文字列にすると既定の書体と大きさに戻す）。",
)
.params(&[
    ParamDef::optional("font", ParamType::String, "書体の名前（空文字列で既定）"),
    ParamDef::optional("size", ParamType::Integer, "文字サイズ（ポイント、6〜72）"),
])
.result("{\"font\": 書体（空は既定）, \"size\": ポイント}");

pub const VIEW_ZOOM_IN: CommandDef = CommandDef::new(
    CommandId::new("view.zoom_in"),
    "拡大",
    "本文を 1 段階拡大する（Ctrl+マウスホイールでも変わる）。保存する。",
)
.default_key("Ctrl+Plus");

pub const VIEW_ZOOM_OUT: CommandDef = CommandDef::new(
    CommandId::new("view.zoom_out"),
    "縮小",
    "本文を 1 段階縮小する。",
)
.default_key("Ctrl+Minus");

pub const VIEW_ZOOM_RESET: CommandDef = CommandDef::new(
    CommandId::new("view.zoom_reset"),
    "拡大率を元に戻す",
    "拡大・縮小を 100% に戻す。",
)
.default_key("Ctrl+0");

pub const VIEW_TAB_WIDTH: CommandDef = CommandDef::new(
    CommandId::new("view.tab_width"),
    "タブ幅",
    "タブ文字の幅（桁数）を変えて保存する。",
)
.params(&[ParamDef::required(
    "width",
    ParamType::Enum(TAB_WIDTHS),
    "2、4、8 のいずれか",
)])
.result("{\"width\": 幅}");

/// 切り替えコマンドの共通の引数（省略すると反転）。
const ENABLED: &[ParamDef] = &[ParamDef::optional(
    "enabled",
    ParamType::Boolean,
    "オンにするか（省略すると今と反対にする）",
)];

macro_rules! toggle_def {
    ($name:ident, $id:literal, $title:literal, $desc:literal) => {
        pub const $name: CommandDef = CommandDef::new(CommandId::new($id), $title, $desc)
            .params(ENABLED)
            .result("{\"enabled\": 切り替えた後の状態}");
    };
}

toggle_def!(
    VIEW_TOGGLE_WHITESPACE,
    "view.toggle_whitespace",
    "空白とタブを表示",
    "空白とタブの記号の表示を切り替えて保存する。"
);
toggle_def!(
    VIEW_TOGGLE_EOL,
    "view.toggle_eol",
    "改行記号を表示",
    "改行記号の表示を切り替えて保存する。"
);
toggle_def!(
    VIEW_TOGGLE_LINE_NUMBERS,
    "view.toggle_line_numbers",
    "行番号を表示",
    "行番号の表示を切り替えて保存する（巨大ファイルのビューにも適用する）。"
);
toggle_def!(
    VIEW_TOGGLE_CURRENT_LINE,
    "view.toggle_current_line",
    "現在行を強調",
    "カーソルのある行の強調を切り替えて保存する。"
);
toggle_def!(
    VIEW_TOGGLE_BRACES,
    "view.toggle_braces",
    "対応する括弧を強調",
    "カーソルの前後の括弧とその対応の強調を切り替えて保存する（2 MB を超える文書では行わない）。"
);
toggle_def!(
    VIEW_TOGGLE_EDGE_GUIDE,
    "view.toggle_edge_guide",
    "折り返し桁のガイド線",
    "桁指定で折り返すときのガイド線の表示を切り替えて保存する。"
);
toggle_def!(
    VIEW_TOGGLE_TAB_SPACES,
    "view.toggle_tab_spaces",
    "タブをスペースで入力",
    "Tab キーでタブ文字の代わりにスペースを入力するかを切り替えて保存する。"
);
toggle_def!(
    VIEW_TOGGLE_AUTO_INDENT,
    "view.toggle_auto_indent",
    "自動インデント",
    "改行したとき前の行の行頭の空白を引き継ぐかを切り替えて保存する。"
);

/// 切り替えコマンド 1 つを登録する。`field` は対象の設定項目。
fn register_toggle(
    registry: &mut Registry<App>,
    def: CommandDef,
    label: &'static str,
    field: fn(&mut ViewSettings) -> &mut bool,
) -> Result<()> {
    registry.register(def, move |app, call| {
        let mut probe = app.settings.view.clone();
        let current = *field(&mut probe);
        let next = call.bool_or("enabled", !current);
        app.update_view(|v| *field(v) = next);
        app.set_status(
            &format!("{label}: {}", if next { "オン" } else { "オフ" }),
            MessageKind::Info,
        );
        Ok(serde_json::json!({ "enabled": next }))
    })
}

/// 表示のコマンドを登録する。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(VIEW_WRAP, |app, call| {
        let mode = WrapMode::from_name(call.str("mode")?).ok_or_else(|| {
            Error::invalid_argument("mode", "none、word、char、column のいずれかです")
        })?;
        let mut column = app.settings.view.wrap_column;
        if mode == WrapMode::Column {
            column = match call.opt_u64("column") {
                Some(n) => n as u32,
                None => {
                    let owner = app.hwnd;
                    let prompts = app.prompts.clone();
                    let label = format!("折り返す桁数（{WRAP_COLUMN_MIN} 〜 {WRAP_COLUMN_MAX}）:");
                    let Some(input) =
                        prompts.input_text(owner, "桁数で折り返し", &label, &column.to_string())
                    else {
                        return Ok(Value::Null);
                    };
                    match crate::ui::bigview::logic::parse_line_number(&input) {
                        Some(n) if n <= u32::MAX as u64 => n as u32,
                        _ => {
                            app.set_status(
                                "桁数は 1 以上の数字で入力してください",
                                MessageKind::Warning,
                            );
                            return Ok(Value::Null);
                        }
                    }
                }
            };
            if !(WRAP_COLUMN_MIN..=WRAP_COLUMN_MAX).contains(&column) {
                let reason = format!("{WRAP_COLUMN_MIN} 〜 {WRAP_COLUMN_MAX} の桁数です");
                if call.opt_u64("column").is_some() {
                    return Err(Error::invalid_argument("column", reason));
                }
                app.set_status(&reason, MessageKind::Warning);
                return Ok(Value::Null);
            }
        }
        app.update_view(|v| {
            v.wrap = mode;
            v.wrap_column = column;
        });
        let text = match mode {
            WrapMode::None => "折り返さない".to_owned(),
            WrapMode::Word => "ウィンドウ幅で折り返す（単語単位）".to_owned(),
            WrapMode::Char => "ウィンドウ幅で折り返す（文字単位）".to_owned(),
            WrapMode::Column => format!("{column} 桁で折り返す"),
        };
        app.set_status(&format!("折り返し: {text}"), MessageKind::Info);
        Ok(serde_json::json!({ "mode": mode.name(), "column": column }))
    })?;

    registry.register(VIEW_FONT, |app, call| {
        let (font, size) = match (call.opt_str("font"), call.opt_u64("size")) {
            (None, None) => {
                let owner = app.hwnd;
                let prompts = app.prompts.clone();
                let current = app.settings.view.clone();
                let name = if current.font.is_empty() {
                    app.editor_font_name()
                } else {
                    current.font.clone()
                };
                match prompts.choose_font(owner, &name, current.font_size) {
                    Some(choice) => choice,
                    None => return Ok(Value::Null),
                }
            }
            (Some(""), _) => (String::new(), FONT_SIZE_DEFAULT),
            (font, size) => {
                if let Some(n) = size {
                    if !(FONT_SIZE_MIN as u64..=FONT_SIZE_MAX as u64).contains(&n) {
                        return Err(Error::invalid_argument(
                            "size",
                            format!("{FONT_SIZE_MIN} 〜 {FONT_SIZE_MAX} ポイントです"),
                        ));
                    }
                }
                (
                    font.map_or_else(|| app.settings.view.font.clone(), str::to_owned),
                    size.map_or(app.settings.view.font_size, |n| n as u32),
                )
            }
        };
        let size = size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        app.update_view(|v| {
            v.font = font.clone();
            v.font_size = size;
        });
        let shown = if font.is_empty() {
            "既定".to_owned()
        } else {
            font.clone()
        };
        app.set_status(&format!("フォント: {shown} {size} pt"), MessageKind::Info);
        Ok(serde_json::json!({ "font": font, "size": size }))
    })?;

    registry.register(VIEW_ZOOM_IN, |app, _| app.zoom_by(1))?;
    registry.register(VIEW_ZOOM_OUT, |app, _| app.zoom_by(-1))?;
    registry.register(VIEW_ZOOM_RESET, |app, _| {
        app.update_view(|v| v.zoom = 0);
        app.set_status("拡大率: 100%", MessageKind::Info);
        Ok(serde_json::json!({ "zoom": 0 }))
    })?;

    registry.register(VIEW_TAB_WIDTH, |app, call: &Call<'_>| {
        let width: u32 = call
            .str("width")?
            .parse()
            .map_err(|_| Error::invalid_argument("width", "2、4、8 のいずれかです"))?;
        app.update_view(|v| v.tab_width = width);
        app.set_status(&format!("タブ幅: {width}"), MessageKind::Info);
        Ok(serde_json::json!({ "width": width }))
    })?;

    register_toggle(
        registry,
        VIEW_TOGGLE_WHITESPACE,
        "空白とタブの表示",
        |v| &mut v.show_whitespace,
    )?;
    register_toggle(registry, VIEW_TOGGLE_EOL, "改行記号の表示", |v| {
        &mut v.show_eol
    })?;
    register_toggle(
        registry,
        VIEW_TOGGLE_LINE_NUMBERS,
        "行番号の表示",
        |v| &mut v.line_numbers,
    )?;
    register_toggle(
        registry,
        VIEW_TOGGLE_CURRENT_LINE,
        "現在行の強調",
        |v| &mut v.highlight_current_line,
    )?;
    register_toggle(
        registry,
        VIEW_TOGGLE_BRACES,
        "対応する括弧の強調",
        |v| &mut v.highlight_braces,
    )?;
    register_toggle(
        registry,
        VIEW_TOGGLE_EDGE_GUIDE,
        "折り返し桁のガイド線",
        |v| &mut v.edge_guide,
    )?;
    register_toggle(
        registry,
        VIEW_TOGGLE_TAB_SPACES,
        "タブをスペースで入力",
        |v| &mut v.use_spaces,
    )?;
    register_toggle(
        registry,
        VIEW_TOGGLE_AUTO_INDENT,
        "自動インデント",
        |v| &mut v.auto_indent,
    )?;
    Ok(())
}

impl App {
    /// 表示の設定を書き換えて、全タブに反映し、設定ファイルに保存する。
    pub(super) fn update_view(&mut self, change: impl FnOnce(&mut ViewSettings)) {
        let mut view = self.settings.view.clone();
        change(&mut view);
        let view = view.clamped();
        if view == self.settings.view {
            return;
        }
        self.settings.view = view;
        self.apply_view_settings();
        self.persist_settings();
    }

    /// 設定の表示項目を編集欄と巨大ファイルのビューに反映する（保存はしない）。
    pub(super) fn apply_view_settings(&mut self) {
        let view = self.settings.view.clone();
        self.editor.apply_view(&view);
        // 書体と文字サイズはスタイルの付け直しで反映する。
        let language = self
            .active
            .map(|i| self.docs[i].language)
            .unwrap_or(Language::PlainText);
        self.editor.apply_theme(&self.theme, language);
        for b in self.bigviews.values() {
            b.view.set_line_numbers(view.line_numbers);
        }
        self.editor.update_brace_highlight();
    }

    /// 拡大率が編集欄側で変わった（Ctrl+ホイール）。設定に取り込んで保存する。
    pub(super) fn on_zoom_changed(&mut self) {
        let zoom = self.editor.zoom().clamp(ZOOM_MIN, ZOOM_MAX);
        if zoom == self.settings.view.zoom {
            return;
        }
        self.settings.view.zoom = zoom;
        // 拡大率が変わると文字の幅が変わるので、桁指定の右余白と行番号欄を合わせ直す。
        self.editor.update_line_number_margin();
        self.persist_settings();
    }

    fn zoom_by(&mut self, delta: i32) -> Result<Value> {
        let zoom = (self.settings.view.zoom + delta).clamp(ZOOM_MIN, ZOOM_MAX);
        self.update_view(|v| v.zoom = zoom);
        self.set_status(
            &format!("拡大率: {}", zoom_label(self.settings.view.font_size, zoom)),
            MessageKind::Info,
        );
        Ok(serde_json::json!({ "zoom": zoom }))
    }

    /// 既定の書体の名前（フォント選択ダイアログの初期値）。
    pub(super) fn editor_font_name(&self) -> String {
        crate::ui::editor::default_font_name().to_owned()
    }

    /// メニューの印（チェック）の状態。印を付ける項目でなければ `None`。
    pub(crate) fn view_menu_checked(&self, command: CommandId, args: &Value) -> Option<bool> {
        let v = &self.settings.view;
        let arg = args.as_object().and_then(|m| m.values().next());
        let arg = arg.and_then(Value::as_str);
        Some(match command.as_str() {
            "view.wrap" => v.wrap.name() == arg?,
            "view.tab_width" => v.tab_width.to_string() == arg?,
            "view.toggle_whitespace" => v.show_whitespace,
            "view.toggle_eol" => v.show_eol,
            "view.toggle_line_numbers" => v.line_numbers,
            "view.toggle_current_line" => v.highlight_current_line,
            "view.toggle_braces" => v.highlight_braces,
            "view.toggle_edge_guide" => v.edge_guide,
            "view.toggle_tab_spaces" => v.use_spaces,
            "view.toggle_auto_indent" => v.auto_indent,
            "edit.rectangular_select" => {
                matches!(
                    self.active.map(|i| self.docs[i].view),
                    Some(DocView::Scintilla(_))
                ) && self.editor.rectangular_select_active()
            }
            _ => return None,
        })
    }
}

/// 拡大率の表示（文字サイズに対する割合）。
pub fn zoom_label(font_size: u32, zoom: i32) -> String {
    let size = (font_size as i32 + zoom).max(1) as f64;
    format!(
        "{}%",
        (size / font_size.max(1) as f64 * 100.0).round() as i64
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_label_is_a_percentage_of_the_font_size() {
        assert_eq!(zoom_label(11, 0), "100%");
        assert_eq!(zoom_label(10, 5), "150%");
        assert_eq!(zoom_label(10, -5), "50%");
    }
}
