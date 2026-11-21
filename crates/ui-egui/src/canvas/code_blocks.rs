//! Copy buttons on Markdown code blocks, drawn over the page texture like comment balloons.
//!
//! A fenced block is imported as a run of consecutive `Code` paragraphs; the layout gives them
//! one shaded, bordered box. This module finds those runs, places a small button at the box's
//! top-right corner while the pointer is over the block, and copies the run's plain text on click.

use std::collections::BTreeMap;

use egui::{Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use wordcraft_doc::{Block, Document, Path, StoryRef};
use wordcraft_layout::{DocLayout, Placed};

use crate::WordApp;
use crate::theme::Tokens;

use super::pages::page_screen_scale;

/// A run of consecutive `Code` paragraphs: its block indices and the text to copy.
struct CodeBlock {
    first: usize,
    last: usize,
    text: String,
}

fn code_blocks(doc: &Document) -> Vec<CodeBlock> {
    let mut out = Vec::new();
    let mut run: Option<(usize, usize, Vec<String>)> = None;
    for (i, block) in doc.body.iter().enumerate() {
        let para = match block.as_ref() {
            Block::Para(p) if p.props.style.as_deref() == Some("Code") => p,
            _ => {
                if let Some((first, last, lines)) = run.take() {
                    out.push(CodeBlock { first, last, text: lines.join("\n") });
                }
                continue;
            }
        };
        match &mut run {
            Some((_, last, lines)) => {
                *last = i;
                lines.push(para.text.clone());
            }
            None => run = Some((i, i, vec![para.text.clone()])),
        }
    }
    if let Some((first, last, lines)) = run.take() {
        out.push(CodeBlock { first, last, text: lines.join("\n") });
    }
    out
}

/// Document-space rectangles of one code block, one entry per page it appears on.
fn block_rects(layout: &DocLayout, block: &CodeBlock) -> Vec<(usize, Rect)> {
    let mut pages: BTreeMap<usize, Rect> = BTreeMap::new();
    for i in block.first..=block.last {
        let Some(locs) = layout.index.get(&(StoryRef::Body, Path::top(i))) else { continue };
        for &(pi, ii) in locs {
            let Some(item) = layout.pages.get(pi).and_then(|p| p.items.get(ii)) else { continue };
            let Placed::Lines { para, l0, l1, x, y, turn, .. } = item else { continue };
            if turn.is_turned() {
                continue;
            }
            let right = para.lines.get(*l0..*l1).unwrap_or(&[]).iter().map(|l| l.right).fold(*x, f32::max);
            let (_, bottom) = item.vertical_span();
            // The shade is placed immediately before its lines. Use the box, not the longest
            // line, so the Copy button hugs the box's top-right corner.
            let shaded = ii
                .checked_sub(1)
                .and_then(|j| layout.pages.get(pi).and_then(|p| p.items.get(j)))
                .and_then(|it| match it {
                    Placed::Fill { rect, .. } => Some(*rect),
                    _ => None,
                })
                .filter(|f| f.contains(wordcraft_geom::Point::new(*x, *y)))
                .map(|f| Rect::from_min_max(pos2(f.x, f.y), pos2(f.right(), f.bottom())));
            let rect = shaded.unwrap_or_else(|| Rect::from_min_max(pos2(*x, *y), pos2(right, bottom)));
            pages.entry(pi).and_modify(|r| *r = r.union(rect)).or_insert(rect);
        }
    }
    pages.into_iter().collect()
}

/// Draw a Copy button on a code block while the pointer is over it; copy its text when clicked.
pub(super) fn code_block_buttons(app: &mut WordApp, ui: &mut Ui, painter: &Painter, rects: &[Rect], layout: &DocLayout, scale: f32, view: Rect) {
    app.canvas.code_copy_rects.clear();
    if app.session.pdf_view().is_some() || app.modal_open() {
        return;
    }
    let blocks = code_blocks(&app.session.doc);
    if blocks.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let mut clicked: Option<String> = None;
    for (n, block) in blocks.iter().enumerate() {
        for (pi, r) in block_rects(layout, block) {
            let Some(pr) = rects.get(pi) else { continue };
            let ps = layout.pages.get(pi).map_or(scale, |page| page_screen_scale(*pr, page, scale));
            let screen =
                Rect::from_min_max(pos2(pr.min.x + r.min.x * ps, pr.min.y + r.min.y * ps), pos2(pr.min.x + r.max.x * ps, pr.min.y + r.max.y * ps));
            if !screen.intersects(view) || !pointer.is_some_and(|p| screen.contains(p)) {
                continue;
            }
            let size = vec2(46.0, 18.0);
            let btn = Rect::from_min_size(pos2((screen.max.x - size.x - 2.0).max(screen.min.x + 2.0), screen.min.y + 2.0), size);
            let resp = ui.interact(btn, ui.scope_id().with(("code_copy", n, pi)), Sense::click());
            painter.rect_filled(btn, 3.0, if resp.hovered() { t.pressed } else { t.panel });
            painter.rect_stroke(btn, 3.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            painter.text(btn.center(), egui::Align2::CENTER_CENTER, crate::tl!("Copy"), egui::FontId::proportional(11.0), t.text);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(block.text.clone());
                app.status(crate::tl!("Code copied"));
            }
            app.canvas.code_copy_rects.push(btn);
        }
    }
    if let Some(text) = clicked {
        crate::system_clipboard::put(app, ui.ctx(), &text, "", "", "");
    }
}

/// Whether a screen point is on a code block's Copy button, so the canvas won't also take the press.
pub(super) fn over_code_copy(app: &WordApp, p: Pos2) -> bool {
    app.canvas.code_copy_rects.iter().any(|r| r.contains(p))
}
