//! 編集用 UTF-8 → ファイルのバイト列。変換できない文字の検出。
//!
//! - 改行は変換しない（本文のバイト列のまま書く）。
//! - UTF-8・UTF-16 はすべての文字を表せる。変換できない文字がありうるのは CP932 だけ。
//! - 出力は [`std::io::Write`] へ少しずつ書く（100 MiB 級でも全体の写しを作らない）。
//!
//! ## CP932 の扱い（`encoding_rs::SHIFT_JIS` との違い）
//!
//! `encoding_rs` の Shift_JIS は WHATWG の定義で、Windows の CP932 とほぼ同じ表
//! （NEC 特殊文字、NEC 選定 IBM 拡張、IBM 拡張を含む）を使う。違いと対処:
//!
//! - **書き出すと別の文字に化ける文字は「変換できない」として扱う。**
//!   WHATWG のエンコーダーは U+00A5（¥）を 0x5C、U+203E（‾）を 0x7E、U+2212（−）を
//!   0x817C に書くが、読み直すと U+005C（\）、U+007E（~）、U+FF0D（－）になる。
//!   黙って化けさせないため、この 3 文字は変換不能として報告する。
//! - **外字（U+E000〜U+E757）は書き出せるようにする。** WHATWG のデコーダーは
//!   0xF040〜0xF9FC を私用領域に読むが、エンコーダーは書けない。Windows の CP932 と
//!   同じ対応で自前で書き、読み込み → 書き出しでバイト列が変わらないようにする。
//! - **重複して登録された文字**（NEC 選定 IBM 拡張 0xED40〜0xEEFC と、0x81CA／0xEEF9／
//!   0xFA54 の ¬ など）は、どのバイト列から読んでも同じ文字になるので、書き出すと
//!   代表のバイト列（IBM 拡張 0xFA40〜 など）に寄る。文字は変わらないがバイト列は
//!   変わる。読み込み時に [`Decoded::reencodes_exactly`](super::Decoded) で分かる。

use std::io::{self, Write};

use super::{Encoding, TextFormat};
use crate::core::{Budget, Completion, Error, Outcome, Result, TaskContext};

/// 変換できない文字を置き換えるバイト（[`OnUnmappable::Replace`]）。
pub const REPLACEMENT_BYTE: u8 = b'?';

/// 変換できない文字があったときの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnUnmappable {
    /// [`Error::Encoding`] で失敗する（既定。何も失わない）。
    Fail,
    /// `?` に置き換えて続ける。利用者が確認した後にだけ使う。
    Replace,
}

/// 変換できない文字 1 個の位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Unmappable {
    pub ch: char,
    /// 本文（UTF-8）の先頭からのバイト位置。
    pub offset: u64,
    /// 行（0 始まり）。改行は CRLF・LF・CR のどれも 1 行と数える。
    pub line: u64,
    /// 行頭からの文字数（0 始まり。Unicode スカラー値の数）。
    pub column: u64,
}

/// 一度に処理する本文の量。この区切りごとに中止を確かめ、進捗を報告する。
const SEGMENT: usize = 1 << 20;
/// 出力の作業領域。
const OUT_BUF: usize = 64 * 1024;

/// CP932 の外字領域（0xF040〜0xF9FC ↔ U+E000〜U+E757）。
const EUDC_FIRST: u32 = 0xE000;
const EUDC_LAST: u32 = 0xE757;

/// `ch` を `encoding` で表せるか（書き出して読み直すと同じ文字に戻るか）。
pub fn can_encode(ch: char, encoding: Encoding) -> bool {
    match encoding {
        Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be => true,
        Encoding::Cp932 => {
            if ch.is_ascii() {
                return true;
            }
            if cp932_alters(ch) {
                return false;
            }
            if cp932_eudc(ch).is_some() {
                return true;
            }
            let mut src = [0u8; 4];
            let mut dst = [0u8; 8];
            let mut encoder = encoding_rs::SHIFT_JIS.new_encoder();
            let (result, _, _) = encoder.encode_from_utf8_without_replacement(
                ch.encode_utf8(&mut src),
                &mut dst,
                true,
            );
            matches!(result, encoding_rs::EncoderResult::InputEmpty)
        }
    }
}

