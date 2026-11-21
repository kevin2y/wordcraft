//! Markdown import and export via `craft-markdown`.

use std::collections::HashSet;
use std::sync::Arc;

use wordcraft_doc::para::InlineObject;
use wordcraft_doc::props::{Border, BorderStyle, Borders, Rgb};
use wordcraft_doc::{Block, Document, SectionProps};

pub use craft_markdown::{Refs, escape, parse, parse_inlines};

pub use crate::markdown_images::ImageLoader;

/// A fenced block's lines are imported as consecutive `Code` paragraphs. Give the style a light
/// shading and a thin box so the layout groups them into one visibly bordered code block. Code
/// isn't prose: mark it (and inline code spans) `noProof` so the live spelling/grammar squiggles
/// don't cover it.
fn style_code_blocks(doc: &mut Document) {
    let styles = Arc::make_mut(&mut doc.styles);
    for style in &mut styles.styles {
        if style.id == "Code" {
            style.para.shading = Some(Rgb(0xF6, 0xF8, 0xFA));
            style.para.borders =
                Some(Borders::box_(Border { style: BorderStyle::Single, width: 0.5, color: Some(Rgb(0xD0, 0xD7, 0xDE)), space: 6.0 }));
            style.chr.no_proof = Some(true);
        }
    }
    // Inline code spans carry the mono font directly rather than using the Code style.
    for block in &mut doc.body {
        let Block::Para(p) = Arc::make_mut(block) else { continue };
        for run in &mut p.runs {
            if run.props.font.as_deref() == Some("Courier New") {
                run.props.no_proof = Some(true);
            }
        }
    }
}

fn shared_section(page: &SectionProps) -> craft_wordmodel::SectionProps {
    let mut d = Document::new();
    d.last_section = page.clone();
    wordcraft_docx::to_shared_model(&d).map(|s| s.last_section).unwrap_or_default()
}

/// The no-op loader: no local paths resolve, so this is the previous behaviour (only `data:` URIs
/// become pictures).
fn no_images() -> impl Fn(&str) -> Option<Arc<Vec<u8>>> {
    |_| None
}

pub fn import(text: &str) -> Document {
    import_with(text, &no_images())
}

/// [`import`] that resolves local image paths through `images` before parsing.
pub fn import_with(text: &str, images: ImageLoader<'_>) -> Document {
    let source = crate::markdown_images::inline(text, images);
    let mut doc = wordcraft_docx::from_shared_model(craft_markdown::import(&source)).unwrap_or_default();
    style_code_blocks(&mut doc);
    doc
}

pub fn import_on(text: &str, page: &SectionProps) -> Document {
    import_on_with(text, &no_images(), page)
}

/// [`import_on`] that resolves local image paths through `images` before parsing.
pub fn import_on_with(text: &str, images: ImageLoader<'_>, page: &SectionProps) -> Document {
    let source = crate::markdown_images::inline(text, images);
    let mut doc = wordcraft_docx::from_shared_model(craft_markdown::import_on(&source, &shared_section(page))).unwrap_or_default();
    style_code_blocks(&mut doc);
    doc
}

/// [`import_on`] returning the document and the 0-based starting line of each top-level block.
pub fn import_on_with_lines(text: &str, page: &SectionProps) -> (Document, Vec<usize>) {
    import_on_with_lines_and_images(text, &no_images(), page)
}

/// [`import_on_with_lines`] that resolves local image paths through `images`.
pub fn import_on_with_lines_and_images(text: &str, images: ImageLoader<'_>, page: &SectionProps) -> (Document, Vec<usize>) {
    let doc = import_on_with(text, images, page);
    let src_lines: Vec<&str> = text.strip_prefix('\u{FEFF}').unwrap_or(text).lines().collect();
    let mut out = Vec::with_capacity(doc.body.len());
    let mut cursor = 0usize;
    for block in &doc.body {
        let needle = match block.as_ref() {
            Block::Para(p) => p.text.split_whitespace().next().unwrap_or(""),
            Block::Table(t) => t
                .rows
                .first()
                .and_then(|r| r.cells.first())
                .and_then(|c| c.blocks.first())
                .and_then(|b| match b.as_ref() {
                    Block::Para(p) => p.text.split_whitespace().next(),
                    _ => None,
                })
                .unwrap_or(""),
        };
        let mut found = cursor;
        if !needle.is_empty() {
            if let Some(rel) = src_lines[cursor..].iter().position(|l| l.contains(needle)) {
                found = cursor + rel;
                cursor = found + 1;
                if matches!(block.as_ref(), Block::Table(_)) {
                    while src_lines.get(cursor).is_some_and(|l| l.trim_start().starts_with('|')) {
                        cursor += 1;
                    }
                }
            }
        } else if cursor >= src_lines.len() {
            found = src_lines.len();
        } else if let Some(rel) = src_lines[cursor..].iter().position(|l| !l.trim().is_empty()) {
            found = cursor + rel;
            cursor = found + 1;
        }
        out.push(found.min(src_lines.len()));
    }
    (doc, out)
}

