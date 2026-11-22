//! タブの独自描画。未保存は小さな丸で示す（[`crate::document::DocumentState::is_dirty`]）。
//! AI の変更は印で示す（[`crate::document::DocumentState::has_ai_edits`]）。
//!
//! - [`TabBar`] がタブの一覧（表示用の写し）と、マウスの状態、配置を持つ。
//!   文書の状態そのものは [`crate::ui::app`] が持ち、変わるたびに
//!   [`TabBar::set_items`] で写しを渡す。
//! - 選択中のタブは本文と同じ背景にし、上端にアクセント色の細い線を引く（控えめな強調）。
//! - 未保存の丸は閉じるボタンの位置に出し、マウスが乗ると閉じるボタン（×）に替わる。
//! - 中クリックで閉じる。タブのないところのダブルクリックで新しいタブ。
//!   どちらも [`TabHit`] を返すだけで、実際の操作は `ui::app` がコマンドとして実行する。
//!
//! 担当: タスク B。

use windows::Win32::Foundation::RECT;

use crate::core::DocumentId;
use crate::ui::theme::Theme;
use crate::ui::window::{BufferedPaint, UiFont};

/// タブの寸法（96 DPI の値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabMetrics {
    pub pad_left: i32,
    pub pad_right: i32,
    /// 文字と閉じるボタンの間。
    pub gap: i32,
    /// 閉じるボタンの一辺。
    pub close: i32,
    pub min_width: i32,
    pub max_width: i32,
    /// 未保存の丸の直径。
    pub dot: i32,
    /// 選択中のタブの上端の線の太さ。
    pub accent: i32,
    /// メモのしるし（紙のアイコン）の幅・高さと、題名との間。
    pub icon_w: i32,
    pub icon_h: i32,
    pub icon_gap: i32,
}

impl TabMetrics {
    pub const DIP: TabMetrics = TabMetrics {
        pad_left: 12,
        pad_right: 6,
        gap: 6,
        close: 18,
        min_width: 72,
        max_width: 220,
        dot: 8,
        accent: 2,
        icon_w: 10,
        icon_h: 13,
        icon_gap: 5,
    };

    pub fn scaled(dpi: u32) -> TabMetrics {
        let s = |v| crate::platform::system::scale(v, dpi);
        let d = Self::DIP;
        TabMetrics {
            pad_left: s(d.pad_left),
            pad_right: s(d.pad_right),
            gap: s(d.gap),
            close: s(d.close),
            min_width: s(d.min_width),
            max_width: s(d.max_width),
            dot: s(d.dot),
            accent: s(d.accent).max(1),
            icon_w: s(d.icon_w),
            icon_h: s(d.icon_h),
            icon_gap: s(d.icon_gap),
        }
    }

    /// メモのしるしが題名の左に取る幅。
    pub fn memo_extra(&self) -> i32 {
        self.icon_w + self.icon_gap
    }

    /// 題名に使える幅 `avail` で、メモのしるしを出してよいか。しるしは補助なので、題名が最低でも
    /// しるし 2 つ分（全角 1〜2 文字）は残るときだけ出す（タブが縮んだときは題名を優先する）。
    pub fn memo_icon_fits(&self, avail: i32) -> bool {
        avail >= self.memo_extra() + 2 * self.icon_w
    }

    /// 文字の幅 `text_width` のタブの自然な幅。
    pub fn natural_width(&self, text_width: i32) -> i32 {
        (self.pad_left + text_width + self.gap + self.close + self.pad_right)
            .clamp(self.min_width, self.max_width)
    }
}

/// タブ 1 つの表示内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabItem {
    pub id: DocumentId,
    pub title: String,
    pub dirty: bool,
    pub ai_edits: bool,
    /// 読み取り専用（巨大ファイルなど）。選択中でも題名を控えめな色にする。
    pub read_only: bool,
    /// メモ（題名の左に紙のしるしを描く）。
    pub memo: bool,
}

/// マウスの位置にあるもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    /// タブのないところ。
    Empty,
    /// 新しいタブのボタン（＋）。
    New,
    /// 見えないタブがある場合に表示する一覧ボタン。
    List,
}