/// 本文を `format` の文字コードで書いたときに変換できない文字を、先頭から順に返す。
///
/// 件数は `ctx.limits.max_items` で打ち切る（[`Completion::ItemLimit`]）。
/// UTF-8・UTF-16 では常に空。上書きの前に利用者へ知らせるために使う。
pub fn find_unmappable(
    text: &str,
    encoding: Encoding,
    ctx: &TaskContext,
) -> Result<Outcome<Vec<Unmappable>>> {
    let mut found = Vec::new();
    if encoding != Encoding::Cp932 {
        return Ok(Outcome::complete(found));
    }
    let mut budget = Budget::new(ctx.limits);
    let mut cursor = LineCursor::default();
    let mut stopped = None;
    let walked = cp932_walk(text, ctx, &mut |_| Ok(()), &mut |offset, ch| {
        if let Err(completion) = budget.try_add(ch.len_utf8() as u64) {
            stopped = Some(completion);
            return Err(Error::Cancelled); // 打ち切りの合図（下で読み替える）
        }
        cursor.advance(text, offset);
        found.push(Unmappable {
            ch,
            offset: offset as u64,
            line: cursor.line,
            column: cursor.column,
        });
        Ok(())
    });
    match (walked, stopped) {
        (_, Some(completion)) => Ok(Outcome::new(found, completion)),
        (Ok(()), None) => Ok(Outcome::new(found, Completion::Complete)),
        (Err(e), None) => Err(e),
    }
}

/// 本文を `format`（文字コード・BOM）で `out` に書く。書いたバイト数を返す。
///
/// 進捗は本文（UTF-8）のバイト数で報告する。`out` の失敗は
/// [`Error::Io`]（`path` なし）、中止は [`Error::Cancelled`]、変換できない文字
/// （[`OnUnmappable::Fail`] のとき）は [`Error::Encoding`]。失敗した場合、`out` には
/// 途中まで書かれている。
pub fn encode_to(
    text: &str,
    format: TextFormat,
    on_unmappable: OnUnmappable,
    out: &mut dyn Write,
    ctx: &TaskContext,
) -> Result<u64> {
    let mut written = 0u64;
    let mut sink = |bytes: &[u8]| -> Result<()> {
        out.write_all(bytes).map_err(write_error)?;
        written += bytes.len() as u64;
        Ok(())
    };
    if format.bom {
        sink(format.encoding.bom())?;
    }
    match format.encoding {
        Encoding::Utf8 => {
            let total = text.len() as u64;
            let mut done = 0;
            for chunk in text.as_bytes().chunks(SEGMENT) {
                ctx.cancel.check()?;
                sink(chunk)?;
                done += chunk.len() as u64;
                ctx.report(done, Some(total));
            }
        }
        Encoding::Utf16Le => utf16_walk(text, ctx, &mut sink, u16::to_le_bytes)?,
        Encoding::Utf16Be => utf16_walk(text, ctx, &mut sink, u16::to_be_bytes)?,
        Encoding::Cp932 => {
            // 変換結果と置換の両方が同じ出力へ書くので、RefCell で共有する。
            let sink = std::cell::RefCell::new(&mut sink);
            cp932_walk(
                text,
                ctx,
                &mut |bytes| (sink.borrow_mut())(bytes),
                &mut |offset, ch| match on_unmappable {
                    OnUnmappable::Fail => Err(unmappable_error(ch, offset)),
                    OnUnmappable::Replace => (sink.borrow_mut())(&[REPLACEMENT_BYTE]),
                },
            )?;
        }
    }
    Ok(written)
}

/// 本文を `format` でバイト列にする（小さな本文やテスト向け。大きな本文は
/// [`encode_to`] でファイルへ直接書く）。
pub fn encode(
    text: &str,
    format: TextFormat,
    on_unmappable: OnUnmappable,
    ctx: &TaskContext,
) -> Result<Vec<u8>> {
    let estimate = match format.encoding {
        Encoding::Utf8 | Encoding::Cp932 => text.len(),
        Encoding::Utf16Le | Encoding::Utf16Be => text.len() * 2,
    };
    let mut out = Vec::with_capacity(estimate + 4);
    encode_to(text, format, on_unmappable, &mut out, ctx)?;
    Ok(out)
}

fn write_error(source: io::Error) -> Error {
    Error::Io {
        op: "書き込み",
        path: None,
        source,
    }
}

fn unmappable_error(ch: char, offset: usize) -> Error {
    Error::Encoding {
        reason: format!(
            "{} に変換できない文字があります: U+{:04X}（バイト位置 {offset}）",
            Encoding::Cp932.label(),
            ch as u32
        ),
    }
}

