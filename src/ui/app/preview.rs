//! Markdown プレビューの統合（統合 I2 の G・H）。
//!
//! - プレビューは初めて開く操作まで何も作らない（[`App::preview`] は `None`。WebView2 の
//!   ランタイムの探索・環境の生成は [`PreviewHost`] の最初の利用で始まる）。
//! - 右ペイン（`view.preview_pane`）と別ウィンドウ（`view.preview_window`）は、同じ
//!   [`PreviewScheduler`] と同じ変換結果を共有する。
//! - 編集 → [`PreviewScheduler::note_change`] → UI のタイマー（[`TIMER_PREVIEW`]）→
//!   本文の写しをワーカーで変換（[`spawn_render`]）→ [`PreviewScheduler::accept`] で
//!   文書 ID と版を照合 → [`compose_document`] → `set_document`。
//! - 対象は本文を Scintilla で編集している文書すべて（無題・任意の拡張子・メモを含む。巨大文書は
//!   対象外）。プレビューは、最後にアクティブになった対象の文書に追従する。
//! - ランタイムがなければ案内を出し、編集は続けられる。

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::core::{DocStamp, DocumentId, Limits, Result, WorkerPool};
use crate::markdown::{
    compose_document, spawn_render, PreviewPalette, PreviewScheduler, PreviewUpdate, Rgb,
    Stylesheet, DEFAULT_MAX_INPUT_BYTES,
};
use crate::ui::preview::{PaneStatus, PreviewHost, PreviewPane, PreviewWindow};
use crate::ui::statusbar::MessageKind;
use crate::ui::theme::{Rgb as ThemeRgb, Theme};

use super::{App, DocView, OpenDoc, UiMessage};

/// 変換の待ち時間を見るタイマー（B が 1〜99、I1 が 110、巨大ファイルが 130。ここは 120）。
pub(super) const TIMER_PREVIEW: usize = 120;

/// プレビューの状態。初めて開くときに作る。
pub(super) struct PreviewState {
    pub host: PreviewHost,
    pub pane: Option<PreviewPane>,
    pub window: Option<PreviewWindow>,
    pub scheduler: PreviewScheduler,
    pub stylesheet: Stylesheet,
    /// 追従している文書。
    pub target: Option<DocumentId>,
    /// 最後に変換した本文（`<body>` の中身）。テーマの変更で組み直すために持つ。
    pub body: Option<Arc<str>>,
    pub timer: bool,
}

/// 文書がプレビューの対象か。本文を Scintilla で編集している文書すべて（無題、任意の拡張子、
/// クリップボード由来、メモ、読み取り専用を含む。言語や拡張子は問わず Markdown として変換する）。
/// 巨大文書（`DocView::External`）は対象外。
pub(super) fn is_previewable(doc: &OpenDoc) -> bool {
    matches!(doc.view, DocView::Scintilla(_))
}

/// タブの切り替えで追従する文書か（Markdown のファイル）。拡張子 `.md`／`.markdown`。
/// まだファイルでない文書は言語の指定（Markdown）で判断する。開くときの対象は
/// [`is_previewable`] で、こちらは開いた後の追従だけに使う。
pub(super) fn follows_on_switch(doc: &OpenDoc) -> bool {
    if !is_previewable(doc) {
        return false;
    }
    match doc.state.path() {
        Some(path) => path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown")),
        None => doc.language == crate::ui::editor::Language::Markdown,
    }
}

fn rgb(c: ThemeRgb) -> Rgb {
    Rgb::new(c.0, c.1, c.2)
}

/// 編集欄のテーマからプレビューの配色を作る。
pub(super) fn palette_for(theme: &Theme) -> PreviewPalette {
    PreviewPalette {
        dark: theme.dark,
        background: rgb(theme.editor.bg),
        foreground: rgb(theme.editor.fg),
        muted: rgb(theme.ui.status_dim),
        border: rgb(theme.ui.tab_separator),
        surface: rgb(theme.ui.bar_bg),
        link: rgb(theme.ui.accent),
        accent: rgb(theme.ui.splitter),
    }
}

