//! 行区切りを探す・数えるためのバイト列の走査。
//!
//! 行の区切りは `\n`、`\r\n`、単独の `\r`（[`crate::document::buffer`] の取り決め）。
//! 区切りの**最後のバイト**の次が、次の行の先頭になる。`\r` が区切りの最後かどうかは
//! 次のバイトを見ないと決まらないので、走査する側は 1 バイト先（`after`）を渡す。

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

#[inline]
pub(crate) fn is_terminator(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// UTF-8 の継続バイト（`10xxxxxx`）か。
#[inline]
pub(crate) fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// 8 バイトのうち 0 のバイトの最上位ビットを立てる。最下位の立っているビットは正確
/// （それより上位には偽の検出がありうる）。
#[inline]
fn zero_mask(v: u64) -> u64 {
    v.wrapping_sub(LO) & !v & HI
}

#[inline]
fn terminator_mask(w: u64) -> u64 {
    zero_mask(w ^ (LO * 0x0A)) | zero_mask(w ^ (LO * 0x0D))
}

/// 最初の `\n` または `\r` の位置。
pub(crate) fn find_terminator(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 8 <= buf.len() {
        let w = u64::from_le_bytes(buf[i..i + 8].try_into().unwrap());
        let m = terminator_mask(w);
        if m != 0 {
            return Some(i + (m.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    buf[i..]
        .iter()
        .position(|&b| is_terminator(b))
        .map(|p| i + p)
}

/// 最後の `\n` または `\r` の位置。
pub(crate) fn rfind_terminator(buf: &[u8]) -> Option<usize> {
    let mut end = buf.len();
    while end >= 8 {
        let w = u64::from_le_bytes(buf[end - 8..end].try_into().unwrap());
        if terminator_mask(w) != 0 {
            break;
        }
        end -= 8;
    }
    buf[..end].iter().rposition(|&b| is_terminator(b))
}

/// `buf` の中で終わる行区切りの数。`after` は `buf` の直後のバイト（本文の末尾なら `None`）。
///
/// `buf` の末尾の `\r` に `\n` が続く場合、その区切りは `buf` の外で終わるので数えない。
pub(crate) fn count_breaks(buf: &[u8], after: Option<u8>) -> u64 {
    let lf = buf.iter().filter(|&&b| b == b'\n').count();
    let cr = buf.iter().filter(|&&b| b == b'\r').count();
    if cr == 0 {
        return lf as u64;
    }
    let mut pairs = buf
        .iter()
        .zip(&buf[1..])
        .filter(|&(&a, &b)| a == b'\r' && b == b'\n')
        .count();
    if buf.last() == Some(&b'\r') && after == Some(b'\n') {
        pairs += 1;
    }
    (lf + cr - pairs) as u64
}

/// `from` 以降で最初に終わる行区切りの、最後のバイトの位置。
pub(crate) fn find_break_end(buf: &[u8], from: usize, after: Option<u8>) -> Option<usize> {
    let mut i = from;
    loop {
        let t = i + find_terminator(&buf[i..])?;
        if buf[t] == b'\n' {
            return Some(t);
        }
        let next = if t + 1 < buf.len() {
            Some(buf[t + 1])
        } else {
            after
        };
        if next != Some(b'\n') {
            return Some(t);
        }
        if t + 1 >= buf.len() {
            return None;
        }
        i = t + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_break_ends(text: &[u8], after: Option<u8>) -> Vec<usize> {
        (0..text.len())
            .filter(|&i| {
                let next = text.get(i + 1).copied().or(after);
                text[i] == b'\n' || (text[i] == b'\r' && next != Some(b'\n'))
            })
            .collect()
    }

    #[test]
    fn terminators_are_found_from_both_ends_at_every_alignment() {
        for len in 0..40 {
            for at in 0..len {
                for t in *b"\n\r" {
                    let mut buf = vec![b'x'; len];
                    buf[at] = t;
                    assert_eq!(find_terminator(&buf), Some(at));
                    assert_eq!(rfind_terminator(&buf), Some(at));
                }
            }
            // 0x0B・0x0C・0x8A など紛らわしい値は区切りではない。
            let buf: Vec<u8> = [0x0B, 0x0C, 0x09, 0x0E, 0x8A, 0x8D, 0x00, 0xFF]
                .iter()
                .cycle()
                .take(len)
                .copied()
                .collect();
            assert_eq!(find_terminator(&buf), None);
            assert_eq!(rfind_terminator(&buf), None);
        }
        let buf = b"ab\ncd\r\nefghijklmnop\rq";
        assert_eq!(find_terminator(buf), Some(2));
        assert_eq!(rfind_terminator(buf), Some(19));
    }

    #[test]
    fn breaks_are_counted_like_the_naive_definition() {
        let samples: [&[u8]; 8] = [
            b"",
            b"abc",
            b"a\nb\r\nc\rd",
            b"\r\n\r\n\n\r\r\n",
            b"tail\r",
            b"\r",
            b"\n",
            b"x\r\ny\r",
        ];
        for text in samples {
            for after in [None, Some(b'\n'), Some(b'x'), Some(b'\r')] {
                let ends = naive_break_ends(text, after);
                assert_eq!(count_breaks(text, after), ends.len() as u64);
                let mut found = Vec::new();
                let mut from = 0;
                while from <= text.len() {
                    match find_break_end(text, from, after) {
                        Some(i) => {
                            found.push(i);
                            from = i + 1;
                        }
                        None => break,
                    }
                }
                assert_eq!(found, ends, "{text:?} {after:?}");
            }
        }
    }
}