/// 各タブの横の位置（`None` は、あふれて見えないタブ）。
///
/// 自然な幅の合計が `avail` に収まればそのまま並べる。収まらなければ長いタブから縮め、
/// それでも最小幅を下回るなら、選択中のタブが見えるように `scroll`（先頭に見せるタブ）を
/// 動かして、見える分だけを並べる。
pub fn layout(
    natural: &[i32],
    avail: i32,
    metrics: &TabMetrics,
    active: Option<usize>,
    scroll: &mut usize,
) -> Vec<Option<(i32, i32)>> {
    let n = natural.len();
    let mut out = vec![None; n];
    if n == 0 || avail <= 0 {
        *scroll = 0;
        return out;
    }
    let total: i64 = natural.iter().map(|&w| i64::from(w)).sum();
    let fit = (avail / metrics.min_width.max(1)).max(1) as usize;
    if total <= i64::from(avail) || fit >= n {
        *scroll = 0;
        let widths = fit_widths(natural, avail);
        let mut x = 0;
        for (slot, w) in out.iter_mut().zip(widths) {
            *slot = Some((x, w));
            x += w;
        }
        return out;
    }
    if let Some(a) = active {
        if a < *scroll {
            *scroll = a;
        } else if a >= *scroll + fit {
            *scroll = a + 1 - fit;
        }
    }
    *scroll = (*scroll).min(n - fit);
    let mut x = 0;
    for (k, w) in fit_widths(&natural[*scroll..*scroll + fit], avail)
        .into_iter()
        .enumerate()
    {
        out[*scroll + k] = Some((x, w));
        x += w;
    }
    out
}

/// 短い題名の自然幅を残し、長い題名だけを共通の上限まで縮める。
/// 端数も配り、1px の狭い領域でも合計幅が領域を超えない。
fn fit_widths(natural: &[i32], avail: i32) -> Vec<i32> {
    let mut low = 0;
    let mut high = natural.iter().copied().max().unwrap_or(0).max(0);
    while low < high {
        let cap = low + (high - low + 1) / 2;
        let total: i64 = natural.iter().map(|&w| i64::from(w.min(cap))).sum();
        if total <= i64::from(avail) {
            low = cap;
        } else {
            high = cap - 1;
        }
    }
    let mut widths: Vec<_> = natural.iter().map(|&w| w.min(low)).collect();
    let mut remainder = avail - widths.iter().sum::<i32>();
    for (&natural, width) in natural.iter().zip(&mut widths) {
        if remainder > 0 && *width < natural {
            *width += 1;
            remainder -= 1;
        }
    }
    widths
}

/// タブバーの状態。
#[derive(Debug, Default)]
pub struct TabBar {
    items: Vec<TabItem>,
    active: Option<usize>,
    hover: Option<TabHit>,
    /// 閉じるボタンを押している途中のタブ。
    pressed_close: Option<usize>,
    scroll: usize,
    /// 最後に計算した配置（バーの座標）。
    positions: Vec<Option<(i32, i32)>>,
    bar: RECT,
    dpi: u32,
}

impl TabBar {
    pub fn new() -> TabBar {
        TabBar {
            dpi: 96,
            ..Default::default()
        }
    }

    pub fn items(&self) -> &[TabItem] {
        &self.items
    }

    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// 一覧と選択中のタブを差し替える。変わったら `true`（描き直しが要る）。
    pub fn set_items(&mut self, items: Vec<TabItem>, active: Option<usize>) -> bool {
        let changed = items != self.items || active != self.active;
        if items.len() != self.items.len() {
            self.hover = None;
            self.pressed_close = None;
        }
        self.items = items;
        self.active = active.filter(|&a| a < self.items.len());
        changed
    }

    /// 配置を計算し直す。`measure` は題名の文字の幅（ピクセル）。
    pub fn relayout(&mut self, bar: RECT, dpi: u32, measure: &dyn Fn(&str) -> i32) {
        self.bar = bar;
        self.dpi = dpi;
        let m = TabMetrics::scaled(dpi);
        let natural: Vec<i32> = self
            .items
            .iter()
            .map(|t| {
                let extra = if t.memo { m.memo_extra() } else { 0 };
                m.natural_width(measure(&t.title) + extra)
            })
            .collect();
        // 「＋」と、あふれるときだけ一覧ボタンの場所を空ける。
        let width = bar.right - bar.left;
        let available = width - self.new_button_width();
        let list_width = self.list_button_width();
        let overflow = natural.len() > (available / m.min_width.max(1)).max(1) as usize;
        let reserved = if overflow { list_width } else { 0 };
        self.positions = layout(
            &natural,
            (available - reserved).max(0),
            &m,
            self.active,
            &mut self.scroll,
        );
    }

    /// タブ `i` の矩形（クライアント座標）。見えなければ `None`。
    pub fn tab_rect(&self, i: usize) -> Option<RECT> {
        let (x, w) = (*self.positions.get(i)?)?;
        Some(RECT {
            left: self.bar.left + x,
            top: self.bar.top,
            right: self.bar.left + x + w,
            bottom: self.bar.bottom,
        })
    }

    /// 「＋」の幅。極端に狭いバーでは出さない（タブの選択領域を優先する）。
    fn new_button_width(&self) -> i32 {
        let w = crate::platform::system::scale(36, self.dpi);
        if self.bar.right - self.bar.left < w * 4 {
            0
        } else {
            w
        }
    }

