//! Reading-position restore, PDF commands and the scroll round trip.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{Services, Session, UiState, WordApp};
use serde_json::json;

fn app() -> WordApp {
    WordApp::new(Session::new(wordcraft_doc::Document::new()), Services::default())
}

/// A one-page PDF with a /ToUnicode CMap, so its text layer has real characters. Inlined so
/// the test needs no fixture file and copying an empty string can never look like success.
const PDF_WITH_TEXT: &str = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA1OTUgODQyXSAvUmVzb3VyY2VzIDw8IC9Gb250IDw8IC9GMSA1IDAgUiA+PiA+PiAvQ29udGVudHMgNCAwIFIgPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCAzNyA+PgpzdHJlYW0KQlQgL0YxIDEyIFRmIDIwIDcwMCBUZCAoSGVsbG8pIFRqIEVUCgplbmRzdHJlYW0KZW5kb2JqCjUgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvSGVsdmV0aWNhIC9Ub1VuaWNvZGUgNiAwIFIgPj4KZW5kb2JqCjYgMCBvYmoKPDwgL0xlbmd0aCAxOTkgPj4Kc3RyZWFtCi9DSURJbml0IC9Qcm9jU2V0IGZpbmRyZXNvdXJjZSBiZWdpbgoxMiBkaWN0IGJlZ2luCmJlZ2luY21hcAoxIGJlZ2luY29kZXNwYWNlcmFuZ2UKPDAwPiA8RkY+CmVuZGNvZGVzcGFjZXJhbmdlCjQgYmVnaW5iZmNoYXIKPDQ4PiA8MDA0OD4KPDY1PiA8MDA2NT4KPDZDPiA8MDA2Qz4KPDZGPiA8MDA2Rj4KZW5kYmZjaGFyCmVuZGNtYXAKZW5kCmVuZAoKZW5kc3RyZWFtCmVuZG9iagp4cmVmCjAgNwowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMDkgMDAwMDAgbiAKMDAwMDAwMDA1OCAwMDAwMDAgbiAKMDAwMDAwMDAxMTUgMDAwMDAgbiAKMDAwMDAwMDI0MSAwMDAwMDAgbiAKMDAwMDAwMDMyOCAwMDAwMDAgbiAKMDAwMDAwMDQxNSAwMDAwMDAgbiAKdHJhaWxlcgo8PCAvU2l6ZSA3IC9Sb290IDEgMCBSID4+CnN0YXJ0eHJlZgo2NjUKJSVFT0YK";

