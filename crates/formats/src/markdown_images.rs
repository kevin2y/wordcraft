//! Resolve local picture paths in Markdown source before parsing.
//!
//! The Markdown parser turns a destination into a picture only when it is a `data:` URI; any
//! other destination is left as its alt text. A Markdown file on disk, though, names its pictures
//! by a path relative to the file. This module rewrites those local paths into `data:` URIs so
//! the parser (an external, read-only dependency) can embed them, without changing the source
//! text the editor keeps for saving.
//!
//! Only inline `![alt](path "title")` destinations and link reference definitions
//! `[label]: path "title"` are rewritten. Remote URLs, `data:` URIs, absolute paths and anything
//! the caller's loader declines are left untouched. Scanning follows the same rules the parser
//! uses to recognise a destination (escapes, code spans, fenced and indented code, `<…>` and
//! balanced parentheses), so a path the parser would not treat as a destination is not rewritten.
//! Bracket pairs and backtick runs are indexed once, as the parser does, so hostile input stays
//! linear. Everything is byte-slice safe and bounded; it never panics.

use std::collections::HashMap;
use std::sync::Arc;

use craft_flow as model;

/// Supplies the bytes of the picture a Markdown destination names, or `None` when that
/// destination is not a local file that may be read.
///
/// The loader receives the destination exactly as the parser would use it (unescaped). Returning
/// bytes commits the destination to being embedded as a `data:` URI.
pub type ImageLoader<'a> = &'a dyn Fn(&str) -> Option<Arc<Vec<u8>>>;

/// `text` with every resolvable local image destination replaced by a `data:` URI.
pub fn inline(text: &str, images: ImageLoader<'_>) -> String {
    let code = CodeSpans::new(text);
    let mut known: HashMap<usize, Option<usize>> = HashMap::new();
    let mut edits: Vec<Edit> = Vec::new();
    // A single budget for all destination and label scans, as the parser uses: unclosed `(`
    // destinations would otherwise rescan the rest of the line from every `![` (quadratic).
    let mut budget = text.len().saturating_mul(8).saturating_add(8192);
    let mut scan = Scanner { text, images, code: &code, known: &mut known, edits: &mut edits, budget: &mut budget };
    scan.run();

    if edits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len().saturating_add(64));
    let mut pos = 0usize;
    for edit in edits {
        if edit.start < pos || edit.end > text.len() || edit.end <= edit.start {
            continue;
        }
        out.push_str(text.get(pos..edit.start).unwrap_or(""));
        out.push_str(&edit.uri);
        pos = edit.end;
    }
    out.push_str(text.get(pos..).unwrap_or(""));
    out
}

/// An absolute byte range in the source to replace with `uri`.
struct Edit {
    start: usize,
    end: usize,
    uri: String,
}

/// Backtick runs paired the way the parser pairs them: a run closes at the next run of exactly
/// the same length. `spans` is sorted by opening offset so membership is a binary search.
struct CodeSpans {
    /// Opening byte offset → byte just after the matching closing run.
    ends: HashMap<usize, usize>,
    /// Sorted (open, close) byte ranges, close being exclusive.
    spans: Vec<(usize, usize)>,
}

impl CodeSpans {
    fn new(text: &str) -> Self {
        let b = text.as_bytes();
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0usize;
        while i < b.len() {
            if b.get(i) == Some(&b'`') {
                let start = i;
                while b.get(i) == Some(&b'`') {
                    i += 1;
                }
                runs.push((start, i - start));
            } else {
                i += 1;
            }
        }
        let mut by_len: HashMap<usize, Vec<usize>> = HashMap::new();
        for &(pos, len) in &runs {
            by_len.entry(len).or_default().push(pos);
        }
        let mut ends = HashMap::new();
        for &(pos, len) in &runs {
            if let Some(positions) = by_len.get(&len) {
                // Every vector is pushed in increasing position order, so partition_point finds
                // the first run after `pos` in logarithmic time.
                let after = positions.partition_point(|&p| p <= pos);
                if let Some(&close) = positions.get(after) {
                    ends.insert(pos, close.saturating_add(len));
                }
            }
        }
        let mut spans: Vec<(usize, usize)> = ends.iter().map(|(&open, &close)| (open, close)).collect();
        spans.sort_unstable();
        Self { ends, spans }
    }

