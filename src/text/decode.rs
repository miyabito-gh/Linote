//! ファイルのバイト列 → 編集用 UTF-8。
//!
//! **不正なバイトの扱い**: 不正な列は U+FFFD（�）に置き換え、個数と位置を
//! [`InvalidBytes`] で返す。黙って壊さない代わりに、呼び出し側（UI）は
//! [`Decoded::is_lossy`] が真のとき「元のバイト列には戻せない」ことを利用者に知らせ、
//! 上書き保存の前に確認する（別の文字コードで開き直すのが普通の解決）。
//!
//! **往復の保証**: [`Decoded::reencodes_exactly`] が真なら、編集せずに同じ形式で
//! 書き出すと元のバイト列と完全に一致する。偽になるのは、不正なバイトがあった場合と、
//! CP932 で重複して登録された文字（NEC 選定 IBM 拡張など。[`encode`](super::encode)
//! の説明を参照）を含む場合。後者は文字としては同じだが、保存するとバイト列が変わる。

use std::io::{self, Write};

use super::encode::{encode_to, OnUnmappable};
use super::{Encoding, Eol, TextFormat};
use crate::core::{Error, Result, TaskContext};

/// 不正なバイト列の記録。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InvalidBytes {
    /// 不正な列の数（U+FFFD に置き換えた数）。
    pub count: u64,
    /// 最初のいくつか（[`InvalidBytes::MAX_OFFSETS`] 個まで）の、ファイル先頭からの
    /// バイト位置（BOM を含む）。
    pub first_offsets: Vec<u64>,
}

impl InvalidBytes {
    pub const MAX_OFFSETS: usize = 16;

    fn record(&mut self, offset: usize) {
        self.count += 1;
        if self.first_offsets.len() < Self::MAX_OFFSETS {
            self.first_offsets.push(offset as u64);
        }
    }
}

/// デコードの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// 本文（UTF-8）。BOM は含まない。改行は元のまま。
    pub text: String,
    /// 先頭に、その文字コードの BOM があったか。
    pub bom: bool,
    pub invalid: InvalidBytes,
    /// 編集せずに同じ文字コード・BOM で書き出すと、元のバイト列と一致するか。
    pub reencodes_exactly: bool,
}

impl Decoded {
    /// 不正なバイトを置き換えたか（元のバイト列には戻せない）。
    pub fn is_lossy(&self) -> bool {
        self.invalid.count > 0
    }
}

/// 一度に処理する入力の量。この区切りごとに中止を確かめる。
const SEGMENT: usize = 1 << 20;

/// `bytes`（ファイルの全体）を `encoding` として読む。
///
/// 先頭がその文字コードの BOM なら取り除く。判定は [`detect`](super::detect)、
/// 利用者の指定で開き直すときは指定された文字コードをそのまま渡す。
/// 妥当な UTF-8 は写しを作らずにそのまま本文にする。
pub fn decode(bytes: Vec<u8>, encoding: Encoding, ctx: &TaskContext) -> Result<Decoded> {
    ctx.cancel.check()?;
    let bom = super::detect::has_bom(&bytes, encoding);
    let bom_len = if bom { encoding.bom().len() } else { 0 };
    let mut invalid = InvalidBytes::default();
    let mut reencodes_exactly = true;
    let text = match encoding {
        Encoding::Utf8 => decode_utf8(bytes, bom_len, &mut invalid),
        Encoding::Utf16Le => decode_utf16(&bytes, bom_len, u16::from_le_bytes, &mut invalid, ctx)?,
        Encoding::Utf16Be => decode_utf16(&bytes, bom_len, u16::from_be_bytes, &mut invalid, ctx)?,
        Encoding::Cp932 => {
            let text = decode_cp932(&bytes, &mut invalid, ctx)?;
            if invalid.count == 0 {
                reencodes_exactly = cp932_reencodes_to(&text, &bytes, ctx)?;
            }
            text
        }
    };
    Ok(Decoded {
        text,
        bom,
        reencodes_exactly: reencodes_exactly && invalid.count == 0,
        invalid,
    })
}

