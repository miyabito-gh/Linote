//! 印刷の純粋な計算（GUI 非依存）: ページ区切りからページ範囲への対応、ヘッダーとフッターの
//! 文字列、用紙の余白から本文の領域の算出。
//!
//! Win32 と Scintilla を使う実際の印刷は `ui::print` にある。ここは `windows` crate に
//! 依存しないので、Linux ネイティブでもテストできる。

/// 1 つの印刷で出すページ数の上限（暴走の歯止め）。
pub const MAX_PAGES: usize = 100_000;

/// 矩形（デバイスの単位。右・下は含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// 余白（0.01 mm 単位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Margins {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Default for Margins {
    /// 既定は 20 mm（上下左右）。
    fn default() -> Self {
        Margins {
            left: 2000,
            top: 2000,
            right: 2000,
            bottom: 2000,
        }
    }
}

/// 余白の上限（0.01 mm。100 mm）。
pub const MARGIN_MAX: i32 = 10_000;

impl Margins {
    pub fn clamped(self) -> Self {
        let c = |v: i32| v.clamp(0, MARGIN_MAX);
        Margins {
            left: c(self.left),
            top: c(self.top),
            right: c(self.right),
            bottom: c(self.bottom),
        }
    }
}

/// プリンターの紙の寸法（デバイスの単位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageMetrics {
    /// 用紙全体の大きさ。
    pub paper_w: i32,
    pub paper_h: i32,
    /// 印刷できる領域（DC の座標系の大きさ）。
    pub printable_w: i32,
    pub printable_h: i32,
    /// 用紙の左上から印刷できる領域の左上までの距離。
    pub offset_x: i32,
    pub offset_y: i32,
    pub dpi_x: i32,
    pub dpi_y: i32,
}

/// 0.01 mm をデバイスの単位（`dpi` は 1 インチあたりの数）にする。
pub fn hundredths_mm_to_device(value: i32, dpi: i32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 1270) / 2540) as i32
}

/// 1 ページの配置（DC の座標系）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLayout {
    /// ページ全体（印刷できる領域）。
    pub page: Rect,
    /// 本文を描く領域。
    pub body: Rect,
    pub header: Option<Rect>,
    pub footer: Option<Rect>,
}

/// 余白、ヘッダー・フッターの有無から配置を求める。本文が小さすぎれば `None`。
/// `line_height` はヘッダー・フッターの 1 行の高さ。
pub fn page_layout(
    metrics: PageMetrics,
    margins: Margins,
    header: bool,
    footer: bool,
    line_height: i32,
) -> Option<PageLayout> {
    let m = margins.clamped();
    let page = Rect {
        left: 0,
        top: 0,
        right: metrics.printable_w,
        bottom: metrics.printable_h,
    };
    // 用紙の端からの余白を、印刷できる領域の座標へ直す（領域の外には出さない）。
    let left = (hundredths_mm_to_device(m.left, metrics.dpi_x) - metrics.offset_x).max(0);
    let top = (hundredths_mm_to_device(m.top, metrics.dpi_y) - metrics.offset_y).max(0);
    let right =
        (metrics.paper_w - hundredths_mm_to_device(m.right, metrics.dpi_x) - metrics.offset_x)
            .min(metrics.printable_w);
    let bottom =
        (metrics.paper_h - hundredths_mm_to_device(m.bottom, metrics.dpi_y) - metrics.offset_y)
            .min(metrics.printable_h);
    let band = line_height + line_height / 2;
    let header_rect = header.then_some(Rect {
        left,
        top,
        right,
        bottom: top + line_height,
    });
    let footer_rect = footer.then_some(Rect {
        left,
        top: bottom - line_height,
        right,
        bottom,
    });
    let body = Rect {
        left,
        top: top + if header { band } else { 0 },
        right,
        bottom: bottom - if footer { band } else { 0 },
    };
    (body.width() >= line_height * 2 && body.height() >= line_height * 3).then_some(PageLayout {
        page,
        body,
        header: header_rect,
        footer: footer_rect,
    })
}