    fn end_of_run(&self, open: usize) -> Option<usize> {
        self.ends.get(&open).copied()
    }

    fn covers(&self, start: usize, end: usize) -> bool {
        let i = self.spans.partition_point(|&(open, _)| open <= start);
        i > 0 && self.spans[i - 1].1 >= end
    }
}

struct Scanner<'a, 'b> {
    text: &'a str,
    images: ImageLoader<'a>,
    code: &'b CodeSpans,
    /// Cached bracket matches, absolute byte offsets, filled as scans pass them (linear total).
    known: &'b mut HashMap<usize, Option<usize>>,
    edits: &'b mut Vec<Edit>,
    /// Bytes the destination scans may still read in total; once spent, images are plain text.
    budget: &'b mut usize,
}

impl Scanner<'_, '_> {
    fn run(&mut self) {
        let bytes = self.text.as_bytes();
        let mut i = 0usize;
        let mut fence: Option<(u8, usize)> = None;
        let mut starts_block = true;
        while i < bytes.len() {
            let line_end = bytes.get(i..).and_then(|r| r.iter().position(|b| *b == b'\n')).map_or(bytes.len(), |n| i + n);
            let line = self.text.get(i..line_end).unwrap_or("");
            let t = line.trim_start_matches([' ', '\t']);
            let indent = line.len() - t.len();
            let marker = (indent < 4).then(|| fence_marker(t)).flatten();

            if let Some((fc, fl)) = fence {
                if marker.is_some_and(|(c, n)| c == fc && n >= fl) && t.trim_start_matches(fc as char).trim().is_empty() {
                    fence = None;
                    starts_block = true;
                }
                // Lines inside a fence are never links or definitions.
            } else if let Some(m) = marker {
                fence = Some(m);
                starts_block = false;
            } else if indent >= 4 {
                // Indented code: neither a definition nor inline parsing applies.
                starts_block = false;
            } else if starts_block && let Some(span) = definition_at(line) {
                self.rewrite(i, line, span);
                // A definition line keeps `starts_block` true for the next line.
            } else {
                self.scan_line(i, line);
                starts_block = is_blank(line) || atx_heading(t) || thematic_break(t);
            }
            i = line_end.saturating_add(1).min(bytes.len());
        }
    }

