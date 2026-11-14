//! フォルダーをまたぐ Grep。行単位で検索し、文字コード（UTF-8／UTF-16／CP932）は
//! [`crate::text`] の判定に従う。結果は構造化され、[`GrepHit::to_json`] でそのまま
//! 将来の MCP の値にできる。
//!
//! 除外: バイナリ、大きすぎるファイル、隠しフォルダー（`.` で始まる名前、Windows の
//! 隠し属性）、`.git` など。シンボリックリンクはたどらない。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::matcher::{SearchOptions, Searcher};
use crate::core::{Completion, Error, Outcome, Result, TaskContext};
use crate::text::{self, Encoding};

/// 既定の最大ヒット数。
pub const DEFAULT_MAX_HITS: u64 = 2000;
/// 既定の、読む 1 ファイルの最大サイズ。
pub const DEFAULT_MAX_FILE_BYTES: u64 = 16 << 20;
/// 結果に載せる行の抜粋の最大バイト数。
const MAX_EXCERPT_BYTES: usize = 240;
/// 判定でバイナリを見分けるために調べる先頭のバイト数。
const BINARY_SAMPLE: usize = 8192;

#[derive(Debug, Clone)]
pub struct GrepOptions {
    pub root: PathBuf,
    pub search: SearchOptions,
    /// 隠しフォルダー・隠しファイルも調べる。
    pub include_hidden: bool,
    pub max_file_bytes: u64,
    /// 調べない名前のフォルダー（大文字小文字を区別しない）。
    pub exclude_dirs: Vec<String>,
    /// 拡張子（ドットなし、小文字）。空ならすべて。
    pub extensions: Vec<String>,
}

impl GrepOptions {
    pub fn new(root: impl Into<PathBuf>, search: SearchOptions) -> Self {
        Self {
            root: root.into(),
            search,
            include_hidden: false,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            exclude_dirs: [".git", ".hg", ".svn", "node_modules", "target"]
                .map(String::from)
                .to_vec(),
            extensions: Vec::new(),
        }
    }
}

/// 1 件の一致行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    pub path: PathBuf,
    /// 1 始まり。
    pub line: u64,
    /// 行頭からの文字数 + 1（最初の一致）。
    pub column: u64,
    /// 行の抜粋（行末を含まない。長い行は最初の一致の付近だけ）。
    pub text: String,
    /// `text` 内の一致のバイト範囲。
    pub ranges: Vec<(usize, usize)>,
    /// そのファイルの文字コードの名前（[`Encoding::name`]）。
    pub encoding: &'static str,
}

impl GrepHit {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "line": self.line,
            "column": self.column,
            "text": self.text,
            "ranges": self.ranges.iter().map(|(s, e)| json!([s, e])).collect::<Vec<_>>(),
            "encoding": self.encoding,
        })
    }
}