/// Opening a Word document again returns to the caret the reader left it at, even after a
/// whole restart (the positions travel with `ui.json`, like the other preferences).
#[test]
fn a_documents_caret_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("wc-view-position-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("notes.txt");
    std::fs::write(&path, "first paragraph\nsecond paragraph\nthird paragraph").unwrap();
    let name = path.to_string_lossy().to_string();

    let mut first = app();
    first.run("file.open", json!({"path": name})).unwrap();
    first.run("caret.docEnd", json!({})).unwrap();
    let caret = first.session.sel.focus.clone();
    assert!(caret.off > 0, "the caret moved into the text: {caret:?}");
    // No frame has painted this document headlessly, so the page stays at the beginning;
    // the caret is remembered all the same.
    first.capture_view_position();
    let saved = serde_json::to_string(&first.prefs()).unwrap();
    assert!(saved.contains(r#""positions""#), "{saved}");

    let mut second = app();
    second.apply_prefs(UiState::from_json(saved.as_bytes()).unwrap());
    second.run("file.open", json!({"path": name})).unwrap();
    assert_eq!(second.session.sel.focus, caret, "the caret is where it was left");
    assert_eq!(second.canvas.restore_view, Some((0, 0.0)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PDF has no caret: its page and the distance into that page are what come back.
#[test]
fn a_pdfs_page_survives_a_restart() {
    let mut first = app();
    first.run("file.open", json!({"path": "book.pdf", "data": PDF_WITH_TEXT})).unwrap();
    // A frame's page rects in content coordinates; the reader is 120 pt into page 3 (of a
    // longer book than this one-page fixture, which the canvas clamps on restoration).
    first.canvas.page_rects_document = Some(first.session.document_id());
    first.canvas.page_content_rects =
        (0..3).map(|i| egui::Rect::from_min_size(egui::pos2(18.0, 18.0 + i as f32 * 900.0), egui::vec2(600.0, 842.0))).collect();
    first.canvas.scale = 1.0;
    first.canvas.scroll_offset = egui::vec2(0.0, first.canvas.page_content_rects[2].min.y + 120.0 - 40.0);
    first.capture_view_position();
    assert_eq!(first.ui.positions.get("book.pdf").map(|p| (p.page, p.top)), Some((2, 120.0)));
    let saved = serde_json::to_string(&first.prefs()).unwrap();

    let mut second = app();
    second.apply_prefs(UiState::from_json(saved.as_bytes()).unwrap());
    second.run("file.open", json!({"path": "book.pdf", "data": PDF_WITH_TEXT})).unwrap();
    assert_eq!(second.canvas.restore_view, Some((2, 120.0)), "the page and its offset come back");
    assert!(second.session.pdf.is_some());
    // The blank placeholder behind a PDF keeps its own caret: only the view moved.
    assert_eq!(second.session.sel.focus.off, 0);
}

/// Changing a file between visits must not drop the reader into the middle of different
/// text at a stale character offset.
#[test]
fn a_changed_file_is_opened_at_its_beginning() {
    let dir = std::env::temp_dir().join(format!("wc-view-changed-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\ntwo\nthree").unwrap();
    let name = path.to_string_lossy().to_string();
    let mut first = app();
    first.run("file.open", json!({"path": name})).unwrap();
    first.run("caret.docEnd", json!({})).unwrap();
    first.capture_view_position();
    let saved = serde_json::to_string(&first.prefs()).unwrap();

    std::fs::write(&path, "a completely different document with many more paragraphs\nand another\nand another\nand another").unwrap();
    let mut second = app();
    second.apply_prefs(UiState::from_json(saved.as_bytes()).unwrap());
    second.run("file.open", json!({"path": name})).unwrap();
    assert_eq!(second.session.sel.focus.off, 0, "a different file is not restored at a stale offset");
    assert_eq!(second.canvas.restore_view, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ui.json` is user-editable: junk reading positions are dropped, not trusted.
#[test]
fn hostile_view_positions_are_dropped() {
    let ui = UiState::from_json(
        br#"{"positions": {"bad.pdf": {"page": 1e9, "top": 1e30, "caret": {"nope": 1}, "fingerprint": 4}, "": {"page": 0}, "good.pdf": {"page": 2, "top": 41, "fingerprint": 4}}}"#,
    )
    .unwrap();
    assert_eq!(ui.positions.len(), 1, "{:?}", ui.positions);
    let good = ui.positions.get("good.pdf").unwrap();
    assert_eq!((good.page, good.top, good.fingerprint), (2, 40.0, 4), "offsets are rounded to the grid");
    // A saved file with a page far beyond anything real still restores clamped, never panics.
    let ui = UiState::from_json(br#"{"positions": {"huge.pdf": {"page": 999999, "top": 99999.0, "fingerprint": 1}}}"#).unwrap();
    assert_eq!(ui.positions.get("huge.pdf").map(|p| p.page), Some(999999));
}

/// The control channel can read and forget the remembered places.
#[test]
fn view_position_command_reads_and_clears() {
    let mut a = app();
    a.session.path = Some("notes.txt".into());
    let r = a.run("ui.viewPosition", json!({})).unwrap();
    assert_eq!(r["path"], json!("notes.txt"));
    assert_eq!(r["count"], json!(1));
    assert!(r["saved"]["caret"].is_object(), "{r}");
    let r = a.run("ui.viewPosition", json!({"clear": true})).unwrap();
    assert_eq!(r["count"], json!(0));
    assert!(r["saved"].is_null());
}

/// `scroll_to_position` and `position_at_scroll` are inverses: a saved (page, top) scrolls
/// back to exactly that place in content coordinates.
#[test]
fn scroll_round_trips_through_a_saved_position() {
    let mut a = app();
    a.canvas.page_content_rects =
        (0..3).map(|i| egui::Rect::from_min_size(egui::pos2(18.0, 18.0 + i as f32 * 900.0), egui::vec2(600.0, 842.0))).collect();
    a.canvas.scale = 1.0;
    let rects = a.canvas.page_content_rects.clone();
    let want = (2usize, 120.0f32);
    let offset = crate::canvas::scroll_to_position(&rects, 1.0, want.0, want.1).unwrap();
    a.canvas.scroll_offset = offset;
    assert_eq!(crate::canvas::position_at_scroll(&a.canvas, false), Some(want));
}

/// The PDF navigation/selection commands are wired, and hostile parameters never panic.
#[test]
fn pdf_link_and_selection_commands_are_wired() {
    let mut a = app();
    // Without a PDF these are clean errors, never panics or blank-document commands.
    assert!(a.run("ui.pdf.goto", json!({"page": 3})).is_err());
    assert!(a.run("ui.pdf.select", json!({"from": [0, 0], "to": [0, 1]})).is_err());
    assert!(a.run("ui.pdf.links", json!({"page": 0})).is_err());
    assert!(crate::canvas::pdf_selected_text(&a).is_none());

    a.run("file.open", json!({"path": "notes.pdf", "data": PDF_WITH_TEXT})).unwrap();
    let pages = a.session.pdf.as_ref().map_or(1, |p| p.page_count());

    // Navigation clamps an out-of-range page and remembers the requested top offset.
    let r = a.run("ui.pdf.goto", json!({"page": 999, "top": 12.5})).unwrap();
    assert_eq!(r["page"], pages);
    assert_eq!(a.canvas.pdf_goto, Some((pages - 1, 12.5)));
    let r = a.run("ui.pdf.links", json!({"page": 0})).unwrap();
    assert!(r["links"].is_array());

    // A selection is UI-only: it never touches Session.sel or the undo history.
    let before = a.session.sel.clone();
    a.run("ui.pdf.select", json!({"from": [0, 0], "to": [0, 5]})).unwrap();
    assert_eq!(a.canvas.pdf_selection.unwrap().ends(), ((0, 0), (0, 5)));
    assert_eq!(a.session.sel, before);
    assert!(a.run("ui.pdf.copy", json!({})).unwrap()["text"].is_string());
    a.run("ui.pdf.select", json!({"from": [0, 9], "to": [0, 2]})).unwrap();
    assert_eq!(a.canvas.pdf_selection.unwrap().ends(), ((0, 2), (0, 9)));

    // Hostile control-channel parameters must not panic.
    for junk in [json!({"page": -1}), json!({"page": 1e30}), json!(null), json!({"page": "x"})] {
        let _ = a.run("ui.pdf.goto", junk);
    }
    for junk in [json!({"from": [0, 0]}), json!({"from": [-1, 0], "to": [0, 1]}), json!(null)] {
        let _ = a.run("ui.pdf.select", junk);
    }
    // Switching to a plain document drops the PDF selection and pending jump, even with no
    // frame rendered in between (a script can open something else at once).
    a.run("file.new", json!({})).unwrap();
    assert!(a.canvas.pdf_selection.is_none());
    assert!(a.canvas.pdf_goto.is_none());
    assert!(a.session.pdf.is_none());
}

/// Right-click › Copy, the ribbon's Copy, keytips, the control channel and MCP all send
/// `edit.copy`. With a PDF open it must read the PDF text layer, not the blank placeholder
/// Document behind the view.
#[test]
fn pdf_edit_copy_reads_the_pdf_text_layer() {
    let mut a = app();
    a.run("file.open", json!({"path": "copy.pdf", "data": PDF_WITH_TEXT})).unwrap();
    a.run("ui.pdf.select", json!({"from": [0, 0], "to": [0, 5]})).unwrap();

    let expected = crate::canvas::pdf_selected_text(&a).unwrap_or_default();
    assert!(!expected.is_empty(), "the fixture has a selectable text layer: {expected:?}");

    let from_menu = a.run("edit.copy", json!({})).unwrap();
    assert_eq!(from_menu["text"], expected, "right-click / ribbon Copy must copy the PDF selection");
    let from_script = a.execute("edit.copy", json!({})).unwrap();
    assert_eq!(from_script["text"], expected, "a script's edit.copy must copy the PDF selection");
    assert_eq!(a.run("ui.pdf.copy", json!({})).unwrap()["text"], expected);

    // Read-only: Cut is still refused. A real document restores the ordinary Copy path.
    assert!(a.execute("edit.cut", json!({})).is_err());
    a.run("file.new", json!({})).unwrap();
    assert!(a.session.pdf.is_none());
    assert!(crate::canvas::pdf_selected_text(&a).is_none());
    assert!(a.execute("edit.copy", json!({})).is_ok());
}