    /// 「＋」のボタンの矩形（最後に見えているタブの右）。
    pub fn new_button_rect(&self) -> RECT {
        let right_of_tabs = (0..self.items.len())
            .filter_map(|i| self.tab_rect(i))
            .map(|r| r.right)
            .max()
            .unwrap_or(self.bar.left);
        let w = self.new_button_width();
        if w == 0 {
            return RECT::default();
        }
        let end = self.bar.right
            - if self.has_hidden_tabs() {
                self.list_button_width()
            } else {
                0
            };
        let left = right_of_tabs.min(end - w).max(self.bar.left);
        let inset = crate::platform::system::scale(4, self.dpi);
        RECT {
            left: left + inset,
            right: left + w - inset,
            top: self.bar.top + inset,
            bottom: self.bar.bottom - inset - 1,
        }
    }

    fn has_hidden_tabs(&self) -> bool {
        self.positions.first().is_some_and(Option::is_none)
            || self.positions.last().is_some_and(Option::is_none)
    }

    fn list_button_width(&self) -> i32 {
        let width = crate::platform::system::scale(36, self.dpi);
        if self.bar.right - self.bar.left < width * 2 {
            0
        } else {
            width
        }
    }

    pub fn list_button_rect(&self) -> Option<RECT> {
        let width = self.list_button_width();
        if width == 0 || !self.has_hidden_tabs() {
            return None;
        }
        let inset = crate::platform::system::scale(4, self.dpi);
        Some(RECT {
            left: self.bar.right - width + inset,
            right: self.bar.right - inset,
            top: self.bar.top + inset,
            bottom: self.bar.bottom - inset - 1,
        })
    }

    fn close_rect(&self, tab: &RECT) -> RECT {
        let m = TabMetrics::scaled(self.dpi);
        // 極端な狭幅では閉じるボタンを隠し、タブの選択領域を確保する。
        let size = m.close.min((tab.bottom - tab.top).max(0));
        let width = (tab.right - tab.left).max(0);
        if width < m.pad_left + size + m.pad_right {
            return RECT {
                left: tab.right,
                right: tab.right,
                top: tab.top,
                bottom: tab.top,
            };
        }
        let cy = (tab.top + tab.bottom) / 2;
        let right = tab.right - m.pad_right;
        RECT {
            left: right - size,
            top: cy - size / 2,
            right,
            bottom: cy - size / 2 + size,
        }
    }

    /// クライアント座標の点にあるもの。バーの外なら `None`。
    pub fn hit_test(&self, x: i32, y: i32) -> Option<TabHit> {
        if !crate::ui::window::contains(&self.bar, x, y) {
            return None;
        }
        let new_button = self.new_button_rect();
        if new_button.right > new_button.left && crate::ui::window::contains(&new_button, x, y) {
            return Some(TabHit::New);
        }
        if self
            .list_button_rect()
            .is_some_and(|rect| crate::ui::window::contains(&rect, x, y))
        {
            return Some(TabHit::List);
        }
        for i in 0..self.items.len() {
            if let Some(r) = self.tab_rect(i) {
                if crate::ui::window::contains(&r, x, y) {
                    let c = self.close_rect(&r);
                    return Some(if crate::ui::window::contains(&c, x, y) {
                        TabHit::Close(i)
                    } else {
                        TabHit::Tab(i)
                    });
                }
            }
        }
        Some(TabHit::Empty)
    }

    /// マウスの位置にあるもの。
    pub fn hover(&self) -> Option<TabHit> {
        self.hover
    }

    /// マウスが動いた。描き直しが要れば `true`。
    pub fn set_hover(&mut self, hit: Option<TabHit>) -> bool {
        let changed = hit != self.hover;
        self.hover = hit;
        changed
    }

    pub fn press_close(&mut self, i: Option<usize>) {
        self.pressed_close = i;
    }

    pub fn pressed_close(&self) -> Option<usize> {
        self.pressed_close
    }

    pub fn bar_rect(&self) -> RECT {
        self.bar
    }

    /// タブ `from` をクライアント座標の x に合わせて動かすときの新しい番号。
    pub fn reorder_target(&self, from: usize, x: i32) -> usize {
        reorder_target(&self.positions, from, x - self.bar.left)
    }