/// 印刷する範囲の指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSelection {
    /// すべてのページ。
    All,
    /// ページ番号の範囲（1 始まり、両端を含む）。
    Pages { from: u32, to: u32 },
}

/// 印刷する 1 ページ分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageJob {
    /// ページ番号（1 始まり。選択範囲だけを印刷する場合は選択範囲の中での番号）。
    pub number: u32,
    /// 本文の範囲（バイト位置、`start..end`）。
    pub start: u64,
    pub end: u64,
}

/// ページの区切りを求める。`format(start)` は `start` から 1 ページ分を組んだときの次の
/// ページの先頭位置を返す（Scintilla の `SCI_FORMATRANGEFULL` を描画なしで呼ぶ）。
/// 返すのは各ページの先頭位置。空の範囲でも 1 ページ（空白）を返す。
pub fn paginate(start: u64, end: u64, mut format: impl FnMut(u64) -> u64) -> Vec<u64> {
    let mut starts = Vec::new();
    let mut pos = start;
    while pos < end && starts.len() < MAX_PAGES {
        starts.push(pos);
        let next = format(pos);
        if next <= pos {
            // 進まない（1 行が 1 ページに入らないなど）。無限ループを避けて打ち切る。
            break;
        }
        pos = next;
    }
    if starts.is_empty() {
        starts.push(start);
    }
    starts
}

/// ページの先頭位置の列と範囲の終端から、印刷するページを選ぶ。
pub fn page_jobs(starts: &[u64], end: u64, selection: PageSelection) -> Vec<PageJob> {
    let total = starts.len() as u32;
    let (from, to) = match selection {
        PageSelection::All => (1, total),
        PageSelection::Pages { from, to } => (from.max(1), to.min(total)),
    };
    (from..=to)
        .map(|n| PageJob {
            number: n,
            start: starts[(n - 1) as usize],
            end: starts.get(n as usize).copied().unwrap_or(end),
        })
        .collect()
}

/// 日時（ヘッダーに出す。地域の書式ではなく固定の書式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