fn decode_utf8(mut bytes: Vec<u8>, bom_len: usize, invalid: &mut InvalidBytes) -> String {
    if bom_len > 0 {
        bytes.drain(..bom_len);
    }
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            let bytes = e.into_bytes();
            let mut text = String::with_capacity(bytes.len() + 64);
            let mut offset = bom_len;
            for chunk in bytes.utf8_chunks() {
                text.push_str(chunk.valid());
                offset += chunk.valid().len();
                if !chunk.invalid().is_empty() {
                    invalid.record(offset);
                    text.push('\u{FFFD}');
                    offset += chunk.invalid().len();
                }
            }
            text
        }
    }
}

fn decode_utf16(
    bytes: &[u8],
    bom_len: usize,
    read: fn([u8; 2]) -> u16,
    invalid: &mut InvalidBytes,
    ctx: &TaskContext,
) -> Result<String> {
    let unit_at = |i: usize| read([bytes[i], bytes[i + 1]]);
    let len = bytes.len();
    // 日本語は 2 バイト → 3 バイトになる。まず同じ大きさを確保し、足りなければ伸ばす。
    let mut text = String::with_capacity(len - bom_len);
    let mut i = bom_len;
    while i + 1 < len {
        ctx.cancel.check()?;
        let segment_end = (i + SEGMENT).min(len);
        while i + 1 < segment_end {
            let unit = unit_at(i);
            match unit {
                0xD800..=0xDBFF => {
                    let low = (i + 3 < len).then(|| unit_at(i + 2));
                    match low {
                        Some(low @ 0xDC00..=0xDFFF) => {
                            let cp = 0x10000
                                + ((u32::from(unit) - 0xD800) << 10)
                                + (u32::from(low) - 0xDC00);
                            // 範囲から必ず妥当なスカラー値になる。
                            text.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            i += 4;
                        }
                        _ => {
                            invalid.record(i);
                            text.push('\u{FFFD}');
                            i += 2;
                        }
                    }
                }
                0xDC00..=0xDFFF => {
                    invalid.record(i);
                    text.push('\u{FFFD}');
                    i += 2;
                }
                _ => {
                    // サロゲート以外の 16 ビット値は必ず妥当なスカラー値。
                    text.push(char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}'));
                    i += 2;
                }
            }
        }
    }
    if i < len {
        // 奇数長: 最後の 1 バイトが余る。
        invalid.record(i);
        text.push('\u{FFFD}');
    }
    Ok(text)
}

fn decode_cp932(bytes: &[u8], invalid: &mut InvalidBytes, ctx: &TaskContext) -> Result<String> {
    let mut decoder = encoding_rs::SHIFT_JIS.new_decoder_without_bom_handling();
    let mut text = String::with_capacity(bytes.len() + bytes.len() / 2 + 16);
    let mut read = 0;
    loop {
        ctx.cancel.check()?;
        let segment_end = (read + SEGMENT).min(bytes.len());
        let last = segment_end == bytes.len();
        loop {
            let input = &bytes[read..segment_end];
            let need = decoder
                .max_utf8_buffer_length_without_replacement(input.len())
                .ok_or_else(|| Error::Internal("CP932 の変換領域が大きすぎる".into()))?;
            // U+FFFD（3 バイト）を足す余地も含めて確保する。
            text.reserve(need + 3);
            let (result, n) = decoder.decode_to_string_without_replacement(input, &mut text, last);
            read += n;
            match result {
                encoding_rs::DecoderResult::InputEmpty => break,
                encoding_rs::DecoderResult::OutputFull => {}
                encoding_rs::DecoderResult::Malformed(bad_len, after) => {
                    // 不正な列は、読み終えた位置の `after` バイト手前で終わる。
                    let start = read
                        .saturating_sub(usize::from(after))
                        .saturating_sub(usize::from(bad_len));
                    invalid.record(start);
                    text.push('\u{FFFD}');
                }
            }
        }
        if last {
            return Ok(text);
        }
    }
}