const NOT_PREVIEWABLE: &str = "巨大ファイルはプレビューできません";
const NO_DOCUMENT: &str = "プレビューする文書がありません";

impl App {
    /// プレビューの状態（初めてなら作る。ここでは WebView2 に何もしない）。
    fn preview_state(&mut self) -> &mut PreviewState {
        let palette = palette_for(&self.theme);
        self.preview.get_or_insert_with(|| PreviewState {
            host: PreviewHost::new(),
            pane: None,
            window: None,
            scheduler: PreviewScheduler::default(),
            stylesheet: Stylesheet::new(&crate::markdown::PreviewStyle {
                palette,
                ..Default::default()
            }),
            target: None,
            body: None,
            timer: false,
        })
    }

    /// アクティブなプレビュー対象の文書（なければ理由を状態表示に出して `None`）。
    fn markdown_target(&mut self) -> Option<DocumentId> {
        let doc = self.active.map(|i| &self.docs[i]);
        match doc {
            Some(d) if is_previewable(d) => Some(d.id()),
            other => {
                let msg = if other.is_some() {
                    NOT_PREVIEWABLE
                } else {
                    NO_DOCUMENT
                };
                self.set_status(msg, MessageKind::Info);
                None
            }
        }
    }

    /// 開く対象: アクティブな文書。アクティブが巨大文書などで対象外でも、もう一方の
    /// 表示（右ペイン／別ウィンドウ）が文書を表示中なら、それを引き継ぐ（切り替えのため）。
    fn preview_open_target(&mut self) -> Option<DocumentId> {
        let active_doc = self
            .active
            .map(|i| &self.docs[i])
            .filter(|d| is_previewable(d))
            .map(|d| d.id());
        let showing = self
            .preview
            .as_ref()
            .filter(|p| p.pane.is_some() || p.window.is_some())
            .and_then(|p| p.target)
            .filter(|&id| self.document(id).is_some());
        active_doc.or(showing).or_else(|| self.markdown_target())
    }

    /// アクティブな文書をプレビューできるか（メニューの有効・無効）。
    pub(super) fn preview_available(&self) -> bool {
        self.active
            .map(|i| &self.docs[i])
            .is_some_and(is_previewable)
    }