impl Timestamp {
    /// `2026/11/14 09:05`。
    pub fn format(&self) -> String {
        format!(
            "{:04}/{:02}/{:02} {:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }
}

/// ヘッダーの左（ファイル名またはタブ名）と右（印刷日時）。
pub fn header_texts(title: &str, now: Timestamp) -> (String, String) {
    (title.to_owned(), now.format())
}

/// フッター（ページ番号「n / m」）。
pub fn footer_text(page: u32, total: u32) -> String {
    format!("{page} / {total}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4_300DPI: PageMetrics = PageMetrics {
        paper_w: 2480,
        paper_h: 3508,
        printable_w: 2480,
        printable_h: 3508,
        offset_x: 0,
        offset_y: 0,
        dpi_x: 300,
        dpi_y: 300,
    };

    #[test]
    fn hundredths_mm_convert_to_device_units() {
        assert_eq!(hundredths_mm_to_device(2540, 300), 300);
        assert_eq!(hundredths_mm_to_device(2000, 300), 236);
        assert_eq!(hundredths_mm_to_device(0, 600), 0);
    }

    #[test]
    fn paginate_collects_page_starts() {
        // 1 ページに 10 バイト入る。
        let starts = paginate(0, 35, |s| (s + 10).min(35));
        assert_eq!(starts, vec![0, 10, 20, 30]);
        // 範囲の途中から。
        assert_eq!(paginate(5, 25, |s| (s + 10).min(25)), vec![5, 15]);
    }

    #[test]
    fn empty_range_is_one_blank_page() {
        assert_eq!(paginate(0, 0, |_| panic!("呼ばれない")), vec![0]);
    }

    #[test]
    fn paginate_stops_when_formatting_makes_no_progress() {
        assert_eq!(paginate(0, 100, |s| s), vec![0]);
    }

    #[test]
    fn page_jobs_cover_all_or_a_range() {
        let starts = [0, 10, 20, 30];
        let all = page_jobs(&starts, 35, PageSelection::All);
        assert_eq!(all.len(), 4);
        assert_eq!(
            all[0],
            PageJob {
                number: 1,
                start: 0,
                end: 10
            }
        );
        assert_eq!(
            all[3],
            PageJob {
                number: 4,
                start: 30,
                end: 35
            }
        );
        let mid = page_jobs(&starts, 35, PageSelection::Pages { from: 2, to: 3 });
        assert_eq!(
            mid,
            vec![
                PageJob {
                    number: 2,
                    start: 10,
                    end: 20
                },
                PageJob {
                    number: 3,
                    start: 20,
                    end: 30
                }
            ]
        );
    }

    #[test]
    fn page_range_is_clamped_and_may_be_empty() {
        let starts = [0, 10];
        let r = page_jobs(&starts, 15, PageSelection::Pages { from: 0, to: 99 });
        assert_eq!(r.iter().map(|j| j.number).collect::<Vec<_>>(), vec![1, 2]);
        assert!(page_jobs(&starts, 15, PageSelection::Pages { from: 3, to: 9 }).is_empty());
        assert!(page_jobs(&starts, 15, PageSelection::Pages { from: 2, to: 1 }).is_empty());
    }

    #[test]
    fn header_and_footer_strings() {
        let now = Timestamp {
            year: 2026,
            month: 11,
            day: 4,
            hour: 9,
            minute: 5,
        };
        assert_eq!(
            header_texts("メモ.txt", now),
            ("メモ.txt".into(), "2026/11/04 09:05".into())
        );
        assert_eq!(footer_text(3, 12), "3 / 12");
    }

    #[test]
    fn layout_reserves_header_and_footer_bands() {
        let plain = page_layout(A4_300DPI, Margins::default(), false, false, 40).unwrap();
        assert_eq!(plain.body.left, 236);
        assert_eq!(plain.body.top, 236);
        assert_eq!(plain.body.right, 2480 - 236);
        assert_eq!(plain.body.bottom, 3508 - 236);
        assert!(plain.header.is_none() && plain.footer.is_none());

        let full = page_layout(A4_300DPI, Margins::default(), true, true, 40).unwrap();
        assert_eq!(full.header.unwrap().top, 236);
        assert_eq!(full.body.top, 236 + 60);
        assert_eq!(full.body.bottom, 3508 - 236 - 60);
        assert_eq!(full.footer.unwrap().bottom, 3508 - 236);
    }

    #[test]
    fn layout_accounts_for_unprintable_offsets() {
        let m = PageMetrics {
            offset_x: 50,
            offset_y: 60,
            printable_w: 2380,
            printable_h: 3388,
            ..A4_300DPI
        };
        let l = page_layout(m, Margins::default(), false, false, 40).unwrap();
        assert_eq!(l.body.left, 236 - 50);
        assert_eq!(l.body.top, 236 - 60);
        assert_eq!(l.body.right, 2380 - (236 - 50));
        assert_eq!(l.body.bottom, 3388 - (236 - 60));
    }

    #[test]
    fn layout_rejects_margins_that_leave_no_body() {
        let huge = Margins {
            left: MARGIN_MAX,
            top: MARGIN_MAX,
            right: MARGIN_MAX,
            bottom: MARGIN_MAX,
        };
        let tiny = PageMetrics {
            printable_w: 600,
            printable_h: 600,
            ..A4_300DPI
        };
        assert!(page_layout(tiny, huge, true, true, 40).is_none());
    }

    #[test]
    fn margins_are_clamped() {
        let m = Margins {
            left: -5,
            top: 99_999,
            right: 0,
            bottom: 10,
        }
        .clamped();
        assert_eq!((m.left, m.top, m.right, m.bottom), (0, MARGIN_MAX, 0, 10));
    }
}