fn utf16_walk(
    text: &str,
    ctx: &TaskContext,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    to_bytes: fn(u16) -> [u8; 2],
) -> Result<()> {
    let total = text.len() as u64;
    let mut buf = Vec::with_capacity(OUT_BUF + 4);
    let mut pos = 0;
    while pos < text.len() {
        ctx.cancel.check()?;
        let end = text.floor_char_boundary((pos + SEGMENT).min(text.len()));
        for unit in text[pos..end].encode_utf16() {
            buf.extend_from_slice(&to_bytes(unit));
            if buf.len() >= OUT_BUF {
                sink(&buf)?;
                buf.clear();
            }
        }
        pos = end;
        ctx.report(pos as u64, Some(total));
    }
    sink(&buf)
}

/// 書き出すと別の文字に化ける文字（モジュールの説明を参照）。
fn cp932_alters(ch: char) -> bool {
    matches!(ch, '\u{00A5}' | '\u{203E}' | '\u{2212}')
}

/// 外字（私用領域）の CP932 のバイト列。
fn cp932_eudc(ch: char) -> Option<[u8; 2]> {
    let cp = ch as u32;
    if !(EUDC_FIRST..=EUDC_LAST).contains(&cp) {
        return None;
    }
    // WHATWG の Shift_JIS デコーダーの逆算（pointer 8836〜10715）。
    let pointer = cp - EUDC_FIRST + 8836;
    let lead = pointer / 188;
    let trail = pointer % 188;
    let lead_byte = lead + if lead < 0x1F { 0x81 } else { 0xC1 };
    let trail_byte = trail + if trail < 0x3F { 0x40 } else { 0x41 };
    Some([lead_byte as u8, trail_byte as u8])
}

/// `segment` の中で最初に現れる「化ける文字」の位置と文字。
fn find_altering(segment: &str) -> Option<(usize, char)> {
    let bytes = segment.as_bytes();
    let mut from = 0;
    // 3 文字の UTF-8 表現は C2 A5、E2 80 BE、E2 88 92。先頭バイトで絞る。
    while let Some(i) = bytes[from..].iter().position(|&b| b == 0xC2 || b == 0xE2) {
        let at = from + i;
        let rest = &bytes[at..];
        let hit = if rest.starts_with(&[0xC2, 0xA5]) {
            Some('\u{00A5}')
        } else if rest.starts_with(&[0xE2, 0x80, 0xBE]) {
            Some('\u{203E}')
        } else if rest.starts_with(&[0xE2, 0x88, 0x92]) {
            Some('\u{2212}')
        } else {
            None
        };
        if let Some(ch) = hit {
            return Some((at, ch));
        }
        from = at + 1;
    }
    None
}

/// 本文を CP932 に変換しながら歩く。変換できたバイト列を `sink` に渡し、変換できない
/// 文字ごとに `unmappable(本文中のバイト位置, 文字)` を呼ぶ（`Err` を返すと止まる）。
fn cp932_walk(
    text: &str,
    ctx: &TaskContext,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    unmappable: &mut dyn FnMut(usize, char) -> Result<()>,
) -> Result<()> {
    let total = text.len() as u64;
    let mut encoder = encoding_rs::SHIFT_JIS.new_encoder();
    let mut buf = vec![0u8; OUT_BUF];
    let mut pos = 0;
    while pos < text.len() {
        ctx.cancel.check()?;
        let segment_end = text.floor_char_boundary((pos + SEGMENT).min(text.len()));
        let altering = find_altering(&text[pos..segment_end]);
        let plain_end = altering.map_or(segment_end, |(i, _)| pos + i);

        let mut p = pos;
        while p < plain_end {
            let (result, read, written) =
                encoder.encode_from_utf8_without_replacement(&text[p..plain_end], &mut buf, false);
            sink(&buf[..written])?;
            p += read;
            match result {
                encoding_rs::EncoderResult::InputEmpty => break,
                encoding_rs::EncoderResult::OutputFull => {}
                encoding_rs::EncoderResult::Unmappable(ch) => match cp932_eudc(ch) {
                    Some(bytes) => sink(&bytes)?,
                    // `read` は変換できない文字まで含む。
                    None => unmappable(p - ch.len_utf8(), ch)?,
                },
            }
        }
        debug_assert_eq!(p, plain_end);

        pos = match altering {
            Some((_, ch)) => {
                unmappable(plain_end, ch)?;
                plain_end + ch.len_utf8()
            }
            None => segment_end,
        };
        ctx.report(pos as u64, Some(total));
    }
    Ok(())
}

