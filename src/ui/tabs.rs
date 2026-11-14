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
        }
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
}

/// マウスの位置にあるもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    /// タブのないところ。
    Empty,
}

/// 各タブの横の位置（`None` は、あふれて見えないタブ）。
///
/// 自然な幅の合計が `avail` に収まればそのまま並べる。収まらなければ等しい幅に縮め、
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
    let total: i32 = natural.iter().sum();
    if total <= avail {
        *scroll = 0;
        let mut x = 0;
        for (slot, &w) in out.iter_mut().zip(natural) {
            *slot = Some((x, w));
            x += w;
        }
        return out;
    }
    let fit = (avail / metrics.min_width).max(1) as usize;
    if fit >= n {
        *scroll = 0;
        let w = avail / n as i32;
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = Some((i as i32 * w, w));
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
    let w = avail / fit as i32;
    for k in 0..fit {
        out[*scroll + k] = Some((k as i32 * w, w));
    }
    out
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
            .map(|t| m.natural_width(measure(&t.title)))
            .collect();
        self.positions = layout(
            &natural,
            bar.right - bar.left,
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

    fn close_rect(&self, tab: &RECT) -> RECT {
        let m = TabMetrics::scaled(self.dpi);
        let cy = (tab.top + tab.bottom) / 2;
        let right = tab.right - m.pad_right;
        RECT {
            left: right - m.close,
            top: cy - m.close / 2,
            right,
            bottom: cy - m.close / 2 + m.close,
        }
    }

    /// クライアント座標の点にあるもの。バーの外なら `None`。
    pub fn hit_test(&self, x: i32, y: i32) -> Option<TabHit> {
        if !crate::ui::window::contains(&self.bar, x, y) {
            return None;
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
            let text_rect = RECT {
                left: r.left + m.pad_left,
                right: close.left - m.gap,
                ..r
            };
            let fg = if active && !item.read_only {
                ui.tab_active_fg
            } else {
                ui.tab_fg
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
            if show_close {
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
            } else if item.dirty {
                let cx = (close.left + close.right) / 2;
                let cy = (close.top + close.bottom) / 2;
                c.dot(cx, cy, m.dot, ui.dirty);
            }
        }
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
}