    /// Scan one non-code line for `![alt](dest)`. `base` is the line's absolute byte offset.
    fn scan_line(&mut self, base: usize, line: &str) {
        let b = line.as_bytes();
        let mut i = 0usize;
        while i < b.len() && *self.budget > 0 {
            match b.get(i).copied().unwrap_or(0) {
                b'\\' => i = i.saturating_add(2).min(b.len()),
                b'`' => {
                    if let Some(end) = self.code.end_of_run(base.saturating_add(i)) {
                        i = end.saturating_sub(base).min(b.len());
                    } else {
                        let run = b.get(i..).map(|r| r.iter().take_while(|x| **x == b'`').count()).unwrap_or(1);
                        i = i.saturating_add(run).min(b.len());
                    }
                }
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    let matched = self.matching_bracket(base.saturating_add(i + 1));
                    if let Some(abs_close) = matched
                        && abs_close >= base
                        && abs_close < base.saturating_add(line.len())
                    {
                        let rel_close = abs_close - base;
                        if b.get(rel_close + 1) == Some(&b'(') {
                            let at = rel_close + 1;
                            let span = link_dest(line, at);
                            // Charge what the scan read, whether or not it produced a destination.
                            let spent = span.map_or(line.len().saturating_sub(at), |s| s.end.saturating_sub(at));
                            *self.budget = self.budget.saturating_sub(spent);
                            if let Some(span) = span {
                                self.rewrite(base, line, span);
                                i = span.end;
                            } else {
                                i += 1;
                            }
                        } else {
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
    }

    /// The `]` matching the `[` at absolute byte `open`, respecting escapes and code spans, with
    /// results cached so repeated failed scans stay linear (as the parser's `known` map does).
    fn matching_bracket(&mut self, open: usize) -> Option<usize> {
        if let Some(found) = self.known.get(&open) {
            return *found;
        }
        let b = self.text.as_bytes();
        let mut opens: Vec<usize> = Vec::new();
        let mut i = open;
        while i < b.len() {
            match b.get(i).copied().unwrap_or(0) {
                b'\\' => i = i.saturating_add(2).min(b.len()),
                b'`' => {
                    if let Some(end) = self.code.end_of_run(i) {
                        i = end.min(b.len());
                    } else {
                        let run = b.get(i..).map(|r| r.iter().take_while(|x| **x == b'`').count()).unwrap_or(1);
                        i = i.saturating_add(run).min(b.len());
                    }
                }
                b'[' => opens.push(i),
                b']' => {
                    if let Some(o) = opens.pop() {
                        self.known.insert(o, Some(i));
                        if opens.is_empty() {
                            self.known.insert(open, Some(i));
                            return Some(i);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        for o in opens {
            self.known.insert(o, None);
        }
        self.known.insert(open, None);
        None
    }

    /// Replace the destination in `line` (`span` is line-relative) with a `data:` URI when the
    /// loader supplies a picture we can sniff. `base` makes the range absolute for the edit.
    fn rewrite(&mut self, base: usize, line: &str, span: DestSpan) {
        if span.dest_end <= span.dest_start || span.dest_end > line.len() {
            return;
        }
        let abs_start = base.saturating_add(span.dest_start);
        let abs_end = base.saturating_add(span.dest_end);
        if self.code.covers(abs_start, abs_end) {
            return;
        }
        let Some(raw) = line.get(span.dest_start..span.dest_end) else { return };
        let dest = unescape(raw);
        let Some(uri) = resolve(&dest, self.images) else { return };
        self.edits.push(Edit { start: abs_start, end: abs_end, uri });
    }
}

/// A destination's byte span within the scanned text and the index just after its closing `)`.
#[derive(Clone, Copy)]
struct DestSpan {
    dest_start: usize,
    dest_end: usize,
    end: usize,
}

/// Parse `(dest "title")` at byte `at` (which holds `(`), following the parser's `link_dest`:
/// the destination is either `<…>` or runs to whitespace / an unbalanced `)`, an optional title
/// follows, and a final `)` must close. Returns the raw destination span and the end of the whole
/// construct. Cuts are only at ASCII delimiters, so slices stay on char boundaries.
fn link_dest(s: &str, at: usize) -> Option<DestSpan> {
    let b = s.as_bytes();
    if b.get(at) != Some(&b'(') {
        return None;
    }
    let mut i = at.saturating_add(1);
    i = i.saturating_add(b.get(i..).map(|r| r.len() - r.trim_ascii_start().len()).unwrap_or(0));
    let angled = b.get(i) == Some(&b'<');
    let (dest_start, dest_end) = if angled {
        let start = i + 1;
        let mut k = start;
        let end = loop {
            match b.get(k).copied()? {
                b'\\' => k = k.saturating_add(2),
                b'>' => break k,
                b'\n' => return None,
                _ => k += 1,
            }
        };
        (start, end)
    } else {
        let start = i;
        let mut depth = 0i32;
        let mut end = None;
        let mut k = i;
        while k < b.len() {
            match b.get(k).copied().unwrap_or(0) {
                b'\\' => {
                    k = k.saturating_add(2).min(b.len());
                    continue;
                }
                b'(' => depth += 1,
                b')' if depth == 0 => {
                    end = Some(k);
                    break;
                }
                b')' => depth -= 1,
                c if c.is_ascii_whitespace() => {
                    end = Some(k);
                    break;
                }
                _ => {}
            }
            k += 1;
        }
        (start, end?)
    };
    i = dest_end.saturating_add(usize::from(angled));
    i = i.saturating_add(b.get(i..).map(|r| r.len() - r.trim_ascii_start().len()).unwrap_or(0));
    if let Some(q) = b.get(i).copied().filter(|c| matches!(c, b'"' | b'\'' | b'(')) {
        let close = if q == b'(' { b')' } else { q };
        let tail = s.get(i + 1..)?;
        let end = tail.find(close as char)?;
        i = i.saturating_add(end).saturating_add(2);
        i = i.saturating_add(b.get(i..).map(|r| r.len() - r.trim_ascii_start().len()).unwrap_or(0));
    }
    if b.get(i) != Some(&b')') {
        return None;
    }
    Some(DestSpan { dest_start, dest_end, end: i + 1 })
}

/// A reference definition `[label]: dest "title"` on one line: returns the destination span,
/// line-relative. Rejects footnote definitions and malformed labels just as the parser does, so
/// only real definitions are rewritten.
fn definition_at(line: &str) -> Option<DestSpan> {
    let t = line.trim_start();
    let lead = line.len() - t.len();
    if lead > 3 || !t.starts_with('[') {
        return None;
    }
    let b = t.as_bytes();
    let mut i = 1usize;
    let mut close = None;
    while i < b.len() {
        match b.get(i).copied().unwrap_or(0) {
            b'\\' => i = i.saturating_add(1),
            b'[' => return None,
            b']' => {
                close = Some(i);
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let close = close?;
    let label = t.get(1..close)?;
    if label.is_empty() || label.starts_with('^') || b.get(close + 1) != Some(&b':') {
        return None;
    }
    let mut k = close.saturating_add(2);
    let rest = t.get(k..)?;
    k = k.saturating_add(rest.len() - rest.trim_start().len());
    let r = t.get(k..)?;
    if r.is_empty() {
        return None;
    }
    let angled = r.starts_with('<');
    let (dest_start, dest_end) = if angled {
        let inner = r.get(1..)?;
        let end = inner.find('>')?;
        (k + 1, k + 1 + end)
    } else {
        let end = r.find(char::is_whitespace).unwrap_or(r.len());
        (k, k + end)
    };
    let after = t.get(if angled { dest_end + 1 } else { dest_end }..)?;
    let title = after.trim();
    let titled =
        title.is_empty() || (title.len() >= 2 && matches!((title.chars().next()?, title.chars().last()?), ('"', '"') | ('\'', '\'') | ('(', ')')));
    if !titled {
        return None;
    }
    Some(DestSpan { dest_start: lead + dest_start, dest_end: lead + dest_end, end: line.len() })
}

/// The fence marker at the start of `t`: (character, run length), or `None` for <3 or an info
/// string containing a backtick. Matches the parser's `fence`.
fn fence_marker(t: &str) -> Option<(u8, usize)> {
    let c = t.as_bytes().first().copied()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.as_bytes().iter().take_while(|x| **x == c).count();
    if n < 3 {
        return None;
    }
    if c == b'`' && t.get(n..).is_some_and(|info| info.contains('`')) {
        return None;
    }
    Some((c, n))
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn atx_heading(t: &str) -> bool {
    let n = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&n) && t.get(n..).is_none_or(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

fn thematic_break(t: &str) -> bool {
    let mut ch = None;
    let mut n = 0;
    for c in t.chars() {
        match c {
            ' ' | '\t' => {}
            '*' | '-' | '_' => {
                if ch.is_some_and(|x| x != c) {
                    return false;
                }
                ch = Some(c);
                n += 1;
            }
            _ => return false,
        }
    }
    n >= 3
}

/// Remove backslash escapes before ASCII punctuation, like the parser's `unescape`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && chars.peek().is_some_and(char::is_ascii_punctuation)
            && let Some(n) = chars.next()
        {
            out.push(n);
        } else {
            out.push(c);
        }
    }
    out
}

/// Turn a destination into a `data:` URI when the loader supplies a picture we can sniff.
fn resolve(dest: &str, images: ImageLoader<'_>) -> Option<String> {
    if dest.is_empty() || dest.starts_with("data:") {
        return None;
    }
    let bytes = images(dest)?;
    let ext = model::sniff_image(&bytes)?;
    Some(format!("data:{};base64,{}", model::mime_of(ext), model::base64_encode(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Arc<Vec<u8>> {
        let mut rgba = Vec::with_capacity(8 * 4 * 4);
        for _y in 0..4 {
            for x in 0..8 {
                rgba.extend_from_slice(&[(x * 30) as u8, 80, 160, 255]);
            }
        }
        let img = craft_image_codecs::Image::from_u8(8, 4, craft_image_codecs::ChannelLayout::Rgba, rgba).unwrap();
        Arc::new(craft_image_codecs::encode(&img, craft_image_codecs::Format::Png, &craft_image_codecs::EncodeOptions::default()).unwrap())
    }

    /// Loader that serves `image1.png` and records the destinations it was asked for.
    fn loader<'a>(seen: &'a std::cell::RefCell<Vec<String>>) -> impl Fn(&str) -> Option<Arc<Vec<u8>>> + 'a {
        move |src| {
            seen.borrow_mut().push(src.to_string());
            (src == "image1.png").then(png)
        }
    }

    #[test]
    fn inline_rewrites_a_local_image_destination() {
        let seen = std::cell::RefCell::new(Vec::new());
        let text = inline("![图片1](image1.png)", &loader(&seen));
        assert!(text.starts_with("![图片1](data:image/png;base64,"), "{text}");
        assert!(text.ends_with(')'));
        assert!(!text.contains("image1.png"));
        assert_eq!(*seen.borrow(), ["image1.png"]);
    }

    #[test]
    fn inline_keeps_a_title_and_the_surrounding_text() {
        let seen = std::cell::RefCell::new(Vec::new());
        let text = inline("Before ![alt](image1.png \"A title\") after.", &loader(&seen));
        assert!(text.starts_with("Before ![alt](data:image/png;base64,"));
        assert!(text.contains("\"A title\") after."));
    }

    #[test]
    fn inline_leaves_urls_and_data_uris_alone() {
        let seen = std::cell::RefCell::new(Vec::new());
        let src = "![web](https://example.com/image1.png) ![raw](data:image/png;base64,AAAA)";
        let text = inline(src, &loader(&seen));
        assert_eq!(text, src);
        // The data URI is skipped before the loader is consulted; the URL is declined by it.
        assert_eq!(*seen.borrow(), ["https://example.com/image1.png"]);
    }

    #[test]
    fn inline_skips_code_spans_and_code_blocks() {
        let seen = std::cell::RefCell::new(Vec::new());
        let src = concat!("`![a](image1.png)`\n\n", "```\n![b](image1.png)\n```\n\n", "    ![c](image1.png)\n\n", "![d](image1.png)\n",);
        let text = inline(src, &loader(&seen));
        assert!(text.contains("`![a](image1.png)`"));
        assert!(text.contains("![b](image1.png)"));
        assert!(text.contains("![c](image1.png)"));
        assert!(text.contains("![d](data:image/png;base64,"));
        assert_eq!(*seen.borrow(), ["image1.png"]);
    }

    #[test]
    fn inline_resolves_a_reference_definition() {
        let seen = std::cell::RefCell::new(Vec::new());
        let src = "![图片1][img]\n\n[img]: image1.png \"Caption\"\n";
        let text = inline(src, &loader(&seen));
        assert!(text.contains("[img]: data:image/png;base64,"));
        assert!(text.contains("\"Caption\""));
        assert!(text.contains("![图片1][img]"));
        assert_eq!(*seen.borrow(), ["image1.png"]);
    }

    #[test]
    fn inline_handles_angle_destinations_and_escapes() {
        let seen = std::cell::RefCell::new(Vec::new());
        let text = inline("![a](<image1.png>)", &loader(&seen));
        assert!(text.contains("(<data:image/png;base64,"), "{text}");
        assert_eq!(*seen.borrow(), ["image1.png"]);
    }

    #[test]
    fn inline_never_calls_the_loader_for_a_declined_path() {
        let seen = std::cell::RefCell::new(Vec::new());
        let src = "![missing](none.png) ![ok](image1.png)";
        let text = inline(src, &loader(&seen));
        assert!(text.contains("![missing](none.png)"));
        assert!(text.contains("![ok](data:image/png;base64,"));
        assert_eq!(*seen.borrow(), ["none.png", "image1.png"]);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let none = |_: &str| None::<Arc<Vec<u8>>>;
        for src in [
            "![",
            "![a](",
            "![a](<",
            "![a](\\",
            "[[[[![a](x)",
            "[a]: <\n![x](y)",
            "````![a](x.png)````",
            "![a](x.png \"unterminated)",
            "\u{feff}![a](x.png)",
        ] {
            let _ = inline(src, &none);
        }
    }
}