    pub fn preview_pane_open(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.pane.is_some())
    }

    pub fn preview_window_open(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.window.is_some())
    }

    // ---------------------------------------------------------------- 右ペイン

    /// 右ペインのプレビューを開く・閉じる。
    pub fn toggle_preview_pane(&mut self) -> Result<()> {
        if self.preview_pane_open() {
            self.close_preview_pane();
            return Ok(());
        }
        let Some(id) = self.preview_open_target() else {
            return Ok(());
        };
        let palette = palette_for(&self.theme);
        let hwnd = self.hwnd;
        let host = self.preview_state().host.clone();
        let pane = PreviewPane::create(&host, hwnd, &palette)?;
        let sender = self.sender();
        pane.set_on_status(move |status| {
            if let PaneStatus::Unavailable(reason) = status {
                let reason = reason.clone();
                sender.send(UiMessage::Run(Box::new(move |app| {
                    app.on_preview_unavailable(&reason)
                })));
            }
        });
        let pane_hwnd = pane.hwnd();
        let unavailable = match pane.status() {
            PaneStatus::Unavailable(reason) => Some(reason),
            _ => None,
        };
        self.preview_state().pane = Some(pane);
        self.set_right_pane(Some(pane_hwnd));
        self.preview_open_surface(id, Surface::Pane);
        // 右ペインと別ウィンドウはどちらか一方（開いてから閉じるので、本文はそのまま引き継ぐ）。
        if self.preview_window_open() {
            self.close_preview_window();
        }
        if let Some(reason) = unavailable {
            self.on_preview_unavailable(&reason);
        }
        Ok(())
    }

    fn close_preview_pane(&mut self) {
        if let Some(p) = self.preview.as_mut() {
            if let Some(pane) = p.pane.take() {
                pane.destroy();
            }
        }
        self.set_right_pane(None);
        self.preview_stop_if_idle();
        self.focus_active();
    }

    // ---------------------------------------------------------------- 別ウィンドウ

    /// 別ウィンドウのプレビューを開く・閉じる。
    pub fn toggle_preview_window(&mut self) -> Result<()> {
        if self.preview_window_open() {
            self.close_preview_window();
            return Ok(());
        }
        let Some(id) = self.preview_open_target() else {
            return Ok(());
        };
        let palette = palette_for(&self.theme);
        let hwnd = self.hwnd;
        let title = self
            .document(id)
            .map(|d| format!("プレビュー - {}", d.title))
            .unwrap_or_else(|| "プレビュー".to_owned());
        let host = self.preview_state().host.clone();
        let window = PreviewWindow::create(&host, hwnd, &title, &palette)?;
        let sender = self.sender();
        window.set_on_closed(move || {
            sender.send(UiMessage::Run(Box::new(|app| {
                app.on_preview_window_closed()
            })));
        });
        let sender = self.sender();
        if let Some(pane) = window.pane() {
            pane.set_on_status(move |status| {
                if let PaneStatus::Unavailable(reason) = status {
                    let reason = reason.clone();
                    sender.send(UiMessage::Run(Box::new(move |app| {
                        app.on_preview_unavailable(&reason)
                    })));
                }
            });
        }
        let unavailable = match window.status() {
            Some(PaneStatus::Unavailable(reason)) => Some(reason),
            _ => None,
        };
        window.show();
        self.preview_state().window = Some(window);
        self.preview_open_surface(id, Surface::Window);
        // 右ペインと別ウィンドウはどちらか一方（開いてから閉じるので、本文はそのまま引き継ぐ）。
        if self.preview_pane_open() {
            self.close_preview_pane();
        }
        if let Some(reason) = unavailable {
            self.on_preview_unavailable(&reason);
        }
        Ok(())
    }

    fn close_preview_window(&mut self) {
        if let Some(p) = self.preview.as_mut() {
            if let Some(w) = p.window.take() {
                w.destroy();
            }
        }
        self.preview_stop_if_idle();
        self.focus_active();
    }

    /// 別ウィンドウが閉じるボタンで隠された。
    fn on_preview_window_closed(&mut self) {
        self.close_preview_window();
    }

    /// 右ペインも別ウィンドウもなくなったら、変換の予定と進行中の変換をやめる。
    fn preview_stop_if_idle(&mut self) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        if p.pane.is_some() || p.window.is_some() {
            return;
        }
        p.scheduler.cancel_all();
        p.target = None;
        p.body = None;
        if p.timer {
            p.timer = false;
            // SAFETY: 自分のタイマーを止める。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
            }
        }
    }

    /// WebView2 を使えない（ランタイムがないなど）。案内を出す。編集は続けられる。
    fn on_preview_unavailable(&mut self, reason: &str) {
        let first_line = reason.lines().next().unwrap_or(reason);
        self.set_status(
            &format!(
                "プレビューには Microsoft Edge WebView2 ランタイムが必要です（{first_line}）。\
                 Evergreen 版をインストールしてから開き直してください。編集は続けられます"
            ),
            MessageKind::Warning,
        );
    }

    // ---------------------------------------------------------------- 変換

    /// 右ペインか別ウィンドウを新しく開いた。もう一方が同じ文書を表示済みなら、その本文を
    /// 新しい方にも渡す（同じ版は変換し直さないので、渡さないと空のままになる）。
    /// それ以外は `id` を対象にして変換を始める。
    fn preview_open_surface(&mut self, id: DocumentId, surface: Surface) {
        let title = self
            .document(id)
            .map(|d| d.title.clone())
            .unwrap_or_default();
        let shown = self.preview.as_ref().and_then(|p| {
            let body = p.body.as_ref().filter(|_| p.target == Some(id))?;
            Some(Arc::<str>::from(compose_document(
                body,
                &p.stylesheet,
                &title,
            )))
        });
        match shown {
            Some(html) => {
                let Some(p) = self.preview.as_ref() else {
                    return;
                };
                match surface {
                    Surface::Pane => {
                        if let Some(pane) = &p.pane {
                            pane.set_document(id, html);
                        }
                    }
                    Surface::Window => {
                        if let Some(w) = &p.window {
                            w.set_document(id, html);
                        }
                    }
                }
            }
            None => self.preview_retarget(id),
        }
    }

    /// プレビューの対象を `id` にして、すぐに変換を始める。
    fn preview_retarget(&mut self, id: DocumentId) {
        let Some(stamp) = self.state(id).map(|s| s.stamp()) else {
            return;
        };
        let hwnd = self.hwnd;
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        p.target = Some(id);
        p.scheduler.request_now(stamp, Instant::now());
        // 前の文書の表示は、新しい文書の結果が届くまで残す。
        p.timer = true;
        // SAFETY: 自分のウィンドウのタイマー。
        unsafe {
            SetTimer(Some(hwnd), TIMER_PREVIEW, 10, None);
        }
    }

    /// アクティブな文書が替わった。プレビュー対象の文書なら追従させる（巨大文書では追従せず、直前の表示を保つ）。
    pub(super) fn preview_active_changed(&mut self, id: DocumentId) {
        let following = self
            .preview
            .as_ref()
            .is_some_and(|p| p.pane.is_some() || p.window.is_some());
        if !following || self.preview.as_ref().and_then(|p| p.target) == Some(id) {
            return;
        }
        if self.document(id).is_some_and(follows_on_switch) {
            self.preview_retarget(id);
            self.retitle_preview_window(id);
        }
    }

    fn retitle_preview_window(&mut self, id: DocumentId) {
        let title = self
            .document(id)
            .map(|d| format!("プレビュー - {}", d.title))
            .unwrap_or_else(|| "プレビュー".to_owned());
        if let Some(w) = self.preview.as_ref().and_then(|p| p.window.as_ref()) {
            w.set_title(&title);
        }
    }

    /// 文書の本文が変わった（[`App::sync`] から）。対象の文書なら、入力が落ち着いてから変換する。
    pub(super) fn preview_note_change(&mut self, id: DocumentId) {
        if self.preview.as_ref().and_then(|p| p.target) != Some(id) {
            return;
        }
        let Some(stamp) = self.state(id).map(|s| s.stamp()) else {
            return;
        };
        let len = self.with_text(id, |ed| ed.length()).unwrap_or(0) as usize;
        let hwnd = self.hwnd;
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        p.scheduler.note_change(stamp, len, Instant::now());
        let delay = p.scheduler.delay_for(len);
        p.timer = true;
        // SAFETY: 自分のウィンドウのタイマー（同じ ID なら置き換わる）。
        unsafe {
            SetTimer(
                Some(hwnd),
                TIMER_PREVIEW,
                delay.as_millis().clamp(10, 2000) as u32,
                None,
            );
        }
    }

    /// 文書を閉じた。対象なら変換をやめる。
    pub(super) fn preview_document_closed(&mut self, id: DocumentId) {
        if self.preview.as_ref().and_then(|p| p.target) == Some(id) {
            if let Some(p) = self.preview.as_mut() {
                p.scheduler.cancel_all();
                p.target = None;
                p.body = None;
            }
        }
    }

    /// タイマー: 待ち時間が過ぎていれば、本文の写しをワーカーで変換させる。
    pub(super) fn preview_tick(&mut self) {
        let hwnd = self.hwnd;
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        // SAFETY: 自分のタイマーを止める（必要なら下で掛け直す）。
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_PREVIEW);
        }
        p.timer = false;
        let now = Instant::now();
        let ticket = p.scheduler.take_due(now);
        let next = p.scheduler.next_due();
        if let Some(due) = next {
            let wait = due
                .saturating_duration_since(now)
                .max(Duration::from_millis(10));
            p.timer = true;
            // SAFETY: 自分のウィンドウのタイマー。
            unsafe {
                SetTimer(
                    Some(hwnd),
                    TIMER_PREVIEW,
                    wait.as_millis().min(2000) as u32,
                    None,
                );
            }
        }
        let Some(ticket) = ticket else { return };
        let id = ticket.stamp.document;
        let snapshot: Result<(DocStamp, String)> = self.text_snapshot(id);
        let (stamp, text) = match snapshot {
            Ok(r) => r,
            Err(_) => {
                ticket.cancel.cancel();
                return;
            }
        };
        if stamp != ticket.stamp {
            // 予定を立てたあとに版が進んだ（次の予定が立っている）。
            ticket.cancel.cancel();
            return;
        }
        let limits = Limits {
            max_items: None,
            max_bytes: Some(DEFAULT_MAX_INPUT_BYTES),
        };
        spawn_render(
            WorkerPool::global(),
            ticket,
            text,
            limits,
            self.sender(),
            |update| UiMessage::Run(Box::new(move |app: &mut App| app.on_preview_update(update))),
        );
    }

    /// 変換の結果を照合して表示する。
    fn on_preview_update(&mut self, update: PreviewUpdate) {
        let current = self.state(update.stamp.document).map(|s| s.version());
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let Some(accepted) = p.scheduler.accept(update, current) else {
            return;
        };
        match accepted.result {
            Ok(body) => {
                p.body = Some(body);
                self.show_preview_body(accepted.stamp.document);
            }
            Err(e) => self.set_status(
                &format!("プレビューを作れませんでした: {e}"),
                MessageKind::Warning,
            ),
        }
    }

    /// 持っている本文から HTML を組んで、右ペインと別ウィンドウに渡す。
    fn show_preview_body(&mut self, id: DocumentId) {
        let title = self
            .document(id)
            .map(|d| d.title.clone())
            .unwrap_or_default();
        let Some(p) = self.preview.as_ref() else {
            return;
        };
        let Some(body) = p.body.as_ref() else { return };
        let html: Arc<str> = Arc::from(compose_document(body, &p.stylesheet, &title));
        if let Some(pane) = &p.pane {
            pane.set_document(id, html.clone());
        }
        if let Some(w) = &p.window {
            w.set_document(id, html);
        }
    }

    // ---------------------------------------------------------------- テーマ

    /// 明暗が替わった。配色を渡し、見た目を組み直す。
    pub(super) fn preview_theme_changed(&mut self) {
        let palette = palette_for(&self.theme);
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        p.stylesheet = Stylesheet::new(&crate::markdown::PreviewStyle {
            palette: palette.clone(),
            ..Default::default()
        });
        if let Some(pane) = &p.pane {
            pane.set_palette(&palette);
        }
        if let Some(w) = &p.window {
            w.set_palette(&palette);
        }
        if let Some(id) = p.target {
            self.show_preview_body(id);
        }
    }

    /// 終了の前に、WebView2 を閉じる。
    pub(super) fn preview_teardown(&mut self) {
        if let Some(mut p) = self.preview.take() {
            p.scheduler.cancel_all();
            if let Some(w) = p.window.take() {
                w.destroy();
            }
            if let Some(pane) = p.pane.take() {
                pane.destroy();
            }
            // SAFETY: 自分のタイマーを止める。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
            }
        }
    }
}

/// プレビューを出す場所。
#[derive(Debug, Clone, Copy)]
enum Surface {
    Pane,
    Window,
}
