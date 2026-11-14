//! The read-only PDF page view's canvas layer: text selection, link hit-testing, the overlays
//! drawn on top of each page bitmap, and the helpers that save and restore a reading position.
//!
//! Every function here works in page points or `(page, character index)` pairs. Nothing writes
//! to `Session.sel` or the editable document, so a PDF can never enter the undo history.

use egui::{Painter, Rect, Ui, Vec2, pos2, vec2};
use wordcraft_engine::{LinkTarget, PdfRect, PdfView};
use wordcraft_layout::DocLayout;

use super::links::is_followable_link;
use super::{CanvasState, GAP, PdfSelection, page_at};
use crate::WordApp;
use crate::theme::Tokens;

/// The scroll offset that puts `page` at the top of the viewport, `top` document points into it.
/// `rects` are the pages in **content** coordinates (the same space as `scroll_offset` and
/// `CanvasState::page_content_rects`); PDF link destinations and restored reading positions share
/// this, so capturing a position with [`position_at_scroll`] and putting it back here are inverses.
pub fn scroll_to_position(rects: &[Rect], scale: f32, page: usize, top: f32) -> Option<Vec2> {
    let pr = rects.get(page)?;
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let top = if top.is_finite() { top.max(0.0) } else { 0.0 };
    let x = (pr.left() - GAP).max(0.0);
    let y = (pr.top() + top * scale - 40.0).max(0.0);
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some(vec2(x, y))
}

/// Inverse of [`scroll_to_position`]: the page and offset a scroll offset is showing. Read Mode
/// turns whole columns sideways, so there the horizontal offset picks the page.
///
/// This **must** measure against `page_content_rects` (content coordinates). `page_rects` are
/// screen coordinates; using them makes a saved position drift by the canvas origin and the
/// current scroll offset — the bug an earlier implementation ran into.
pub fn position_at_scroll(c: &CanvasState, read_mode: bool) -> Option<(usize, f32)> {
    let rects = &c.page_content_rects;
    if rects.is_empty() || !c.scale.is_finite() || c.scale <= 0.0 || !c.scroll_offset.is_finite() {
        return None;
    }
    let offset = c.scroll_offset;
    let last = rects.len().saturating_sub(1);
    let page = if read_mode {
        rects.iter().position(|r| r.right() > offset.x).unwrap_or(last)
    } else {
        rects.iter().position(|r| r.bottom() > offset.y + 80.0).unwrap_or(last)
    };
    let pr = rects.get(page)?;
    let top = if read_mode { offset.y - pr.top() } else { offset.y + 40.0 - pr.top() } / c.scale;
    if !top.is_finite() {
        return None;
    }
    Some((page, top.max(0.0)))
}

/// Draw a PDF's text selection and hovered link on top of its page bitmap. `page_rect` is the
/// page's screen rect and `scale` the screen points per PDF point the bitmap was placed at.
pub(super) fn pdf_paint_overlay(app: &WordApp, painter: &Painter, pdf: &PdfView, page: usize, page_rect: Rect, scale: f32, t: &Tokens) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    // PDF coordinates come from a file: a non-finite one must not become an infinite rectangle.
    let s = |v: f32| if v.is_finite() { v } else { 0.0 };
    let to_screen = |r: &PdfRect| {
        Rect::from_min_max(
            pos2(page_rect.min.x + s(r.x0) * scale, page_rect.min.y + s(r.y0) * scale),
            pos2(page_rect.min.x + s(r.x1) * scale, page_rect.min.y + s(r.y1) * scale),
        )
    };
    if let Some((hp, idx)) = app.canvas.pdf_hover_link
        && hp == page
        && let Some(link) = pdf.links(page).get(idx)
    {
        // A warm highlight says "this is clickable", distinct from the cool selection colour.
        painter.rect_filled(to_screen(&link.rect), 0.0, t.find_highlight);
    }
    let Some(sel) = app.canvas.pdf_selection else { return };
    let (a, b) = sel.ends();
    if page < a.0 || page > b.0 {
        return;
    }
    let Some(chars) = pdf.text(page) else { return };
    let lo = if page == a.0 { a.1.min(chars.len()) } else { 0 };
    let hi = if page == b.0 { b.1.saturating_add(1).min(chars.len()) } else { chars.len() };
    if lo >= hi {
        return;
    }
    // Merge neighbouring characters of one line into a single strip, so a whole sentence is a
    // handful of rectangles instead of hundreds.
    let mut strip: Option<(usize, Rect)> = None;
    for c in chars.iter().take(hi).skip(lo) {
        let r = to_screen(&c.rect);
        match &mut strip {
            Some((line, acc)) if *line == c.line => *acc = (*acc).union(r),
            _ => {
                if let Some((_, acc)) = strip.take() {
                    painter.rect_filled(acc, 0.0, t.selection);
                }
                strip = Some((c.line, r));
            }
        }
    }
    if let Some((_, acc)) = strip {
        painter.rect_filled(acc, 0.0, t.selection);
    }
}

/// Mouse behaviour in the read-only PDF view: clicking a link jumps to its page, dragging picks
/// characters out of the PDF text layer. Nothing here touches `Session.sel`, so the view never
/// becomes editable and no undo entry is created.
pub(super) fn pdf_mouse(app: &mut WordApp, ui: &Ui, resp: &egui::Response, rects: &[Rect], layout: &DocLayout, scale: f32) {
    let Some(pdf) = app.session.pdf_view() else { return };
    let Some(p) = resp.interact_pointer_pos().or_else(|| resp.hover_pos()) else { return };
    let Some((page, x, y)) = page_at(rects, layout, scale, p) else {
        app.canvas.pdf_hover_link = None;
        return;
    };
    let hover = pdf.links(page).iter().position(|l| l.rect.contains(x, y));
    app.canvas.pdf_hover_link = hover.map(|i| (page, i));
    if hover.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if resp.clicked() {
        match pdf.link_at(page, x, y) {
            Some(LinkTarget::Page { page: target, top }) => {
                app.canvas.pdf_goto = Some((target, top.unwrap_or(0.0)));
                app.canvas.pdf_selection = None;
                ui.ctx().request_repaint();
                return;
            }
            Some(LinkTarget::Uri(url)) if is_followable_link(&url) => {
                app.canvas.open_url = Some(url);
                return;
            }
            _ => app.canvas.pdf_selection = None,
        }
    }
    if resp.drag_started()
        && hover.is_none()
        && let Some(i) = pdf.char_index_at(page, x, y)
    {
        app.canvas.pdf_selection = Some(PdfSelection { anchor: (page, i), focus: (page, i), dragging: true });
    }
    if resp.dragged()
        && let Some(sel) = &mut app.canvas.pdf_selection
        && sel.dragging
        && let Some(i) = pdf.char_index_at(page, x, y)
    {
        sel.focus = (page, i);
    }
    if resp.drag_stopped()
        && let Some(sel) = &mut app.canvas.pdf_selection
    {
        sel.dragging = false;
    }
}

/// The plain text of the PDF selection, or `None` when no PDF selection exists. Ctrl+C and the
/// `ui.pdf.copy` command both go through here.
pub fn pdf_selected_text(app: &WordApp) -> Option<String> {
    let pdf = app.session.pdf_view()?;
    let sel = app.canvas.pdf_selection?;
    let (a, b) = sel.ends();
    Some(pdf.selection_text(a, b))
}