/// 1 ファイル分の結果（逐次、UI へ渡す単位）。
#[derive(Debug, Clone)]
pub struct GrepFile {
    pub path: PathBuf,
    pub hits: Vec<GrepHit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GrepSummary {
    /// 内容を調べたファイル数。
    pub files_scanned: u64,
    /// バイナリ・大きすぎる・読めないなどで飛ばしたファイル数。
    pub files_skipped: u64,
    pub hits: u64,
}

impl GrepSummary {
    pub fn to_json(&self, completion: Completion) -> Value {
        json!({
            "files_scanned": self.files_scanned,
            "files_skipped": self.files_skipped,
            "hits": self.hits,
            "completion": completion.as_str(),
        })
    }
}

fn is_hidden(path: &Path, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if let Ok(m) = fs::symlink_metadata(path) {
            return m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0;
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    false
}

/// 1 ファイルを調べる。バイナリ・読めないときは `None`。
fn scan_file(
    path: &Path,
    searcher: &Searcher,
    max_hits: u64,
    ctx: &TaskContext,
) -> Option<Result<Vec<GrepHit>>> {
    let bytes = fs::read(path).ok()?;
    let det = text::detect(&bytes);
    let wide = matches!(det.encoding, Encoding::Utf16Le | Encoding::Utf16Be);
    if !wide && bytes[..bytes.len().min(BINARY_SAMPLE)].contains(&0) {
        return None;
    }
    let decoded = match text::decode(bytes, det.encoding, ctx) {
        Ok(d) => d,
        Err(e) => return Some(Err(e)),
    };
    let encoding = det.encoding.name();
    let body = decoded.text.as_str();
    let sep = if body.contains('\n') || !body.contains('\r') {
        '\n'
    } else {
        '\r'
    };
    let mut hits = Vec::new();
    for (i, line) in body.split(sep).enumerate() {
        if i % 4096 == 0 && ctx.cancel.is_cancelled() {
            return Some(Err(Error::Cancelled));
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(hit) = line_hit(path, searcher, line, i as u64 + 1, encoding) {
            hits.push(hit);
            if hits.len() as u64 >= max_hits {
                break;
            }
        }
    }
    Some(Ok(hits))
}

fn line_hit(
    path: &Path,
    searcher: &Searcher,
    line: &str,
    number: u64,
    encoding: &'static str,
) -> Option<GrepHit> {
    let matches: Vec<(usize, usize)> = searcher.line_matches(line);
    let (first, _) = *matches.first()?;
    let column = line[..first].chars().count() as u64 + 1;
    let mut offset = 0;
    if line.len() > MAX_EXCERPT_BYTES {
        offset = first.saturating_sub(60);
        while !line.is_char_boundary(offset) {
            offset -= 1;
        }
    }
    let mut end = (offset + MAX_EXCERPT_BYTES).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let ranges = matches
        .iter()
        .filter(|(s, e)| *s >= offset && *e <= end)
        .map(|(s, e)| (s - offset, e - offset))
        .collect();
    Some(GrepHit {
        path: path.to_path_buf(),
        line: number,
        column,
        text: line[offset..end].to_owned(),
        ranges,
        encoding,
    })
}

/// フォルダーを調べ、ファイルごとに `on_file`（一致のあったファイルだけ）を呼ぶ。
/// 無効なパターンと、起点のフォルダーが読めないときは `Err`。件数は
/// `ctx.limits.max_items`（ヒット数。未指定なら [`DEFAULT_MAX_HITS`]）。
/// 進捗の単位は調べたファイル数（全体は不明）。
pub fn grep(
    options: &GrepOptions,
    ctx: &TaskContext,
    on_file: &mut dyn FnMut(GrepFile),
) -> Result<Outcome<GrepSummary>> {
    let searcher = options.search.compile()?;
    let max_hits = ctx.limits.max_items.unwrap_or(DEFAULT_MAX_HITS);
    fs::read_dir(&options.root).map_err(|e| Error::io("read_dir", &options.root, e))?;
    let mut summary = GrepSummary::default();
    let mut stack = vec![options.root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            summary.files_skipped += 1;
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut dirs = Vec::new();
        for entry in entries {
            if ctx.cancel.is_cancelled() {
                return Ok(Outcome::new(summary, Completion::Cancelled));
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if !options.include_hidden && is_hidden(&path, &name) {
                continue;
            }
            if ft.is_dir() {
                if !options
                    .exclude_dirs
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(&name))
                {
                    dirs.push(path);
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            if !options.extensions.is_empty() {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !options.extensions.contains(&ext) {
                    continue;
                }
            }
            if entry
                .metadata()
                .map_or(true, |m| m.len() > options.max_file_bytes)
            {
                summary.files_skipped += 1;
                continue;
            }
            let remaining = max_hits - summary.hits;
            match scan_file(&path, &searcher, remaining, ctx) {
                None => summary.files_skipped += 1,
                Some(Err(e)) if e.is_cancelled() => {
                    return Ok(Outcome::new(summary, Completion::Cancelled));
                }
                Some(Err(_)) => summary.files_skipped += 1,
                Some(Ok(hits)) => {
                    summary.files_scanned += 1;
                    ctx.report(summary.files_scanned, None);
                    if !hits.is_empty() {
                        summary.hits += hits.len() as u64;
                        on_file(GrepFile { path, hits });
                    }
                    if summary.hits >= max_hits {
                        return Ok(Outcome::new(summary, Completion::ItemLimit));
                    }
                }
            }
        }
        // 名前順に深さ優先でたどる（stack は後入れ先出しなので逆順に積む）。
        stack.extend(dirs.into_iter().rev());
    }
    Ok(Outcome::complete(summary))
}

/// [`grep`] の結果を全部集める（テストと MCP 向け）。
pub fn grep_collect(
    options: &GrepOptions,
    ctx: &TaskContext,
) -> Result<Outcome<(GrepSummary, Vec<GrepFile>)>> {
    let mut files = Vec::new();
    let out = grep(options, ctx, &mut |f| files.push(f))?;
    Ok(Outcome::new((out.value, files), out.completion))
}