    /// バー全体を描く。
    pub fn paint(&self, target: windows::Win32::Graphics::Gdi::HDC, theme: &Theme, font: &UiFont) {
        let Some(buf) = BufferedPaint::new(target, self.bar) else {
            return;
        };
        let c = buf.canvas(self.dpi);
        let ui = &theme.ui;
        let local = buf.local_rect();
        c.fill(&local, ui.bar_bg);
        c.set_font(font);
        let m = TabMetrics::scaled(self.dpi);
        let offset = |r: RECT| RECT {
            left: r.left - self.bar.left,
            top: r.top - self.bar.top,
            right: r.right - self.bar.left,
            bottom: r.bottom - self.bar.top,
        };
        // 下端の区切り線（選択中のタブのところでは途切れ、本文とつながる）。
        c.fill(
            &RECT {
                top: local.bottom - 1,
                ..local
            },
            ui.tab_separator,
        );
        if let Some(rect) = self.list_button_rect() {
            let b = offset(rect);
            if self.hover == Some(TabHit::List) {
                c.fill(&b, ui.tab_hover_bg);
            }
            let (cx, cy) = ((b.left + b.right) / 2, (b.top + b.bottom) / 2);
            let step = c.px(1).max(1);
            for row in 0..5 {
                let half = (4 - row) * step;
                c.fill(
                    &RECT {
                        left: cx - half,
                        right: cx + half + step,
                        top: cy + (row - 2) * step,
                        bottom: cy + (row - 1) * step,
                    },
                    ui.tab_fg,
                );
            }
        }
        // 「＋」のボタン（ふだんは記号だけ、マウスが乗ると背景）。
        if self.new_button_width() > 0 {
            let b = offset(self.new_button_rect());
            if self.hover == Some(TabHit::New) {
                c.fill(&b, ui.tab_hover_bg);
            }
            let (cx, cy) = ((b.left + b.right) / 2, (b.top + b.bottom) / 2);
            let half = c.px(5);
            let t = c.px(1).max(1);
            c.fill(
                &RECT {
                    left: cx - half,
                    right: cx + half + 1,
                    top: cy - t / 2,
                    bottom: cy - t / 2 + t,
                },
                ui.tab_fg,
            );
            c.fill(
                &RECT {
                    left: cx - t / 2,
                    right: cx - t / 2 + t,
                    top: cy - half,
                    bottom: cy + half + 1,
                },
                ui.tab_fg,
            );
        }
        for (i, item) in self.items.iter().enumerate() {
            let Some(r) = self.tab_rect(i) else { continue };
            let r = offset(r);
            let active = self.active == Some(i);
            let hovered = matches!(self.hover, Some(TabHit::Tab(h) | TabHit::Close(h)) if h == i);
            if active {
                c.fill(&r, ui.tab_active_bg);
                c.fill(
                    &RECT {
                        bottom: r.top + m.accent,
                        ..r
                    },
                    ui.accent,
                );
            } else {
                if hovered {
                    c.fill(
                        &RECT {
                            bottom: r.bottom - 1,
                            ..r
                        },
                        ui.tab_hover_bg,
                    );
                }
                // 右端の区切り（選択中のタブの左右は背景の差で分かるので引かない）。
                let next_active = self.active == Some(i + 1);
                if !next_active {
                    let inset = c.px(8);
                    c.fill(
                        &RECT {
                            left: r.right - 1,
                            right: r.right,
                            top: r.top + inset,
                            bottom: r.bottom - inset,
                        },
                        ui.tab_separator,
                    );
                }
            }
            let close = self.close_rect(&r);
            let text_left = (r.left + m.pad_left).min(r.right);
            let has_close = close.right > close.left;
            let text_right = if has_close {
                close.left - m.gap
            } else {
                r.right - m.pad_right
            };
            let fg = if active && !item.read_only {
                ui.tab_active_fg
            } else {
                ui.tab_fg
            };
            // メモは題名の左に紙のしるし（文字と同じ色。テーマに従う）。
            let mut title_left = text_left;
            if item.memo && m.memo_icon_fits(text_right - text_left) {
                draw_memo_icon(&c, text_left, (r.top + r.bottom) / 2, &m, fg);
                title_left += m.memo_extra();
            }
            let text_rect = RECT {
                left: title_left,
                right: text_right.max(title_left),
                ..r
            };
            c.text(&text_rect, &item.title, fg, false);
            if item.ai_edits {
                // AI の変更: 題名の左上の小さな印。
                let d = (m.dot * 3 / 4).max(3);
                c.dot(r.left + m.pad_left / 2, r.top + c.px(9), d, ui.ai_mark);
            }
            let close_hovered = self.hover == Some(TabHit::Close(i));
            let show_close = close_hovered || hovered || (active && !item.dirty);
            if close_hovered {
                c.fill(&close, ui.close_hover_bg);
            }
            if show_close && has_close {
                let cx = (close.left + close.right) / 2;
                let cy = (close.top + close.bottom) / 2;
                let k = c.px(4);
                let color = if active || close_hovered {
                    ui.tab_active_fg
                } else {
                    ui.tab_fg
                };
                let pen = c.px(1).max(1);
                c.line(cx - k, cy - k, cx + k + 1, cy + k + 1, pen, color);
                c.line(cx - k, cy + k, cx + k + 1, cy - k - 1, pen, color);
            } else if item.dirty && has_close {
                let cx = (close.left + close.right) / 2;
                let cy = (close.top + close.bottom) / 2;
                c.dot(cx, cy, m.dot, ui.dirty);
            }
        }
    }
}