/// バイト位置から行・列を求めるための、前へだけ進むカーソル。
#[derive(Debug, Default)]
struct LineCursor {
    offset: usize,
    line: u64,
    column: u64,
}

impl LineCursor {
    fn advance(&mut self, text: &str, to: usize) {
        let bytes = text.as_bytes();
        for i in self.offset..to {
            match bytes[i] {
                b'\n' => {
                    self.line += 1;
                    self.column = 0;
                }
                b'\r' => {
                    // CRLF は LF の側で数える。
                    if bytes.get(i + 1) != Some(&b'\n') {
                        self.line += 1;
                        self.column = 0;
                    }
                }
                // UTF-8 の継続バイトは文字数に数えない。
                b if b & 0xC0 != 0x80 => self.column += 1,
                _ => {}
            }
        }
        self.offset = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CancelToken, Limits, NoProgress};
    use crate::text::Eol;
    use std::sync::Arc;

    fn fmt(encoding: Encoding, bom: bool) -> TextFormat {
        TextFormat {
            encoding,
            bom,
            eol: Eol::Lf,
        }
    }

    fn enc(text: &str, encoding: Encoding, bom: bool) -> Vec<u8> {
        encode(
            text,
            fmt(encoding, bom),
            OnUnmappable::Fail,
            &TaskContext::detached(),
        )
        .unwrap()
    }

    #[test]
    fn utf8_and_bom() {
        assert_eq!(enc("aあ\r\n", Encoding::Utf8, false), "aあ\r\n".as_bytes());
        assert_eq!(
            enc("aあ", Encoding::Utf8, true),
            b"\xEF\xBB\xBFa\xE3\x81\x82"
        );
        assert_eq!(enc("", Encoding::Utf8, true), b"\xEF\xBB\xBF");
        assert_eq!(enc("", Encoding::Utf8, false), b"");
    }

    #[test]
    fn utf16_byte_order_and_surrogates() {
        assert_eq!(
            enc("aあ😀", Encoding::Utf16Le, true),
            [0xFF, 0xFE, 0x61, 0x00, 0x42, 0x30, 0x3D, 0xD8, 0x00, 0xDE]
        );
        assert_eq!(
            enc("aあ😀", Encoding::Utf16Be, true),
            [0xFE, 0xFF, 0x00, 0x61, 0x30, 0x42, 0xD8, 0x3D, 0xDE, 0x00]
        );
        assert_eq!(enc("a", Encoding::Utf16Be, false), [0x00, 0x61]);
    }

    #[test]
    fn cp932_basic_and_vendor_chars() {
        assert_eq!(
            enc("aあｱ①㈱\n", Encoding::Cp932, false),
            [0x61, 0x82, 0xA0, 0xB1, 0x87, 0x40, 0x87, 0x8A, 0x0A]
        );
        // CP932 に BOM はない（bom を立てても何も書かない）。
        assert_eq!(enc("a", Encoding::Cp932, true), b"a");
    }

    #[test]
    fn cp932_unmappable_fails_or_replaces() {
        let text = "絵😀字𠮷¥x";
        let err = encode(
            text,
            fmt(Encoding::Cp932, false),
            OnUnmappable::Fail,
            &TaskContext::detached(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "encoding");
        let bytes = encode(
            text,
            fmt(Encoding::Cp932, false),
            OnUnmappable::Replace,
            &TaskContext::detached(),
        )
        .unwrap();
        assert_eq!(bytes, [0x8A, 0x47, b'?', 0x8E, 0x9A, b'?', b'?', b'x']);
        // 連続する変換不能文字と、末尾の変換不能文字。
        let bytes = encode(
            "😀😀¥‾−",
            fmt(Encoding::Cp932, false),
            OnUnmappable::Replace,
            &TaskContext::detached(),
        )
        .unwrap();
        assert_eq!(bytes, b"?????");
    }

    #[test]
    fn find_unmappable_reports_positions() {
        let text = "一行目\r\n絵文字 😀 と 𠮷野家\rab¥\n−";
        let ctx = TaskContext::detached();
        let found = find_unmappable(text, Encoding::Cp932, &ctx).unwrap();
        assert!(found.completion.is_complete());
        let got: Vec<(char, u64, u64)> = found
            .value
            .iter()
            .map(|u| (u.ch, u.line, u.column))
            .collect();
        assert_eq!(got, [('😀', 1, 4), ('𠮷', 1, 8), ('¥', 2, 2), ('−', 3, 0)]);
        for u in &found.value {
            assert!(text[u.offset as usize..].starts_with(u.ch));
        }
        for e in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
            assert!(find_unmappable(text, e, &ctx).unwrap().value.is_empty());
        }
        assert!(find_unmappable("普通の日本語 abc", Encoding::Cp932, &ctx)
            .unwrap()
            .value
            .is_empty());
    }

