#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The code came from an app whose lints allow this.
#![allow(clippy::type_complexity)]
//! Markdown: a CommonMark subset plus GitHub tables and strikethrough.
//!
//! Import: ATX and setext headings, paragraphs with soft/hard breaks, block quotes (→ Quote
//! style), fenced and indented code (→ one Code paragraph per line), thematic breaks (→ a
//! bottom-border paragraph), bullet and ordered lists (nested by indentation), GFM pipe tables,
//! and inlines: emphasis/strong (the CommonMark delimiter algorithm), `~~strike~~`, code spans,
//! inline links, autolinks, images (`data:` URIs become pictures; other destinations become
//! pictures when [`import_with`] is given a loader that can read them), backslash escapes,
//! entities, HTML comments (`<!-- ... -->`), `<br>` and `<a id>` anchors.
//!
//! Export writes the same constructs; pictures become `![alt](media-name)`, where a picture read
//! from a local path keeps that path as its media name.

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeExamples;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use craft_wordmodel::table::{MAX_COLS, MAX_ROWS};
use craft_wordmodel::{Align, Document, InlineObject};

use craft_flow::{self as model, Cell, FBlock, FTable, Fmt, Inline, Kind, ListInfo, Para, make_img};

/// Resolves a Markdown image destination that isn't a `data:` URI to picture bytes. `None`
/// leaves the destination as it is in the source (the reader then shows its alt text).
pub type ImageLoader<'a> = &'a dyn Fn(&str) -> Option<Arc<Vec<u8>>>;

/// The import context: the loader plus the paths of pictures it loaded, so a local image's
/// destination can be kept as its media key (see [`adopt_image_paths`]).
pub(crate) struct ImageCtx<'a> {
    pub loader: ImageLoader<'a>,
    pub sources: RefCell<Vec<(String, Arc<Vec<u8>>)>>,
}

impl<'a> ImageCtx<'a> {
    /// A context with no loader, for the plain [`parse`] / [`import`] entry points.
    pub fn none() -> Self {
        Self { loader: &|_| None, sources: RefCell::new(Vec::new()) }
    }
}

mod blocks;
mod escaping;
mod export;
pub mod extracted;
mod inlines;

pub use craft_flow::MAX_TABLE_CELLS;

/// The text of an entity body (`amp`, `#38`, `#x26`).
fn decode_entity(body: &str) -> Option<String> {
    craft_html::decode_entity(body).map(String::from)
}

pub use blocks::{Refs, import, import_on, import_on_with, import_with, parse};
pub use export::escape;
pub use export::export;
#[cfg(test)]
use export::{escape_line_start, inlines_md, table_md, write_blocks};
pub use inlines::parse_inlines;

/// Rename a document's media keys after import so a picture loaded from a local path keeps that
/// path as its key. `craft_flow::to_doc` names pictures `image1.<ext>` by content; matching the
/// loaded bytes back to their destination lets the Markdown writer put the relative path in the
/// `![alt](dest)` it emits, so a save needs no companion-file copy and no side table on the
/// document. Keys stay plain relative paths, so they cannot escape a folder on export.
fn adopt_image_paths(doc: &mut Document, ctx: ImageCtx<'_>) {
    let sources = ctx.sources.into_inner();
    if sources.is_empty() {
        return;
    }
    // One destination per distinct byte content. Content-deduplicated media means several
    // destinations can share a key; the first one wins, as with `add_media`.
    let mut by_bytes: HashMap<Arc<Vec<u8>>, String> = HashMap::new();
    for (dest, bytes) in sources {
        by_bytes.entry(bytes).or_insert(dest);
    }
    let old_keys: Vec<String> = doc.media.keys().cloned().collect();
    let mut renames: Vec<(String, String)> = Vec::new();
    for key in old_keys {
        let Some(bytes) = doc.media.get(&key) else { continue };
        if let Some(dest) = by_bytes.get(bytes) {
            renames.push((key, dest.clone()));
        }
    }
    if renames.is_empty() {
        return;
    }
    // Only accept a destination that is a plain relative path (no scheme, no absolute or
    // parent-traversal component, no control characters). Anything else keeps its generated key.
    let mut media = std::mem::take(&mut doc.media);
    let mut applied: Vec<(String, String)> = Vec::new();
    for (old, dest) in renames {
        if !safe_destination(&dest) || media.contains_key(&dest) {
            continue;
        }
        if let Some(bytes) = media.remove(&old) {
            media.insert(dest.clone(), bytes);
            applied.push((old, dest));
        }
    }
    doc.media = media;
    if applied.is_empty() {
        return;
    }
    // Update every reference to a renamed key, in the body and all parts (headers, footers…).
    let fix_object = |o: &mut InlineObject| {
        if let InlineObject::Image { media: key, .. } = o
            && let Some((_, dest)) = applied.iter().find(|(old, _)| old == key)
        {
            *key = dest.clone();
        }
    };
    for block in &mut doc.body {
        craft_wordmodel::walk::each_para_mut(std::sync::Arc::make_mut(block), 0, &mut |p| {
            for o in &mut p.objects {
                fix_object(o);
            }
        });
    }
    for part in doc.parts.values_mut() {
        for block in &mut part.blocks {
            craft_wordmodel::walk::each_para_mut(std::sync::Arc::make_mut(block), 0, &mut |p| {
                for o in &mut p.objects {
                    fix_object(o);
                }
            });
        }
    }
}

/// Whether `dest` is a plain relative path safe to use as a media key and to write back to a
/// Markdown file: relative, no scheme or network path, no `..` component, no control characters.
fn safe_destination(dest: &str) -> bool {
    if dest.is_empty() || dest.len() > model::MAX_LINK || dest.starts_with("//") || dest.contains(['\0', '\n', '\r', '\\']) {
        return false;
    }
    if dest.split_once(':').is_some_and(|(scheme, _)| !scheme.contains('/')) {
        return false;
    }
    let path = std::path::Path::new(dest);
    !path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)))
}

#[cfg(test)]
use blocks::split_row;
#[cfg(test)]
mod tests;
