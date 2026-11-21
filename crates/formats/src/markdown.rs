//! Markdown import and export via `craft-markdown`.

use std::sync::Arc;

use wordcraft_doc::props::{Border, BorderStyle, Borders, Rgb};
use wordcraft_doc::{Block, Document, SectionProps};

pub use craft_markdown::{ImageLoader, Refs, escape, parse, parse_inlines};

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

/// The no-op loader: only `data:` URIs become pictures, as before local image paths were read.
fn no_images() -> impl Fn(&str) -> Option<Arc<Vec<u8>>> {
    |_| None
}

pub fn import(text: &str) -> Document {
    import_with(text, &no_images())
}

/// [`import`] resolving non-`data:` image destinations through `images`. Each loaded picture
/// keeps its path as its media key, so [`export`] writes that path back.
pub fn import_with(text: &str, images: ImageLoader<'_>) -> Document {
    let mut doc = wordcraft_docx::from_shared_model(craft_markdown::import_with(text, images)).unwrap_or_default();
    style_code_blocks(&mut doc);
    doc
}

pub fn import_on(text: &str, page: &SectionProps) -> Document {
    import_on_with(text, &no_images(), page)
}

/// [`import_on`] resolving non-`data:` image destinations through `images`.
pub fn import_on_with(text: &str, images: ImageLoader<'_>, page: &SectionProps) -> Document {
    let mut doc = wordcraft_docx::from_shared_model(craft_markdown::import_on_with(text, &shared_section(page), images)).unwrap_or_default();
    style_code_blocks(&mut doc);
    doc
}

/// [`import_on`] returning the document and the 0-based starting line of each top-level block.
pub fn import_on_with_lines(text: &str, page: &SectionProps) -> (Document, Vec<usize>) {
    import_on_with_lines_and_images(text, &no_images(), page)
}

/// [`import_on_with_lines`] resolving non-`data:` image destinations through `images`.
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

pub fn export(doc: &Document) -> String {
    match wordcraft_docx::to_shared_model(doc) {
        Ok(shared) => craft_markdown::export(&shared),
        Err(_) => String::new(),
    }
}