/// メモのしるし: 罫線の入った小さな紙。`x` は左端、`cy` は縦の中心。色は題名と同じ。
fn draw_memo_icon(
    c: &crate::ui::window::Canvas,
    x: i32,
    cy: i32,
    m: &TabMetrics,
    color: crate::ui::theme::Rgb,
) {
    let t = c.px(1).max(1);
    let (w, h) = (m.icon_w, m.icon_h);
    let top = cy - h / 2;
    let (right, bottom) = (x + w, top + h);
    let edge = |l: i32, tp: i32, r: i32, b: i32| {
        c.fill(
            &RECT {
                left: l,
                top: tp,
                right: r,
                bottom: b,
            },
            color,
        );
    };
    // 枠。
    edge(x, top, right, top + t);
    edge(x, bottom - t, right, bottom);
    edge(x, top, x + t, bottom);
    edge(right - t, top, right, bottom);
    // 罫線 2 本。
    let inset = t + c.px(2);
    let (l, r) = (x + inset, right - inset);
    let y1 = top + h / 3;
    let y2 = top + h * 2 / 3;
    edge(l, y1, r, y1 + t);
    edge(l, y2, r, y2 + t);
}

// ------------------------------------------------------------------ ドラッグ（U4）

/// ドラッグを始める移動量（96 DPI の値）。これ未満の動きはクリックとして扱う。
pub const DRAG_THRESHOLD_DIP: i32 = 5;

/// 押したタブのドラッグの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDrag {
    pub id: DocumentId,
    /// 押した位置（クライアント座標）。
    pub start: (i32, i32),
    /// しきい値を超えて、ドラッグが始まった。
    pub started: bool,
}

impl TabDrag {
    pub fn new(id: DocumentId, start: (i32, i32)) -> TabDrag {
        TabDrag {
            id,
            start,
            started: false,
        }
    }

    /// マウスが動いた。しきい値（ピクセル）を超えたらドラッグ開始とし、始まっていれば `true`。
    pub fn update(&mut self, at: (i32, i32), threshold: i32) -> bool {
        if !self.started {
            let (dx, dy) = (at.0 - self.start.0, at.1 - self.start.1);
            self.started = dx.abs() > threshold || dy.abs() > threshold;
        }
        self.started
    }
}

/// 離した位置の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragEnd {
    /// ウィンドウの中: 並べ替えは動かしている間に済んでいるので、そのまま。
    Stay,
    /// ウィンドウの外: 新しいウィンドウで開く。
    Detach,
}

/// 離したカーソル位置（画面座標）が、ウィンドウ全体の矩形（画面座標）の外か。
pub fn drag_end(cursor: (i32, i32), window: &RECT) -> DragEnd {
    let inside = cursor.0 >= window.left
        && cursor.0 < window.right
        && cursor.1 >= window.top
        && cursor.1 < window.bottom;
    if inside {
        DragEnd::Stay
    } else {
        DragEnd::Detach
    }
}

/// タブ `from` をカーソルの x（バーの左端からの距離）に合わせて動かしたときの新しい番号。
///
/// すべてのタブが見えているときは、`from` 以外の各タブの中心と比べて決める
/// （幅の違うタブでも行き来が起きない）。あふれて見えないタブがあるときは、
/// カーソルの下にあるタブの番号（左右の外では見えている端のタブ）にする。
pub fn reorder_target(positions: &[Option<(i32, i32)>], from: usize, x: i32) -> usize {
    let n = positions.len();
    if from >= n {
        return from;
    }
    if positions.iter().all(Option::is_some) {
        // 現在の並びでの、`from` 以外のタブの中心を越えた数。動かした後も同じカーソル位置
        // なら同じ結果になり（行き来しない）、戻るには反対側の中心を越える必要がある。
        return positions
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != from)
            .filter(|(_, p)| p.is_some_and(|(px, w)| px + w / 2 < x))
            .count();
    }
    let visible: Vec<(usize, (i32, i32))> = positions
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.map(|p| (i, p)))
        .collect();
    let (Some(first), Some(last)) = (visible.first(), visible.last()) else {
        return from;
    };
    if x < first.1 .0 {
        return first.0;
    }
    if x >= last.1 .0 + last.1 .1 {
        return last.0;
    }
    visible
        .iter()
        .find(|(_, (px, w))| x >= *px && x < *px + *w)
        .map_or(from, |(i, _)| *i)
}

/// `v` の `from` 番目を `to` 番目へ動かす（範囲外は何もしない）。動かしたら `true`。
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= v.len() || to >= v.len() || from == to {
        return false;
    }
    let item = v.remove(from);
    v.insert(to, item);
    true
}

/// 並べ替えの後の、選択中のタブの番号。
pub fn active_after_move(active: Option<usize>, from: usize, to: usize) -> Option<usize> {
    active.map(|a| {
        if a == from {
            to
        } else if from < to && a > from && a <= to {
            a - 1
        } else if to < from && a >= to && a < from {
            a + 1
        } else {
            a
        }
    })
}

// ------------------------------------------------------------------ 右クリックメニュー（U4）