/// `text` を CP932 で書き出すと `original` と一致するか。
fn cp932_reencodes_to(text: &str, original: &[u8], ctx: &TaskContext) -> Result<bool> {
    struct Compare<'a> {
        rest: &'a [u8],
    }
    impl Write for Compare<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.rest.strip_prefix(buf) {
                Some(rest) => {
                    self.rest = rest;
                    Ok(buf.len())
                }
                None => Err(io::Error::other("differs")),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut compare = Compare { rest: original };
    let format = TextFormat {
        encoding: Encoding::Cp932,
        bom: false,
        eol: Eol::Lf,
    };
    // 進捗は読み込み側が報告するので、ここでは中止だけ引き継ぐ。
    let quiet = TaskContext::new(
        ctx.cancel.clone(),
        std::sync::Arc::new(crate::core::NoProgress),
        crate::core::Limits::UNLIMITED,
    );
    match encode_to(text, format, OnUnmappable::Fail, &mut compare, &quiet) {
        Ok(_) => Ok(compare.rest.is_empty()),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        // 食い違い、または書き出せない文字（どちらも「一致しない」）。
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CancelToken, Limits, NoProgress};
    use std::sync::Arc;

    fn dec(bytes: &[u8], encoding: Encoding) -> Decoded {
        decode(bytes.to_vec(), encoding, &TaskContext::detached()).unwrap()
    }

    #[test]
    fn utf8_valid_with_and_without_bom() {
        let d = dec("aあ\r\n".as_bytes(), Encoding::Utf8);
        assert_eq!(d.text, "aあ\r\n");
        assert!(!d.bom && !d.is_lossy() && d.reencodes_exactly);
        let d = dec(b"\xEF\xBB\xBFa", Encoding::Utf8);
        assert_eq!(d.text, "a");
        assert!(d.bom && d.reencodes_exactly);
        let d = dec(b"", Encoding::Utf8);
        assert_eq!(d.text, "");
        assert!(!d.bom);
        // 本文の途中の U+FEFF は BOM ではない。
        let d = dec("a\u{FEFF}b".as_bytes(), Encoding::Utf8);
        assert_eq!(d.text, "a\u{FEFF}b");
    }

    #[test]
    fn utf8_invalid_bytes_are_replaced_and_reported() {
        let d = dec(b"\xEF\xBB\xBFab\xFFcd\xE3\x81", Encoding::Utf8);
        assert_eq!(d.text, "ab\u{FFFD}cd\u{FFFD}");
        assert!(d.is_lossy() && !d.reencodes_exactly);
        assert_eq!(d.invalid.count, 2);
        assert_eq!(d.invalid.first_offsets, [5, 8]);
    }

    #[test]
    fn utf16_both_orders() {
        let d = dec(
            &[0xFF, 0xFE, 0x61, 0x00, 0x42, 0x30, 0x3D, 0xD8, 0x00, 0xDE],
            Encoding::Utf16Le,
        );
        assert_eq!(d.text, "aあ😀");
        assert!(d.bom && !d.is_lossy() && d.reencodes_exactly);
        let d = dec(
            &[0x00, 0x61, 0x30, 0x42, 0xD8, 0x3D, 0xDE, 0x00],
            Encoding::Utf16Be,
        );
        assert_eq!(d.text, "aあ😀");
        assert!(!d.bom);
        let d = dec(&[0xFE, 0xFF], Encoding::Utf16Be);
        assert_eq!(d.text, "");
        assert!(d.bom);
    }

    #[test]
    fn utf16_invalid_sequences() {
        // 対にならない上位サロゲート、対にならない下位サロゲート、末尾の上位サロゲート、
        // 余った 1 バイト。
        let d = dec(
            &[
                0x3D, 0xD8, 0x61, 0x00, 0x00, 0xDE, 0x62, 0x00, 0x3D, 0xD8, 0x63,
            ],
            Encoding::Utf16Le,
        );
        assert_eq!(d.text, "\u{FFFD}a\u{FFFD}b\u{FFFD}\u{FFFD}");
        assert_eq!(d.invalid.count, 4);
        assert_eq!(d.invalid.first_offsets, [0, 4, 8, 10]);
        assert!(!d.reencodes_exactly);
        let d = dec(&[0x61], Encoding::Utf16Le);
        assert_eq!(d.text, "\u{FFFD}");
        assert_eq!(d.invalid.first_offsets, [0]);
    }

    #[test]
    fn cp932_valid_and_invalid() {
        let d = dec(&[0x61, 0x82, 0xA0, 0xB1, 0x87, 0x40, 0x0A], Encoding::Cp932);
        assert_eq!(d.text, "aあｱ①\n");
        assert!(!d.bom && !d.is_lossy() && d.reencodes_exactly);

        // 0xFD は使われないバイト。0x82 で終わるのは 2 バイト文字の片割れ。
        let d = dec(&[0x61, 0xFD, 0x62, 0x82], Encoding::Cp932);
        assert_eq!(d.text, "a\u{FFFD}b\u{FFFD}");
        assert_eq!(d.invalid.count, 2);
        assert_eq!(d.invalid.first_offsets, [1, 3]);
        assert!(!d.reencodes_exactly);
    }

    #[test]
    fn cp932_duplicates_are_flagged_as_not_byte_exact() {
        // NEC 選定 IBM 拡張の「髙」(0xEEE0) は IBM 拡張 (0xFBFC) と同じ文字。
        let d = dec(&[0xEE, 0xE0], Encoding::Cp932);
        assert_eq!(d.text, "髙");
        assert!(!d.is_lossy());
        assert!(!d.reencodes_exactly);
        let d = dec(&[0xFB, 0xFC], Encoding::Cp932);
        assert_eq!(d.text, "髙");
        assert!(d.reencodes_exactly);
        // 外字は往復する。
        let d = dec(&[0xF0, 0x40, 0xF9, 0xFC], Encoding::Cp932);
        assert_eq!(d.text, "\u{E000}\u{E757}");
        assert!(d.reencodes_exactly);
    }

    #[test]
    fn many_invalid_bytes_keep_only_the_first_offsets() {
        let d = dec(&[0xFF; 100], Encoding::Utf8);
        assert_eq!(d.invalid.count, 100);
        assert_eq!(d.invalid.first_offsets.len(), InvalidBytes::MAX_OFFSETS);
        assert_eq!(d.text.chars().count(), 100);
    }

    #[test]
    fn cancel_is_honoured() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let ctx = TaskContext::new(cancel, Arc::new(NoProgress), Limits::UNLIMITED);
        for e in Encoding::ALL {
            assert!(matches!(
                decode(b"abc".to_vec(), e, &ctx),
                Err(Error::Cancelled)
            ));
        }
    }

    #[test]
    fn cp932_across_segment_boundaries() {
        // 2 バイト文字が区切り（1 MiB）をまたぐ。不正なバイトも混ぜる。
        let mut bytes = vec![b'a'];
        let n = SEGMENT + 10;
        for _ in 0..n {
            bytes.extend_from_slice(&[0x82, 0xA0]);
        }
        bytes.push(0xFD);
        bytes.extend_from_slice(&[0x82, 0xA0]);
        let d = dec(&bytes, Encoding::Cp932);
        assert_eq!(d.invalid.count, 1);
        assert_eq!(d.invalid.first_offsets, [(1 + n * 2) as u64]);
        assert_eq!(d.text.chars().count(), 1 + n + 2);
        assert!(d.text.ends_with("あ\u{FFFD}あ"));

        let clean = &bytes[..1 + n * 2];
        let d = dec(clean, Encoding::Cp932);
        assert!(!d.is_lossy() && d.reencodes_exactly);
    }
}