    #[test]
    fn find_unmappable_respects_the_item_limit() {
        let text = "😀".repeat(10);
        let ctx = TaskContext::detached().with_limits(Limits::items(3));
        let found = find_unmappable(&text, Encoding::Cp932, &ctx).unwrap();
        assert_eq!(found.value.len(), 3);
        assert_eq!(found.completion, Completion::ItemLimit);
    }

    #[test]
    fn cancel_stops_encoding() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let ctx = TaskContext::new(cancel, Arc::new(NoProgress), Limits::UNLIMITED);
        for e in Encoding::ALL {
            let r = encode("abc", fmt(e, false), OnUnmappable::Fail, &ctx);
            assert!(matches!(r, Err(Error::Cancelled)), "{e:?}");
        }
        assert!(matches!(
            find_unmappable("abc", Encoding::Cp932, &ctx),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn writer_errors_are_reported() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk on fire"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for e in Encoding::ALL {
            let r = encode_to(
                "abc",
                fmt(e, false),
                OnUnmappable::Fail,
                &mut Failing,
                &TaskContext::detached(),
            );
            assert_eq!(r.unwrap_err().code(), "io", "{e:?}");
        }
    }

    /// 書き出せると判断した文字は、読み直すと必ず同じ文字に戻る（黙って化けない）。
    #[test]
    fn cp932_encodable_chars_survive_a_round_trip() {
        let ctx = TaskContext::detached();
        let mut encodable = 0u32;
        for cp in 0..=0x10FFFFu32 {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            if !can_encode(ch, Encoding::Cp932) {
                continue;
            }
            encodable += 1;
            let mut s = [0u8; 4];
            let s: &str = ch.encode_utf8(&mut s);
            let bytes = encode(s, fmt(Encoding::Cp932, false), OnUnmappable::Fail, &ctx).unwrap();
            let back = crate::text::decode(bytes.clone(), Encoding::Cp932, &ctx).unwrap();
            assert_eq!(back.text, s, "U+{cp:04X} → {bytes:02X?}");
            assert_eq!(back.invalid.count, 0);
        }
        // ASCII 128 + U+0080 + 半角カナ 63 + JIS X 0208・拡張 + 外字 1880。
        assert!(encodable > 9000, "{encodable}");
        for ch in ['\u{00A5}', '\u{203E}', '\u{2212}', '😀', '𠮷', '\u{FFFD}'] {
            assert!(!can_encode(ch, Encoding::Cp932), "{ch:?}");
        }
        for ch in [
            '①', 'Ⅱ', '㈱', 'ｶ', '髙', '\u{E000}', '\u{E757}', '～', '－',
        ] {
            assert!(can_encode(ch, Encoding::Cp932), "{ch:?}");
        }
        assert!(!can_encode('\u{E758}', Encoding::Cp932));
    }

    #[test]
    fn eudc_bytes() {
        assert_eq!(cp932_eudc('\u{E000}'), Some([0xF0, 0x40]));
        assert_eq!(cp932_eudc('\u{E757}'), Some([0xF9, 0xFC]));
        assert_eq!(cp932_eudc('\u{E03E}'), Some([0xF0, 0x7E]));
        assert_eq!(cp932_eudc('\u{E03F}'), Some([0xF0, 0x80]));
    }

    #[test]
    fn large_text_crosses_segment_and_buffer_boundaries() {
        // 区切り（1 MiB）と作業領域（64 KiB）をまたぐ多バイト文字・変換不能文字。
        let unit = "あ😀b¥";
        let text = unit.repeat((SEGMENT * 2) / unit.len() + 7);
        let ctx = TaskContext::detached();
        let le = enc(&text, Encoding::Utf16Le, false);
        let expect: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(le, expect);
        let sjis = encode(
            &text,
            fmt(Encoding::Cp932, false),
            OnUnmappable::Replace,
            &ctx,
        )
        .unwrap();
        let n = text.len() / unit.len();
        assert_eq!(sjis, [0x82, 0xA0, b'?', b'b', b'?'].repeat(n));
        let found = find_unmappable(&text, Encoding::Cp932, &ctx).unwrap();
        assert_eq!(found.value.len(), n * 2);
    }
}