/// Markdown text and the picture files it points at, both in document order.
///
/// The shared writer names every picture `image.<ext>`, which cannot work for two pictures of
/// the same kind and does not write the picture file at all. `plan` gives each picture a stable
/// neighbour name (`image.png`, `image2.png`, …) and returns its bytes, so a caller saving to a
/// real path can write them beside the `.md`.
#[derive(Debug)]
pub struct ExportPlan {
    pub text: String,
    pub images: Vec<(String, Arc<Vec<u8>>)>,
}

pub fn export(doc: &Document) -> String {
    export_plan(doc).text
}

/// [`export`] together with the image files its destinations name.
pub fn export_plan(doc: &Document) -> ExportPlan {
    export_plan_named(doc, &|_, fallback| fallback.to_string())
}

/// As [`export_plan`], but `name_for` picks each picture's companion file name from its media key
/// and the generated fallback (`image.png`, `image2.png`, …). A caller saving to a path can name a
/// picture after the file it came from; the returned `images` use exactly the chosen names, so
/// writing each beside the `.md` makes the destinations resolve.
pub fn export_plan_named(doc: &Document, name_for: &dyn Fn(&str, &str) -> String) -> ExportPlan {
    let text = match wordcraft_docx::to_shared_model(doc) {
        Ok(shared) => craft_markdown::export(&shared),
        Err(_) => return ExportPlan { text: String::new(), images: Vec::new() },
    };
    let keys = used_media(doc);
    let mut names = Vec::with_capacity(keys.len());
    let mut images = Vec::with_capacity(keys.len());
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for key in keys {
        let Some(bytes) = doc.media.get(&key) else { continue };
        let ext = image_ext(bytes, &key);
        let n = counts.entry(ext.clone()).or_insert(0);
        *n = n.saturating_add(1);
        let fallback = if *n == 1 { format!("image.{ext}") } else { format!("image{n}.{ext}") };
        let name = name_for(&key, &fallback);
        names.push(name.clone());
        images.push((name, bytes.clone()));
    }
    ExportPlan { text: rewrite_image_destinations(&text, &names), images }
}

/// Media keys of pictures the Markdown writer actually emits, in the order it emits them.
fn used_media(doc: &Document) -> Vec<String> {
    fn object(o: &InlineObject, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        match o {
            InlineObject::Image { media, .. } => {
                if seen.insert(media.clone()) {
                    out.push(media.clone());
                }
            }
            InlineObject::Group { children, .. } => {
                for child in children {
                    object(&child.obj, seen, out);
                }
            }
            _ => {}
        }
    }
    fn block(b: &Block, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        match b {
            Block::Para(p) => {
                for o in &p.objects {
                    object(o, seen, out);
                }
            }
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for inner in &cell.blocks {
                            block(inner, seen, out);
                        }
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for b in &doc.body {
        block(b, &mut seen, &mut out);
    }
    out
}

/// A safe, lowercase file extension for a companion image name.
fn image_ext(bytes: &[u8], key: &str) -> String {
    let from_bytes = craft_flow::sniff_image(bytes);
    let ext = from_bytes.or_else(|| key.rsplit_once('.').map(|(_, ext)| ext)).unwrap_or("png");
    let ext: String = ext.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).take(8).collect();
    if ext.is_empty() { "png".to_string() } else { ext }
}

/// Percent-encode the characters that would end an unbracketed Markdown destination. The
/// Import side decodes these with `percent_decode`, so the original file name round-trips.
fn encode_destination(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'~' | b'+' | b',') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

/// Replace the writer's `![alt](image.ext)` destinations with the plan's neighbour names.
/// Only image destinations are touched; ordinary links and text are copied verbatim.
fn rewrite_image_destinations(text: &str, names: &[String]) -> String {
    if names.is_empty() {
        return text.to_string();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len().saturating_add(names.len().saturating_mul(4)));
    let mut pos = 0usize;
    let mut i = 0usize;
    let mut next = 0usize;
    while i.saturating_add(1) < b.len() && next < names.len() {
        if b.get(i) != Some(&b'!') || b.get(i + 1) != Some(&b'[') {
            i += 1;
            continue;
        }
        let mut k = i.saturating_add(2);
        let mut depth = 0usize;
        while k < b.len() {
            match b.get(k).copied().unwrap_or(0) {
                b'\\' => k = k.saturating_add(2).min(b.len()),
                b'[' => {
                    depth = depth.saturating_add(1);
                    k += 1;
                }
                b']' if depth > 0 => {
                    depth -= 1;
                    k += 1;
                }
                b']' if b.get(k + 1) == Some(&b'(') => {
                    let start = k.saturating_add(2);
                    let mut end = start;
                    while end < b.len() && b.get(end) != Some(&b')') {
                        end += 1;
                    }
                    // Figures and charts write `![alt]()` with no destination; they are not
                    // pictures from `media`, so leave them alone or the names would shift.
                    if end < b.len() && end > start {
                        out.push_str(text.get(pos..start).unwrap_or(""));
                        // The file on disk keeps its plain name, but a Markdown destination must
                        // escape spaces and parentheses or the reader stops at them. Our reader
                        // percent-decodes destinations, so this round-trips.
                        out.push_str(&encode_destination(&names[next]));
                        pos = end;
                        i = end;
                        next += 1;
                        k = b.len();
                    } else {
                        k += 1;
                    }
                }
                _ => k += 1,
            }
        }
        i += 1;
    }
    out.push_str(text.get(pos..).unwrap_or(""));
    out
}