/// タブの右クリックメニューの項目（`None` は区切り線）。
pub const TAB_MENU: &[Option<(&str, &str)>] = &[
    Some(("file.close", "閉じる")),
    Some(("file.close_others", "ほかのタブを閉じる")),
    Some(("file.close_right", "右側のタブを閉じる")),
    None,
    Some(("file.save_as_memo", "メモとして保存")),
    Some(("file.move_to_new_window", "新しいウィンドウに移動")),
    None,
    Some(("file.copy_path", "フルパスをコピー")),
    Some(("file.reveal_in_explorer", "エクスプローラーで表示")),
];

/// 右クリックしたタブ（`index`／全 `count` 個、ファイルのパスの有無 `has_path`）で、
/// コマンドが使えるか。
pub fn tab_menu_enabled(command: &str, index: usize, count: usize, has_path: bool) -> bool {
    match command {
        "file.close_others" | "file.move_to_new_window" => count > 1,
        "file.close_right" => index + 1 < count,
        "file.copy_path" | "file.reveal_in_explorer" => has_path,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: TabMetrics = TabMetrics::DIP;

    #[test]
    fn natural_width_is_clamped() {
        assert_eq!(M.natural_width(0), M.min_width);
        assert_eq!(M.natural_width(10_000), M.max_width);
        assert_eq!(M.natural_width(60), 12 + 60 + 6 + 18 + 6);
    }

    #[test]
    fn tabs_that_fit_keep_their_natural_width() {
        let mut scroll = 3;
        let pos = layout(&[100, 120, 80], 400, &M, Some(2), &mut scroll);
        assert_eq!(pos, vec![Some((0, 100)), Some((100, 120)), Some((220, 80))]);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn tabs_shrink_evenly_before_overflowing() {
        let mut scroll = 0;
        let pos = layout(&[200, 200, 200], 450, &M, Some(0), &mut scroll);
        assert_eq!(
            pos,
            vec![Some((0, 150)), Some((150, 150)), Some((300, 150))]
        );
    }

    #[test]
    fn short_names_keep_their_width_while_long_names_shrink() {
        let mut scroll = 0;
        let pos = layout(&[72, 220, 220], 400, &M, Some(0), &mut scroll);
        assert_eq!(pos, vec![Some((0, 72)), Some((72, 164)), Some((236, 164))]);
        let pos = layout(&[200, 200, 200], 451, &M, Some(0), &mut scroll);
        assert_eq!(
            pos,
            vec![Some((0, 151)), Some((151, 150)), Some((301, 150))]
        );
    }

    #[test]
    fn dpi_long_names_and_tab_counts_stay_inside_the_bar() {
        for dpi in [96, 144, 192] {
            let m = TabMetrics::scaled(dpi);
            for count in [1, 2, 8, 30] {
                let names = vec![m.max_width; count];
                for avail in [1, 30, 100, 680, 1600] {
                    let mut scroll = 0;
                    let pos = layout(&names, avail, &m, Some(count - 1), &mut scroll);
                    assert!(pos[count - 1].is_some());
                    for (x, width) in pos.into_iter().flatten() {
                        assert!(x >= 0 && width > 0 && x + width <= avail);
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_tabs_hide_the_close_button_and_keep_selection_clickable() {
        let mut bar = TabBar::new();
        bar.set_items(vec![item(1, "長い日本語のファイル名.txt")], Some(0));
        bar.relayout(
            RECT {
                left: 0,
                top: 0,
                right: 20,
                bottom: 24,
            },
            96,
            &|_| 400,
        );
        let r = bar.tab_rect(0).unwrap();
        let close = bar.close_rect(&r);
        assert_eq!(close.left, close.right);
        assert_eq!(bar.hit_test(5, 10), Some(TabHit::Tab(0)));
        assert_eq!(bar.hit_test(19, 10), Some(TabHit::Tab(0)));
    }

    #[test]
    fn overflow_keeps_the_active_tab_visible() {
        let natural = vec![100; 10];
        let mut scroll = 0;
        // 最小幅 72 で 300 に収まるのは 4 つ。
        let pos = layout(&natural, 300, &M, Some(7), &mut scroll);
        assert_eq!(scroll, 4);
        assert!(pos[7].is_some());
        assert_eq!(pos.iter().filter(|p| p.is_some()).count(), 4);
        assert_eq!(pos[4], Some((0, 75)));
        // 左へ戻ると先頭も動く。
        let pos = layout(&natural, 300, &M, Some(1), &mut scroll);
        assert_eq!(scroll, 1);
        assert!(pos[1].is_some() && pos[0].is_none());
        // 末尾を超えてスクロールしない。
        scroll = 9;
        layout(&natural, 300, &M, None, &mut scroll);
        assert_eq!(scroll, 6);
    }

    #[test]
    fn empty_and_degenerate_layouts() {
        let mut scroll = 5;
        assert!(layout(&[], 300, &M, None, &mut scroll).is_empty());
        assert_eq!(scroll, 0);
        let pos = layout(&[100, 100], 0, &M, Some(0), &mut scroll);
        assert_eq!(pos, vec![None, None]);
        // 1 つも収まらない幅でも、選択中の 1 つは出す。
        let pos = layout(&[100, 100], 30, &M, Some(1), &mut scroll);
        assert_eq!(pos, vec![None, Some((0, 30))]);
    }

    fn item(n: u64, title: &str) -> TabItem {
        TabItem {
            id: DocumentId::from_raw(n),
            title: title.into(),
            dirty: false,
            ai_edits: false,
            read_only: false,
            memo: false,
        }
    }

    #[test]
    fn hit_testing_finds_tabs_and_close_buttons() {
        let mut bar = TabBar::new();
        bar.set_items(vec![item(1, "a"), item(2, "b")], Some(0));
        bar.relayout(
            RECT {
                left: 0,
                top: 0,
                right: 500,
                bottom: 32,
            },
            96,
            &|s| s.len() as i32 * 8,
        );
        let r0 = bar.tab_rect(0).unwrap();
        assert_eq!((r0.left, r0.right), (0, 72));
        assert_eq!(bar.hit_test(5, 10), Some(TabHit::Tab(0)));
        // 閉じるボタンは右端から pad_right の内側、縦は中央。
        assert_eq!(bar.hit_test(72 - 6 - 9, 16), Some(TabHit::Close(0)));
        assert_eq!(bar.hit_test(80, 16), Some(TabHit::Tab(1)));
        assert_eq!(bar.hit_test(300, 16), Some(TabHit::Empty));
        assert_eq!(bar.hit_test(300, 40), None);
        assert!(bar.set_hover(Some(TabHit::Tab(1))));
        assert!(!bar.set_hover(Some(TabHit::Tab(1))));
        // 一覧の数が変わると、マウスの状態は消える。
        bar.press_close(Some(1));
        assert!(bar.set_items(vec![item(1, "a")], Some(0)));
        assert_eq!(bar.pressed_close(), None);
        assert!(!bar.set_items(vec![item(1, "a")], Some(0)));
    }

    #[test]
    fn drag_starts_only_past_the_threshold() {
        let mut d = TabDrag::new(DocumentId::from_raw(1), (100, 10));
        assert!(!d.update((104, 12), 5));
        assert!(!d.update((95, 10), 5));
        assert!(d.update((106, 10), 5));
        // いったん始まれば、戻っても続く。
        assert!(d.update((100, 10), 5));
        let mut v = TabDrag::new(DocumentId::from_raw(1), (0, 0));
        assert!(v.update((0, -6), 5));
    }

    #[test]
    fn drag_end_is_detach_only_outside_the_window() {
        let w = RECT {
            left: 100,
            top: 50,
            right: 900,
            bottom: 650,
        };
        assert_eq!(drag_end((100, 50), &w), DragEnd::Stay);
        assert_eq!(drag_end((899, 649), &w), DragEnd::Stay);
        assert_eq!(drag_end((900, 300), &w), DragEnd::Detach);
        assert_eq!(drag_end((99, 300), &w), DragEnd::Detach);
        assert_eq!(drag_end((500, 49), &w), DragEnd::Detach);
        assert_eq!(drag_end((500, 650), &w), DragEnd::Detach);
        // 別のモニターにあたる負の座標。
        assert_eq!(drag_end((-1500, 200), &w), DragEnd::Detach);
    }

    #[test]
    fn reorder_target_uses_centers_of_the_other_tabs() {
        // 幅 100 のタブが 4 つ。タブ 0 をドラッグ。
        let pos: Vec<_> = (0..4).map(|i| Some((i * 100, 100))).collect();
        assert_eq!(reorder_target(&pos, 0, 10), 0);
        assert_eq!(reorder_target(&pos, 0, 149), 0);
        // 他のタブの中心（150、250、350）を超えた分だけ進む（戻るときは動かした後の並びの中心）。
        assert_eq!(reorder_target(&pos, 0, 151), 1);
        assert_eq!(reorder_target(&pos, 0, 251), 2);
        assert_eq!(reorder_target(&pos, 0, 351), 3);
        assert_eq!(reorder_target(&pos, 0, 9999), 3);
        assert_eq!(reorder_target(&pos, 0, -50), 0);
        // 末尾のタブを左へ。
        assert_eq!(reorder_target(&pos, 3, 0), 0);
        assert_eq!(reorder_target(&pos, 3, 120), 1);
        assert_eq!(reorder_target(&pos, 3, 49), 0);
        assert_eq!(reorder_target(&pos, 3, 51), 1);
        // 幅が違っても、動かした後の同じ位置で結果が変わらない（行き来しない）。
        let widths = [200, 80, 120];
        let layout_of = |order: &[usize]| {
            let mut x = 0;
            order
                .iter()
                .map(|&i| {
                    let p = Some((x, widths[i]));
                    x += widths[i];
                    p
                })
                .collect::<Vec<_>>()
        };
        let cursor = 230;
        let before = layout_of(&[0, 1, 2]);
        let to = reorder_target(&before, 0, cursor);
        let mut order = vec![0, 1, 2];
        move_item(&mut order, 0, to);
        let after = layout_of(&order);
        assert_eq!(reorder_target(&after, to, cursor), to);
    }

    #[test]
    fn reorder_target_with_overflow_uses_the_tab_under_the_cursor() {
        let pos = vec![None, Some((0, 75)), Some((75, 75)), None];
        assert_eq!(reorder_target(&pos, 1, 100), 2);
        assert_eq!(reorder_target(&pos, 2, 10), 1);
        assert_eq!(reorder_target(&pos, 1, -30), 1);
        assert_eq!(reorder_target(&pos, 1, 500), 2);
        assert_eq!(reorder_target(&[None, None], 1, 5), 1);
        assert_eq!(reorder_target(&[], 0, 5), 0);
        assert_eq!(reorder_target(&pos, 9, 5), 9);
    }

    #[test]
    fn moving_items_keeps_the_active_tab() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert!(move_item(&mut v, 0, 2));
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        assert!(!move_item(&mut v, 1, 1));
        assert!(!move_item(&mut v, 4, 0));
        assert!(!move_item(&mut v, 0, 4));
        // どの選択位置でも、同じ文書が選択されたまま。
        for from in 0..4 {
            for to in 0..4 {
                for active in 0..4 {
                    let mut v: Vec<usize> = (0..4).collect();
                    move_item(&mut v, from, to);
                    let a = active_after_move(Some(active), from, to).unwrap();
                    assert_eq!(v[a], active, "from {from} to {to} active {active}");
                }
            }
        }
        assert_eq!(active_after_move(None, 0, 1), None);
    }

    #[test]
    fn memo_tabs_reserve_room_for_the_icon() {
        let mut bar = TabBar::new();
        let mut memo = item(2, "bbbbbbbb");
        memo.memo = true;
        bar.set_items(vec![item(1, "bbbbbbbb"), memo], Some(0));
        bar.relayout(
            RECT {
                left: 0,
                top: 0,
                right: 2000,
                bottom: 30,
            },
            96,
            &|s| s.chars().count() as i32 * 10,
        );
        let plain = bar.tab_rect(0).unwrap();
        let with_icon = bar.tab_rect(1).unwrap();
        let m = TabMetrics::scaled(96);
        assert_eq!(
            (with_icon.right - with_icon.left) - (plain.right - plain.left),
            m.memo_extra()
        );
    }

    #[test]
    fn memo_icon_gives_way_to_the_title_in_narrow_tabs() {
        for dpi in [96, 144, 192] {
            let m = TabMetrics::scaled(dpi);
            // 最小幅のタブで題名に使える幅: しるしは出さない（題名を優先）。
            let at_min = m.min_width - (m.pad_left + m.gap + m.close + m.pad_right);
            assert!(!m.memo_icon_fits(at_min), "dpi {dpi}");
            // 自然な幅のタブなら出す。
            let natural = m.natural_width(100 + m.memo_extra());
            let avail = natural - (m.pad_left + m.gap + m.close + m.pad_right);
            assert!(m.memo_icon_fits(avail), "dpi {dpi}");
        }
    }

    #[test]
    fn long_memo_titles_stay_within_the_max_width() {
        let mut bar = TabBar::new();
        let mut memo = item(1, "x");
        memo.memo = true;
        bar.set_items(vec![memo], Some(0));
        bar.relayout(
            RECT {
                left: 0,
                top: 0,
                right: 2000,
                bottom: 30,
            },
            96,
            &|_| 5000,
        );
        let r = bar.tab_rect(0).unwrap();
        assert_eq!(r.right - r.left, TabMetrics::scaled(96).max_width);
    }

    #[test]
    fn tab_menu_enablement() {
        assert!(tab_menu_enabled("file.close", 0, 1, false));
        assert!(!tab_menu_enabled("file.close_others", 0, 1, true));
        assert!(tab_menu_enabled("file.close_others", 0, 2, true));
        assert!(!tab_menu_enabled("file.close_right", 2, 3, true));
        assert!(tab_menu_enabled("file.close_right", 1, 3, true));
        assert!(!tab_menu_enabled("file.move_to_new_window", 0, 1, true));
        assert!(tab_menu_enabled("file.move_to_new_window", 0, 2, false));
        assert!(!tab_menu_enabled("file.copy_path", 0, 2, false));
        assert!(tab_menu_enabled("file.reveal_in_explorer", 0, 2, true));
        assert!(tab_menu_enabled("file.save_as_memo", 0, 1, false));
        assert_eq!(TAB_MENU.iter().flatten().count(), 7);
    }
}
