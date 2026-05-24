
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use lopdf::{content::Content, content::Operation, dictionary, Document, Object, Stream};
use pdfium_render::prelude::*;

#[derive(Clone, Debug)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Default)]
pub struct DocInfo {
    pub file_name: String,
    pub file_size: u64,
    pub path: PathBuf,
    pub pages: Vec<PageSize>,
    pub fonts: Vec<EmbeddedFontInfo>,
}

impl DocInfo {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedFontInfo {
    pub id: (u32, u16),
    pub base_font: String,
    pub subtype: String,
    pub is_subset: bool,
    pub is_simple: bool,
    pub program: Option<std::sync::Arc<Vec<u8>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderPurpose {
    Page,
    Thumbnail,
    Preview,
}

pub struct RenderedPage {
    pub index: usize,
    pub scale: f32,
    pub purpose: RenderPurpose,
    pub width_px: u32,
    pub height_px: u32,
    pub rgba: Vec<u8>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayEdit {
    pub page_index: usize,
    pub kind: OverlayKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub ref_width: f32,
    pub ref_height: f32,
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub text: Option<String>,
    pub font_size: Option<f32>,
    pub color: Option<[u8; 3]>,
    pub font_family: Option<String>,
    pub font_embedded_id: Option<(u32, u16)>,
    pub image_data: Option<std::sync::Arc<Vec<u8>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    Text,
    Image,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Text,
    Path,
    Image,
    Shading,
    Form,
    Other,
}

#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub object_index: usize,
    pub kind: ObjectKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}


#[derive(Clone, Debug)]
pub struct PageTextChar {
    pub ch: char,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Default)]
pub struct ObjectDetails {
    pub text: Option<String>,
    pub font_name: Option<String>,
    pub font_size: Option<f32>,
    pub fill_color: Option<[u8; 3]>,
    pub fill_alpha: Option<u8>,
    pub stroke_color: Option<[u8; 3]>,
    pub stroke_alpha: Option<u8>,
    pub stroke_width: Option<f32>,
    pub char_spacing: Option<f32>,
    pub word_spacing: Option<f32>,
    pub is_kerned_tj: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrangeAction {
    BringToFront,
    SendToBack,
    Shift(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DupZOrder {
    #[default]
    OnTop,
    Behind,
    AboveSource,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectEdit {
    pub page_index: usize,
    pub object_index: usize,
    pub dx: f32,
    pub dy: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub text: Option<String>,
    pub fill_color: Option<[u8; 3]>,
    pub fill_alpha: Option<u8>,
    pub stroke_color: Option<[u8; 3]>,
    pub stroke_alpha: Option<u8>,
    pub stroke_width: Option<f32>,
    pub font_size: Option<f32>,
    pub char_spacing: Option<f32>,
    pub word_spacing: Option<f32>,
    pub image_data: Option<std::sync::Arc<Vec<u8>>>,
    pub arrange: Option<ArrangeAction>,
    pub delete: bool,
    pub copy_seq: Option<u32>,
    pub dup_z: DupZOrder,
}

impl ObjectEdit {
    pub fn new(page_index: usize, object_index: usize) -> Self {
        Self {
            page_index,
            object_index,
            dx: 0.0,
            dy: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: None,
            fill_color: None,
            fill_alpha: None,
            stroke_color: None,
            stroke_alpha: None,
            stroke_width: None,
            font_size: None,
            char_spacing: None,
            word_spacing: None,
            image_data: None,
            arrange: None,
            delete: false,
            copy_seq: None,
            dup_z: DupZOrder::OnTop,
        }
    }
    pub fn is_noop(&self) -> bool {
        if self.copy_seq.is_some() {
            return false;
        }
        !self.delete
            && self.dx == 0.0
            && self.dy == 0.0
            && self.scale_x == 1.0
            && self.scale_y == 1.0
            && self.rotation == 0.0
            && !self.flip_horizontal
            && !self.flip_vertical
            && self.text.is_none()
            && self.fill_color.is_none()
            && self.fill_alpha.is_none()
            && self.stroke_color.is_none()
            && self.stroke_alpha.is_none()
            && self.stroke_width.is_none()
            && self.font_size.is_none()
            && self.char_spacing.is_none()
            && self.word_spacing.is_none()
            && self.image_data.is_none()
            && self.arrange.is_none()
    }
    fn is_pure_arrange(&self) -> bool {
        self.arrange.is_some()
            && self.copy_seq.is_none()
            && !self.delete
            && self.dx == 0.0
            && self.dy == 0.0
            && self.scale_x == 1.0
            && self.scale_y == 1.0
            && self.rotation == 0.0
            && !self.flip_horizontal
            && !self.flip_vertical
            && self.text.is_none()
            && self.fill_color.is_none()
            && self.stroke_color.is_none()
            && self.stroke_width.is_none()
            && self.font_size.is_none()
            && self.char_spacing.is_none()
            && self.word_spacing.is_none()
            && self.image_data.is_none()
    }
}


enum Cmd {
    Open(PathBuf),
    #[allow(dead_code)]
    Close,
    Render { index: usize, scale: f32, generation: u64, purpose: RenderPurpose },
    ListObjects { page: usize, generation: u64 },
    ObjectDetails { page: usize, object_index: usize, generation: u64 },
    SetObjectEdits(Vec<ObjectEdit>),
    Export { output: PathBuf, overlay_edits: Vec<OverlayEdit>, object_edits: Vec<ObjectEdit>, source: PathBuf },
    RenderPreview {
        page: usize,
        scale: f32,
        generation: u64,
        overlay_edits: Vec<OverlayEdit>,
        object_edits: Vec<ObjectEdit>,
        source: PathBuf,
    },
    LoadPageText { page: usize, generation: u64 },
    ExportObjectImage { page: usize, object_index: usize, output: PathBuf },
    Quit,
}

pub enum Event {
    Opened(DocInfo),
    Rendered(RenderedPage),
    Objects {
        page: usize,
        generation: u64,
        objects: Vec<ObjectInfo>,
        safe_to_edit: bool,
    },
    ObjectDetails { page: usize, object_index: usize, generation: u64, details: ObjectDetails },
    Exported(PathBuf),
    PageText { page: usize, generation: u64, chars: Vec<PageTextChar> },
    ObjectImageExported(PathBuf),
    Error(String),
}

pub struct PdfHandle {
    tx: Sender<Cmd>,
    rx: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

pub type WakeFn = Box<dyn Fn() + Send + Sync>;

impl PdfHandle {
    pub fn spawn(wake: WakeFn) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<Event>();
        let thread = std::thread::Builder::new()
            .name("pdf-engine".into())
            .spawn(move || engine_loop(cmd_rx, evt_tx, wake))
            .expect("spawn pdf-engine thread");
        Self { tx: cmd_tx, rx: evt_rx, thread: Some(thread) }
    }

    pub fn open(&self, path: PathBuf) {
        let _ = self.tx.send(Cmd::Open(path));
    }
    #[allow(dead_code)]
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
    pub fn request_render(&self, index: usize, scale: f32, generation: u64, purpose: RenderPurpose) {
        let _ = self.tx.send(Cmd::Render { index, scale, generation, purpose });
    }
    pub fn list_objects(&self, page: usize, generation: u64) {
        let _ = self.tx.send(Cmd::ListObjects { page, generation });
    }
    pub fn request_object_details(&self, page: usize, object_index: usize, generation: u64) {
        let _ = self.tx.send(Cmd::ObjectDetails { page, object_index, generation });
    }
    pub fn set_object_edits(&self, edits: Vec<ObjectEdit>) {
        let _ = self.tx.send(Cmd::SetObjectEdits(edits));
    }
    pub fn export(&self, source: PathBuf, output: PathBuf, overlay_edits: Vec<OverlayEdit>, object_edits: Vec<ObjectEdit>) {
        let _ = self.tx.send(Cmd::Export { output, overlay_edits, object_edits, source });
    }

    pub fn render_preview(
        &self,
        source: PathBuf,
        page: usize,
        scale: f32,
        generation: u64,
        overlay_edits: Vec<OverlayEdit>,
        object_edits: Vec<ObjectEdit>,
    ) {
        let _ = self.tx.send(Cmd::RenderPreview {
            page,
            scale,
            generation,
            overlay_edits,
            object_edits,
            source,
        });
    }

    pub fn load_page_text(&self, page: usize, generation: u64) {
        let _ = self.tx.send(Cmd::LoadPageText { page, generation });
    }

    pub fn export_object_image(&self, page: usize, object_index: usize, output: PathBuf) {
        let _ = self.tx.send(Cmd::ExportObjectImage { page, object_index, output });
    }

    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(e) => out.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

impl Drop for PdfHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        let _ = self.thread.take();
    }
}


struct Engine {
    doc: Option<PdfDocument<'static>>,
    edited_doc: Option<PdfDocument<'static>>,
    lopdf_source: Option<lopdf::Document>,
    pdfium: Box<Pdfium>,
    info: Option<DocInfo>,
    object_edits: Vec<ObjectEdit>,
    generation: u64,
}

fn engine_loop(rx: Receiver<Cmd>, tx: Sender<Event>, wake: WakeFn) {
    let pdfium = match load_pdfium() {
        Ok(p) => Box::new(p),
        Err(e) => {
            let _ = tx.send(Event::Error(format!("Could not initialise PDFium: {e}")));
            wake();
            return;
        }
    };
    let mut engine = Engine {
        doc: None,
        edited_doc: None,
        lopdf_source: None,
        pdfium,
        info: None,
        object_edits: Vec::new(),
        generation: 0,
    };

    let mut queue: VecDeque<Cmd> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv() {
                Ok(c) => queue.push_back(c),
                Err(_) => return,
            }
        }
        while let Ok(c) = rx.try_recv() {
            queue.push_back(c);
        }
        if queue.iter().any(|c| matches!(c, Cmd::Quit)) {
            return;
        }
        dedup_render_requests(&mut queue);
        let pick = queue
            .iter()
            .position(|c| !matches!(c, Cmd::Render { purpose: RenderPurpose::Thumbnail, .. }))
            .unwrap_or(0);
        if let Some(cmd) = queue.remove(pick) {
            handle_cmd(&mut engine, &tx, cmd);
            wake();
        }
    }
}

fn dedup_render_requests(queue: &mut VecDeque<Cmd>) {
    let mut seen: HashSet<(usize, RenderPurpose)> = HashSet::new();
    let mut kept: VecDeque<Cmd> = VecDeque::new();
    while let Some(cmd) = queue.pop_back() {
        match &cmd {
            Cmd::Render { index, purpose, .. } => {
                if seen.insert((*index, *purpose)) {
                    kept.push_front(cmd);
                }
            }
            _ => kept.push_front(cmd),
        }
    }
    *queue = kept;
}

fn handle_cmd(engine: &mut Engine, tx: &Sender<Event>, cmd: Cmd) {
    match cmd {
        Cmd::Quit => {}
        Cmd::Close => {
            engine.edited_doc = None;
            engine.doc = None;
            engine.lopdf_source = None;
            engine.info = None;
            engine.object_edits.clear();
        }
        Cmd::Open(path) => match engine.open(&path) {
            Ok(info) => {
                let _ = tx.send(Event::Opened(info));
            }
            Err(e) => {
                let _ = tx.send(Event::Error(format!("Could not open {}: {e}", path.display())));
            }
        },
        Cmd::Render { index, scale, generation, purpose } => {
            if generation != engine.generation {
                return;
            }
            match engine.render(index, scale, purpose) {
                Ok(page) => {
                    let _ = tx.send(Event::Rendered(page));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("Render of page {} failed: {e}", index + 1)));
                }
            }
        }
        Cmd::ListObjects { page, generation } => {
            if generation != engine.generation {
                return;
            }
            match engine.list_objects(page) {
                Ok((objects, safe_to_edit)) => {
                    let _ = tx.send(Event::Objects { page, generation, objects, safe_to_edit });
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("Listing objects on page {} failed: {e}", page + 1)));
                }
            }
        }
        Cmd::ObjectDetails { page, object_index, generation } => {
            if generation != engine.generation {
                return;
            }
            match engine.object_details(page, object_index) {
                Ok(details) => {
                    let _ = tx.send(Event::ObjectDetails { page, object_index, generation, details });
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("Reading object #{object_index} on page {} failed: {e}", page + 1)));
                }
            }
        }
        Cmd::SetObjectEdits(edits) => {
            engine.object_edits = edits;
            if let Err(msg) = engine.rebuild_edited_doc() {
                let _ = tx.send(Event::Error(msg));
            }
        }
        Cmd::Export { output, overlay_edits, object_edits, source } => {
            match export_all(&engine.pdfium, &source, &output, &overlay_edits, &object_edits) {
                Ok(()) => {
                    let _ = tx.send(Event::Exported(output));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("Export failed: {e}")));
                }
            }
        }
        Cmd::RenderPreview { page, scale, generation, overlay_edits, object_edits, source } => {
            if generation != engine.generation {
                return;
            }
            match render_preview_page(
                &engine.pdfium,
                &source,
                page,
                scale,
                generation,
                &overlay_edits,
                &object_edits,
            ) {
                Ok(rendered) => {
                    let _ = tx.send(Event::Rendered(rendered));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("Preview of page {} failed: {e}", page + 1)));
                }
            }
        }
        Cmd::LoadPageText { page, generation } => {
            if generation != engine.generation {
                return;
            }
            match engine.load_page_text(page) {
                Ok(chars) => {
                    let _ = tx.send(Event::PageText { page, generation, chars });
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!(
                        "Reading text on page {} failed: {e}",
                        page + 1
                    )));
                }
            }
        }
        Cmd::ExportObjectImage { page, object_index, output } => {
            match engine.export_object_image(page, object_index, &output) {
                Ok(()) => {
                    let _ = tx.send(Event::ObjectImageExported(output));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!(
                        "Exporting image on page {} (#{object_index}) failed: {e}",
                        page + 1
                    )));
                }
            }
        }
    }
}

impl Engine {
    fn open(&mut self, path: &Path) -> Result<DocInfo, PdfiumError> {
        self.edited_doc = None;
        self.doc = None;
        self.lopdf_source = None;
        self.object_edits.clear();
        let doc = self.pdfium.load_pdf_from_file(path, None)?;
        let doc: PdfDocument<'static> = unsafe { std::mem::transmute(doc) };

        let mut pages = Vec::with_capacity(doc.pages().len() as usize);
        for page in doc.pages().iter() {
            pages.push(PageSize { width: page.width().value, height: page.height().value });
        }

        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.pdf")
            .to_string();

        let fonts = lopdf::Document::load(path)
            .map(|d| enumerate_embedded_fonts(&d))
            .unwrap_or_default();

        let info = DocInfo { file_name, file_size, path: path.to_path_buf(), pages, fonts };
        self.doc = Some(doc);
        self.info = Some(info.clone());
        self.generation += 1;
        Ok(info)
    }

    fn render(&self, index: usize, scale: f32, purpose: RenderPurpose) -> Result<RenderedPage, PdfiumError> {
        let info = self.info.as_ref().ok_or(PdfiumError::PageIndexOutOfBounds)?;
        let size = info.pages.get(index).ok_or(PdfiumError::PageIndexOutOfBounds)?;

        let target_w = ((size.width * scale).round() as i32).clamp(8, 8000);
        let target_h = ((size.height * scale).round() as i32).clamp(8, 12000);

        let use_edited = self.edited_doc.is_some()
            && self.object_edits.iter().any(|e| e.page_index == index && !e.is_noop());
        let doc = if use_edited {
            self.edited_doc.as_ref().unwrap()
        } else {
            self.doc.as_ref().ok_or(PdfiumError::PageIndexOutOfBounds)?
        };

        let page = doc.pages().get(index as u16)?;
        let cfg = PdfRenderConfig::new()
            .set_target_width(target_w)
            .set_maximum_height(target_h)
            .set_image_smoothing(true)
            .set_text_smoothing(true);
        let bitmap = page.render_with_config(&cfg)?;
        let rgba_img = bitmap.as_image().into_rgba8();
        let (w, h) = (rgba_img.width(), rgba_img.height());
        Ok(RenderedPage {
            index,
            scale,
            purpose,
            width_px: w,
            height_px: h,
            rgba: rgba_img.into_raw(),
            generation: self.generation,
        })
    }

    fn page(&self, index: usize) -> Result<PdfPage<'_>, PdfiumError> {
        let doc = self.doc.as_ref().ok_or(PdfiumError::PageIndexOutOfBounds)?;
        doc.pages().get(index as u16)
    }

    fn list_objects(&self, page_index: usize) -> Result<(Vec<ObjectInfo>, bool), PdfiumError> {
        let page = self.page(page_index)?;
        let vb = visible_box(&page);
        let (vleft, vtop) = (vb.left().value, vb.top().value);
        let safe = page_is_simple(&page);
        let mut out = Vec::new();
        for (object_index, object) in page.objects().iter().enumerate() {
            let Ok(quad) = object.bounds() else { continue };
            let rect = quad.to_rect();
            let width = rect.width().value;
            let height = rect.height().value;
            if width < 0.5 || height < 0.5 {
                continue;
            }
            out.push(ObjectInfo {
                object_index,
                kind: object_kind(object.object_type()),
                x: rect.left().value - vleft,
                y: vtop - rect.top().value,
                width,
                height,
            });
        }
        Ok((out, safe))
    }

    fn object_details(&self, page_index: usize, object_index: usize) -> Result<ObjectDetails, PdfiumError> {
        let page = self.page(page_index)?;
        let object = page.objects().get(object_index)?;
        let text_obj = object.as_text_object();
        let fill = object.fill_color().ok();
        let stroke = object.stroke_color().ok();
        let stroke_width = if let Some(p) = object.as_path_object() {
            match p.is_stroked() {
                Ok(true) => object.stroke_width().ok().map(|w| w.value),
                _ => Some(0.0),
            }
        } else {
            object.stroke_width().ok().map(|w| w.value)
        };
        let is_kerned_tj = text_obj.is_some() && self.text_op_has_kerning(page_index, object_index);
        let (cur_tc, cur_tw) = if text_obj.is_some() {
            self.text_state_at(page_index, object_index)
        } else {
            (None, None)
        };
        Ok(ObjectDetails {
            text: text_obj.map(|t| t.text()),
            font_name: text_obj.map(|t| t.font().name()),
            font_size: text_obj.map(|t| t.unscaled_font_size().value),
            fill_color: fill.map(|c| [c.red(), c.green(), c.blue()]),
            fill_alpha: fill.map(|c| c.alpha()),
            stroke_color: stroke.map(|c| [c.red(), c.green(), c.blue()]),
            stroke_alpha: stroke.map(|c| c.alpha()),
            stroke_width,
            char_spacing: cur_tc,
            word_spacing: cur_tw,
            is_kerned_tj,
        })
    }

    fn text_state_at(&self, page_index: usize, object_index: usize) -> (Option<f32>, Option<f32>) {
        let Some(lopdf_doc) = self
            .lopdf_source
            .as_ref()
            .cloned()
            .or_else(|| self.info.as_ref().and_then(|i| lopdf::Document::load(&i.path).ok()))
        else {
            return (None, None);
        };
        let pages = lopdf_doc.get_pages();
        let Some(&page_id) = pages.get(&(page_index as u32 + 1)) else { return (None, None) };
        let Ok(content_bytes) = lopdf_doc.get_page_content(page_id) else { return (None, None) };
        let Ok(content) = lopdf::content::Content::decode(&content_bytes) else { return (None, None) };
        let (_, snapshots) = walk_with_snapshots(&content.operations);
        let Some(snap) = snapshots.get(object_index) else { return (None, None) };
        let extract = |op: &Operation| -> Option<f32> {
            match op.operands.first()? {
                Object::Real(n) => Some(*n),
                Object::Integer(n) => Some(*n as f32),
                _ => None,
            }
        };
        let tc = snap
            .char_spacing
            .as_ref()
            .and_then(extract)
            .filter(|v| v.abs() > f32::EPSILON);
        let tw = snap
            .word_spacing
            .as_ref()
            .and_then(extract)
            .filter(|v| v.abs() > f32::EPSILON);
        (tc, tw)
    }

    fn text_op_has_kerning(&self, page_index: usize, object_index: usize) -> bool {
        let Some(lopdf_doc) = self
            .lopdf_source
            .as_ref()
            .cloned()
            .or_else(|| self.info.as_ref().and_then(|i| lopdf::Document::load(&i.path).ok()))
        else {
            return false;
        };
        let pages = lopdf_doc.get_pages();
        let Some(&page_id) = pages.get(&(page_index as u32 + 1)) else { return false };
        let Ok(content_bytes) = lopdf_doc.get_page_content(page_id) else { return false };
        let Ok(content) = lopdf::content::Content::decode(&content_bytes) else { return false };
        let objects = enumerate_content_objects(&content.operations);
        let Some(obj) = objects.get(object_index) else { return false };
        if obj.kind != ObjectKind::Text {
            return false;
        }
        let Some(op) = content.operations.get(obj.op_start) else { return false };
        if op.operator != "TJ" {
            return false;
        }
        match op.operands.first() {
            Some(Object::Array(arr)) => arr
                .iter()
                .any(|e| matches!(e, Object::Integer(_) | Object::Real(_))),
            _ => false,
        }
    }

    fn rebuild_edited_doc(&mut self) -> Result<(), String> {
        self.edited_doc = None;
        let effective: Vec<ObjectEdit> =
            self.object_edits.iter().filter(|e| !e.is_noop()).cloned().collect();
        if effective.is_empty() {
            return Ok(());
        }
        let path = match &self.info {
            Some(i) => i.path.clone(),
            None => return Ok(()),
        };

        let mut by_page: HashMap<usize, Vec<ObjectEdit>> = HashMap::new();
        for e in effective {
            by_page.entry(e.page_index).or_default().push(e);
        }

        let plan = match self.doc.as_ref() {
            Some(d) => plan_edits(d, by_page),
            None => EditPlan { surgical: HashMap::new(), pdfium: by_page },
        };

        let doc = if !plan.surgical.is_empty() {
            let bytes = self
                .surgical_apply(&path, &plan.surgical)
                .map_err(|e| format!("Surgical edit failed: {e}"))?;
            self.pdfium
                .load_pdf_from_byte_vec(bytes, None)
                .map_err(|e| format!("Could not reopen edited bytes: {e}"))?
        } else {
            self.pdfium
                .load_pdf_from_file(&path, None)
                .map_err(|e| format!("Could not reopen {} for editing: {e}", path.display()))?
        };
        let mut doc: PdfDocument<'static> = unsafe { std::mem::transmute(doc) };

        for (page_index, edits) in plan.pdfium {
            if let Ok(mut page) = doc.pages_mut().get(page_index as u16) {
                let mut refs: Vec<&ObjectEdit> = edits.iter().collect();
                let _ = apply_object_edits_to_page(&mut page, &mut refs);
            }
        }
        self.edited_doc = Some(doc);
        Ok(())
    }

    fn surgical_apply(
        &mut self,
        path: &Path,
        surgical: &HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if self.lopdf_source.is_none() {
            self.lopdf_source = Some(lopdf::Document::load(path)?);
        }
        let encoded = encode_text_edits_via_pdfium(&self.pdfium, path, surgical);
        let mut doc = self
            .lopdf_source
            .as_ref()
            .expect("just populated")
            .clone();
        apply_surgical_edits_to_doc_with_encoded(&mut doc, surgical, &encoded)?;
        let mut out = Vec::new();
        doc.save_to(&mut out)?;
        Ok(out)
    }

    fn export_object_image(
        &self,
        page_index: usize,
        object_index: usize,
        output: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let doc = self.doc.as_ref().ok_or("No document open")?;
        let page = doc.pages().get(page_index as u16)?;
        let object = page.objects().get(object_index)?;
        let image_object = object
            .as_image_object()
            .ok_or("Selected object is not an image")?;
        if let Some(parent) = output.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let img = image_object.get_processed_image(doc)?;
        img.save_with_format(output, image::ImageFormat::Png)?;
        Ok(())
    }

    fn load_page_text(&self, page_index: usize) -> Result<Vec<PageTextChar>, PdfiumError> {
        let page = self.page(page_index)?;
        let vb = visible_box(&page);
        let (vleft, vtop) = (vb.left().value, vb.top().value);
        let text_page = page.text()?;
        let mut out = Vec::new();
        for ch in text_page.chars().iter() {
            let unicode = match ch.unicode_char() {
                Some(c) if !c.is_control() => c,
                _ => continue,
            };
            if let Ok(b) = ch.loose_bounds() {
                let x = b.left().value - vleft;
                let y = vtop - b.top().value;
                let width = (b.right().value - b.left().value).max(0.0);
                let height = (b.top().value - b.bottom().value).max(0.0);
                if width >= 0.1 && height >= 0.1 {
                    out.push(PageTextChar { ch: unicode, x, y, width, height });
                }
            }
        }
        Ok(out)
    }
}


fn apply_object_edits_to_page(page: &mut PdfPage, edits: &mut Vec<&ObjectEdit>) -> Result<(), PdfiumError> {
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    if page_is_simple(page) {
        preserve_page_object_styles(page)?;
    }
    edits.sort_by(|a, b| b.object_index.cmp(&a.object_index));
    for edit in edits.iter() {
        if edit.delete {
            if edit.object_index < page.objects().len() {
                page.objects_mut().remove_object_at_index(edit.object_index)?;
            }
            continue;
        }
        if edit.is_pure_arrange() {
            continue;
        }
        if edit.object_index >= page.objects().len() {
            continue;
        }
        let mut object = page.objects_mut().get(edit.object_index)?;
        apply_object_property_edit(&mut object, edit)?;
        let has_transform = edit.dx != 0.0
            || edit.dy != 0.0
            || edit.scale_x != 1.0
            || edit.scale_y != 1.0
            || edit.rotation != 0.0
            || edit.flip_horizontal
            || edit.flip_vertical;
        if has_transform {
            let rect = object.bounds()?.to_rect();
            let cx = rect.left().value + rect.width().value / 2.0;
            let cy = rect.bottom().value + rect.height().value / 2.0;
            let sx = edit.scale_x.max(0.01) * if edit.flip_horizontal { -1.0 } else { 1.0 };
            let sy = edit.scale_y.max(0.01) * if edit.flip_vertical { -1.0 } else { 1.0 };
            let matrix = PdfMatrix::identity()
                .translate(PdfPoints::new(-cx), PdfPoints::new(-cy))?
                .scale(sx, sy)?
                .rotate_clockwise_degrees(edit.rotation)?
                .translate(PdfPoints::new(cx + edit.dx), PdfPoints::new(cy - edit.dy))?;
            object.apply_matrix(matrix)?;
        }
    }
    apply_arrange_edits_to_page(page, edits)?;
    page.regenerate_content()?;
    Ok(())
}

fn apply_arrange_edits_to_page(page: &mut PdfPage, edits: &[&ObjectEdit]) -> Result<(), PdfiumError> {
    let arrange: Vec<&ObjectEdit> = edits.iter().copied().filter(|e| e.arrange.is_some() && !e.delete).collect();
    if arrange.is_empty() {
        return Ok(());
    }
    let deleted: Vec<usize> = edits.iter().filter(|e| e.delete).map(|e| e.object_index).collect();
    let count = page.objects().len();
    let current_order: Vec<usize> = (0..count + deleted.len()).filter(|i| !deleted.contains(i)).collect();
    if current_order.len() != count {
        return Ok(());
    }
    let mut target_order = current_order.clone();
    for edit in &arrange {
        if let Some(action) = edit.arrange {
            target_order = arrange_object_index_order(&target_order, edit.object_index, action);
        }
    }
    if target_order == current_order {
        return Ok(());
    }
    let mut pulled: Vec<(usize, PdfPageObject)> = Vec::with_capacity(count);
    for position in (0..count).rev() {
        let original_index = current_order[position];
        let object = page.objects_mut().remove_object_at_index(position)?;
        pulled.push((original_index, object));
    }
    for original_index in target_order {
        if let Some(pos) = pulled.iter().position(|(idx, _)| *idx == original_index) {
            let (_, object) = pulled.remove(pos);
            page.objects_mut().add_object(object)?;
        }
    }
    Ok(())
}

fn arrange_object_index_order(order: &[usize], object_index: usize, action: ArrangeAction) -> Vec<usize> {
    let Some(position) = order.iter().position(|i| *i == object_index) else {
        return order.to_vec();
    };
    let mut next = order.to_vec();
    let moved = next.remove(position);
    let limit = next.len() as i32;
    let new_pos = match action {
        ArrangeAction::BringToFront => limit,
        ArrangeAction::SendToBack => 0,
        ArrangeAction::Shift(n) => (position as i32 + n).clamp(0, limit),
    };
    next.insert(new_pos as usize, moved);
    next
}

fn page_is_simple(page: &PdfPage) -> bool {
    page.objects().iter().all(|o| {
        let opaque = o.fill_color().map_or(true, |c| c.alpha() == 255)
            && o.stroke_color().map_or(true, |c| c.alpha() == 255);
        opaque
            && matches!(
                o.object_type(),
                PdfPageObjectType::Text | PdfPageObjectType::Path | PdfPageObjectType::Image
            )
    })
}

fn preserve_page_object_styles(page: &mut PdfPage) -> Result<(), PdfiumError> {
    let count = page.objects().len();
    for i in 0..count {
        let mut object = page.objects_mut().get(i)?;
        if !matches!(object.object_type(), PdfPageObjectType::Text | PdfPageObjectType::Path) {
            continue;
        }
        if let Ok(c) = object.fill_color() {
            object.set_fill_color(c)?;
        }
        if let Ok(c) = object.stroke_color() {
            object.set_stroke_color(c)?;
        }
        if let Ok(w) = object.stroke_width() {
            object.set_stroke_width(w)?;
        }
        if let Some(path_object) = object.as_path_object_mut() {
            if let (Ok(fill_mode), Ok(stroked)) = (path_object.fill_mode(), path_object.is_stroked()) {
                path_object.set_fill_and_stroke_mode(fill_mode, stroked)?;
            }
        }
    }
    Ok(())
}

fn apply_object_property_edit(object: &mut PdfPageObject, edit: &ObjectEdit) -> Result<(), PdfiumError> {
    if let Some(data) = &edit.image_data {
        if object.as_image_object_mut().is_some() {
            let rect = object.bounds()?.to_rect();
            let cx = rect.left().value + rect.width().value / 2.0;
            let cy = rect.bottom().value + rect.height().value / 2.0;
            let (ow, oh) = (rect.width().value, rect.height().value);
            if let Ok(img) = image::load_from_memory(data) {
                if let Some(image_object) = object.as_image_object_mut() {
                    image_object.set_image(&img)?;
                }
                let next = object.bounds()?.to_rect();
                let (nw, nh) = (next.width().value, next.height().value);
                let ncx = next.left().value + nw / 2.0;
                let ncy = next.bottom().value + nh / 2.0;
                if ow > 0.0 && oh > 0.0 && nw > 0.0 && nh > 0.0 {
                    let matrix = PdfMatrix::identity()
                        .translate(PdfPoints::new(-ncx), PdfPoints::new(-ncy))?
                        .scale(ow / nw, oh / nh)?
                        .translate(PdfPoints::new(cx), PdfPoints::new(cy))?;
                    object.apply_matrix(matrix)?;
                }
            }
        }
    }
    if let Some(text) = &edit.text {
        if let Some(t) = object.as_text_object_mut() {
            t.set_text(text)?;
        }
    }
    if let Some(font_size) = edit.font_size {
        if let Some(t) = object.as_text_object() {
            let current = t.unscaled_font_size().value;
            if current > 0.0 && font_size > 0.0 {
                let rect = object.bounds()?.to_rect();
                let cx = rect.left().value + rect.width().value / 2.0;
                let cy = rect.bottom().value + rect.height().value / 2.0;
                let ratio = (font_size / current).clamp(0.05, 20.0);
                let matrix = PdfMatrix::identity()
                    .translate(PdfPoints::new(-cx), PdfPoints::new(-cy))?
                    .scale(ratio, ratio)?
                    .translate(PdfPoints::new(cx), PdfPoints::new(cy))?;
                object.apply_matrix(matrix)?;
            }
        }
    }
    if let Some(c) = edit.fill_color {
        object.set_fill_color(PdfColor::new(c[0], c[1], c[2], edit.fill_alpha.unwrap_or(255)))?;
    }
    if let Some(c) = edit.stroke_color {
        object.set_stroke_color(PdfColor::new(c[0], c[1], c[2], edit.stroke_alpha.unwrap_or(255)))?;
    }
    if let Some(w) = edit.stroke_width {
        object.set_stroke_width(PdfPoints::new(w.max(0.0)))?;
    }
    if object.object_type() == PdfPageObjectType::Path
        && (edit.fill_color.is_some() || edit.stroke_color.is_some() || edit.stroke_width.is_some())
    {
        let existing_stroke_w = object.stroke_width().map(|w| w.value).unwrap_or(0.0);
        if let Some(path_object) = object.as_path_object_mut() {
            let fill_mode = if edit.fill_color.is_some() {
                PdfPathFillMode::Winding
            } else {
                path_object.fill_mode().unwrap_or(PdfPathFillMode::Winding)
            };
            let was_stroked = path_object.is_stroked().unwrap_or(false);
            let do_stroke = if let Some(w) = edit.stroke_width {
                w > 0.0
            } else if edit.stroke_color.is_some() {
                existing_stroke_w > 0.0 || !was_stroked
            } else {
                was_stroked
            };
            path_object.set_fill_and_stroke_mode(fill_mode, do_stroke)?;
        }
    }
    Ok(())
}

fn render_preview_page(
    pdfium: &Pdfium,
    source: &Path,
    page_index: usize,
    scale: f32,
    generation: u64,
    overlay_edits: &[OverlayEdit],
    object_edits: &[ObjectEdit],
) -> Result<RenderedPage, Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join(format!("paper-preview-{}.pdf", uuid::Uuid::new_v4()));
    export_all(pdfium, source, &tmp, overlay_edits, object_edits)?;
    let result = (|| -> Result<RenderedPage, Box<dyn std::error::Error>> {
        let doc = pdfium.load_pdf_from_file(&tmp, None)?;
        let page = doc.pages().get(page_index as u16)?;
        let (w, h) = (page.width().value, page.height().value);
        let target_w = ((w * scale).round() as i32).clamp(8, 8000);
        let target_h = ((h * scale).round() as i32).clamp(8, 12000);
        let cfg = PdfRenderConfig::new()
            .set_target_width(target_w)
            .set_maximum_height(target_h)
            .set_image_smoothing(true)
            .set_text_smoothing(true);
        let bitmap = page.render_with_config(&cfg)?;
        let img = bitmap.as_image().into_rgba8();
        let (pw, ph) = (img.width(), img.height());
        Ok(RenderedPage {
            index: page_index,
            scale,
            purpose: RenderPurpose::Preview,
            width_px: pw,
            height_px: ph,
            rgba: img.into_raw(),
            generation,
        })
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

fn export_all(
    pdfium: &Pdfium,
    source: &Path,
    output: &Path,
    overlay_edits: &[OverlayEdit],
    object_edits: &[ObjectEdit],
) -> Result<(), Box<dyn std::error::Error>> {
    let effective: Vec<ObjectEdit> =
        object_edits.iter().filter(|e| !e.is_noop()).cloned().collect();
    if effective.is_empty() {
        return export_overlays(pdfium, source, output, overlay_edits);
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut by_page: HashMap<usize, Vec<ObjectEdit>> = HashMap::new();
    for e in effective {
        by_page.entry(e.page_index).or_default().push(e);
    }
    let plan = {
        let src_doc = pdfium.load_pdf_from_file(source, None)?;
        plan_edits(&src_doc, by_page)
    };
    let mut doc = if !plan.surgical.is_empty() {
        let encoded = encode_text_edits_via_pdfium(pdfium, source, &plan.surgical);
        let mut lopdf_doc = lopdf::Document::load(source)?;
        apply_surgical_edits_to_doc_with_encoded(&mut lopdf_doc, &plan.surgical, &encoded)?;
        let mut bytes = Vec::new();
        lopdf_doc.save_to(&mut bytes)?;
        pdfium.load_pdf_from_byte_vec(bytes, None)?
    } else {
        pdfium.load_pdf_from_file(source, None)?
    };
    for (page_index, edits) in plan.pdfium {
        let mut page = doc.pages_mut().get(page_index as u16)?;
        let mut refs: Vec<&ObjectEdit> = edits.iter().collect();
        apply_object_edits_to_page(&mut page, &mut refs)?;
    }
    if overlay_edits.is_empty() {
        doc.save_to_file(output)?;
    } else {
        let tmp = std::env::temp_dir().join(format!("paper-export-{}.pdf", uuid::Uuid::new_v4()));
        doc.save_to_file(&tmp)?;
        drop(doc);
        let result = export_overlays(pdfium, &tmp, output, overlay_edits);
        let _ = std::fs::remove_file(&tmp);
        result?;
    }
    Ok(())
}

fn visible_box(page: &PdfPage) -> PdfRect {
    page.boundaries()
        .crop()
        .or_else(|_| page.boundaries().media())
        .map(|b| b.bounds)
        .unwrap_or_else(|_| page.page_size())
}

fn object_kind(t: PdfPageObjectType) -> ObjectKind {
    match t {
        PdfPageObjectType::Text => ObjectKind::Text,
        PdfPageObjectType::Path => ObjectKind::Path,
        PdfPageObjectType::Image => ObjectKind::Image,
        PdfPageObjectType::Shading => ObjectKind::Shading,
        PdfPageObjectType::XObjectForm => ObjectKind::Form,
        PdfPageObjectType::Unsupported => ObjectKind::Other,
    }
}


fn load_pdfium() -> Result<Pdfium, PdfiumError> {
    for candidate in bundled_pdfium_candidates() {
        if candidate.exists() {
            if let Ok(bindings) = Pdfium::bind_to_library(&candidate) {
                return Ok(Pdfium::new(bindings));
            }
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new)
}

fn bundled_pdfium_candidates() -> Vec<PathBuf> {
    let lib = Pdfium::pdfium_platform_library_name();
    let plat = pdfium_platform_dir();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = vec![
        manifest.join("pdfium").join(plat).join("lib").join(&lib),
        manifest.join("pdfium").join(&lib),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(&lib));
            out.push(dir.join("pdfium").join(plat).join("lib").join(&lib));
            out.push(dir.join("../Resources/pdfium").join(plat).join("lib").join(&lib));
            out.push(dir.join("../lib").join(&lib));
        }
    }
    out
}

fn pdfium_platform_dir() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "linux-x64" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "linux-arm64" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "macos-x64" }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "macos-arm64" }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { "win-x64" }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    { "win-arm64" }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
    )))]
    { "unsupported" }
}


fn export_overlays(
    _pdfium: &Pdfium,
    input: &Path,
    output: &Path,
    edits: &[OverlayEdit],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut document = Document::load(input)?;
    let pages = document.get_pages();

    let mut font_ids = std::collections::HashMap::new();
    for base_font in ["Helvetica", "Times-Roman", "Courier"] {
        let id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => base_font,
            "Encoding" => "WinAnsiEncoding",
        });
        font_ids.insert(base_font, id);
    }

    let unicode_face = ttf_parser::Face::parse(UNICODE_FONT_BYTES, 0).ok();
    let mut used_gids: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();
    if let Some(face) = &unicode_face {
        for e in edits {
            if e.kind != OverlayKind::Text {
                continue;
            }
            let Some(text) = e.text.as_deref() else { continue };
            if !overlay_text_needs_unicode(text) {
                continue;
            }
            for c in text.chars() {
                if c.is_control() {
                    continue;
                }
                if let Some(g) = face.glyph_index(c) {
                    used_gids.insert(g.0);
                }
            }
        }
    }
    let unicode_font_id = match &unicode_face {
        Some(face) if !used_gids.is_empty() => Some(build_unicode_font(&mut document, face, &used_gids)?),
        _ => None,
    };

    let mut by_page: std::collections::HashMap<usize, Vec<&OverlayEdit>> = std::collections::HashMap::new();
    for e in edits {
        by_page.entry(e.page_index).or_default().push(e);
    }

    for (page_number, page_id) in pages {
        let page_index = page_number as usize - 1;
        let Some(page_edits) = by_page.get(&page_index) else { continue };
        let media = resolve_box(&document, page_id, b"MediaBox").ok_or("page has no MediaBox")?;
        let visible = match resolve_box(&document, page_id, b"CropBox") {
            Some((cl, cb, cr, ct)) => (cl.max(media.0), cb.max(media.1), cr.min(media.2), ct.min(media.3)),
            None => media,
        };
        let (origin_x, top_y) = (visible.0, visible.3);
        let mut ops = Vec::new();
        for e in page_edits {
            append_edit(
                &mut document,
                page_id,
                &mut ops,
                e,
                origin_x,
                top_y,
                &font_ids,
                unicode_font_id,
                unicode_face.as_ref(),
            )?;
        }
        if ops.is_empty() {
            continue;
        }
        let content = Content { operations: ops };
        let stream = Stream::new(dictionary! {}, content.encode()?);
        let stream_id = document.add_object(stream);
        append_content_stream(&mut document, page_id, stream_id)?;
    }

    document.prune_objects();
    document.compress();
    document.save(output)?;
    let meta = std::fs::metadata(output)?;
    if meta.len() == 0 {
        return Err("export produced an empty file".into());
    }
    Ok(())
}

fn append_edit(
    document: &mut Document,
    page_id: lopdf::ObjectId,
    ops: &mut Vec<Operation>,
    edit: &OverlayEdit,
    origin_x: f32,
    top_y: f32,
    font_ids: &std::collections::HashMap<&'static str, lopdf::ObjectId>,
    unicode_font_id: Option<lopdf::ObjectId>,
    unicode_face: Option<&ttf_parser::Face>,
) -> Result<(), Box<dyn std::error::Error>> {
    let x = origin_x + edit.x;
    let y = top_y - edit.y - edit.height;
    let width = edit.width.max(1.0);
    let height = edit.height.max(1.0);

    ops.push(Operation::new("q", vec![]));
    append_transform(
        ops,
        x,
        y,
        width,
        height,
        -edit.rotation,
        edit.flip_horizontal,
        edit.flip_vertical,
    );

    match edit.kind {
        OverlayKind::Text => {
            let color = edit.color.unwrap_or([17, 24, 39]);
            let text = edit.text.as_deref().unwrap_or("");
            let font_size = edit.font_size.unwrap_or(18.0).max(4.0);
            let leading = font_size * 1.2;
            let unicode = match (unicode_font_id, unicode_face) {
                (Some(uid), Some(face)) if overlay_text_needs_unicode(text) => Some((uid, face)),
                _ => None,
            };
            let res: Vec<u8> = if let Some((uid, _)) = unicode {
                ensure_page_resource(document, page_id, b"Font", "PFUni", Object::Reference(uid))?;
                b"PFUni".to_vec()
            } else {
                match edit.font_embedded_id {
                    Some((num, gen)) => {
                        let name = format!("PFEmb{num}_{gen}");
                        ensure_page_resource(document, page_id, b"Font", &name, Object::Reference((num, gen)))?;
                        name.into_bytes()
                    }
                    None => {
                        let base = standard_font_base(edit.font_family.as_deref());
                        let r = standard_font_resource(base);
                        if let Some(id) = font_ids.get(base) {
                            ensure_page_resource(document, page_id, b"Font", r, Object::Reference(*id))?;
                        }
                        r.as_bytes().to_vec()
                    }
                }
            };
            push_rgb(ops, color, "rg");
            ops.push(Operation::new("BT", vec![]));
            ops.push(Operation::new("Tf", vec![Object::Name(res), font_size.into()]));
            ops.push(Operation::new("Td", vec![0.into(), (height - font_size).into()]));
            for (i, raw_line) in text.split('\n').enumerate() {
                if i > 0 {
                    ops.push(Operation::new("Td", vec![0.into(), (-leading).into()]));
                }
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                let operand = match unicode {
                    Some((_, face)) => {
                        Object::String(encode_identity_h(face, line), lopdf::StringFormat::Hexadecimal)
                    }
                    None => Object::string_literal(encode_overlay_text_bytes(line)),
                };
                ops.push(Operation::new("Tj", vec![operand]));
            }
            ops.push(Operation::new("ET", vec![]));
        }
        OverlayKind::Image => {
            if let Some(data) = &edit.image_data {
                let name = format!("PFImage{}", document.max_id + 1);
                let img_id = add_image_xobject(document, data)?;
                ensure_page_resource(document, page_id, b"XObject", &name, Object::Reference(img_id))?;
                ops.push(Operation::new(
                    "cm",
                    vec![width.into(), 0.into(), 0.into(), height.into(), 0.into(), 0.into()],
                ));
                ops.push(Operation::new("Do", vec![Object::Name(name.as_bytes().to_vec())]));
            }
        }
    }

    ops.push(Operation::new("Q", vec![]));
    Ok(())
}

fn append_transform(
    ops: &mut Vec<Operation>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rotation_deg: f32,
    flip_h: bool,
    flip_v: bool,
) {
    let rad = rotation_deg.to_radians();
    let (s, c) = rad.sin_cos();
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    ops.push(Operation::new("cm", vec![1.into(), 0.into(), 0.into(), 1.into(), cx.into(), cy.into()]));
    ops.push(Operation::new("cm", vec![c.into(), s.into(), (-s).into(), c.into(), 0.into(), 0.into()]));
    if flip_h || flip_v {
        let sx: f32 = if flip_h { -1.0 } else { 1.0 };
        let sy: f32 = if flip_v { -1.0 } else { 1.0 };
        ops.push(Operation::new("cm", vec![sx.into(), 0.into(), 0.into(), sy.into(), 0.into(), 0.into()]));
    }
    ops.push(Operation::new("cm", vec![1.into(), 0.into(), 0.into(), 1.into(), (-w / 2.0).into(), (-h / 2.0).into()]));
}

fn add_image_xobject(document: &mut Document, png: &[u8]) -> Result<lopdf::ObjectId, Box<dyn std::error::Error>> {
    let img = image::load_from_memory(png)?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.to_rgba8();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    let mut has_alpha = false;
    for p in rgba.pixels() {
        rgb.extend_from_slice(&p.0[0..3]);
        alpha.push(p.0[3]);
        if p.0[3] < 255 {
            has_alpha = true;
        }
    }
    let smask = if has_alpha {
        let mut s = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            alpha,
        );
        s.compress()?;
        Some(document.add_object(s))
    } else {
        None
    };
    let mut dict = dictionary! {
        "Type" => "XObject", "Subtype" => "Image",
        "Width" => w as i64, "Height" => h as i64,
        "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
    };
    if let Some(id) = smask {
        dict.set("SMask", Object::Reference(id));
    }
    let mut stream = Stream::new(dict, rgb);
    stream.compress()?;
    Ok(document.add_object(stream))
}

fn ensure_page_resource(
    document: &mut Document,
    page_id: lopdf::ObjectId,
    category: &[u8],
    name: &str,
    value: Object,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(Object::Reference(res_id)) = inherited_value(document, page_id, b"Resources")? {
        let res = document.get_object_mut(res_id)?.as_dict_mut()?;
        ensure_dict(res, category)?.set(name, value);
        return Ok(());
    }
    let inherited = inherited_value(document, page_id, b"Resources")?;
    {
        let page = document.get_object_mut(page_id)?.as_dict_mut()?;
        if !page.has(b"Resources") {
            match inherited {
                Some(Object::Dictionary(d)) => page.set("Resources", Object::Dictionary(d)),
                _ => page.set("Resources", Object::Dictionary(lopdf::Dictionary::new())),
            }
        }
    }
    let page = document.get_object_mut(page_id)?.as_dict_mut()?;
    let res = page.get_mut(b"Resources")?.as_dict_mut()?;
    ensure_dict(res, category)?.set(name, value);
    Ok(())
}

fn ensure_dict<'a>(dict: &'a mut lopdf::Dictionary, key: &[u8]) -> Result<&'a mut lopdf::Dictionary, lopdf::Error> {
    if !dict.has(key) {
        dict.set(key, Object::Dictionary(lopdf::Dictionary::new()));
    }
    dict.get_mut(key)?.as_dict_mut()
}

fn append_content_stream(document: &mut Document, page_id: lopdf::ObjectId, stream_id: lopdf::ObjectId) -> Result<(), lopdf::Error> {
    let page = document.get_object_mut(page_id)?.as_dict_mut()?;
    match page.get_mut(b"Contents") {
        Ok(Object::Array(contents)) => contents.push(Object::Reference(stream_id)),
        Ok(existing) => {
            let old = existing.clone();
            *existing = Object::Array(vec![old, Object::Reference(stream_id)]);
        }
        Err(_) => page.set("Contents", Object::Reference(stream_id)),
    }
    Ok(())
}

fn encode_overlay_text_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .filter_map(|c| {
            let cp = c as u32;
            if cp <= 0xFF {
                Some(cp as u8)
            } else {
                None
            }
        })
        .collect()
}

const UNICODE_FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const UNICODE_FONT_NAME: &str = "DejaVuSans";

pub enum OverlayFontAdvice {
    Standard,
    Unicode { unsupported: Vec<char> },
}

pub fn overlay_font_advice(text: &str) -> OverlayFontAdvice {
    if !overlay_text_needs_unicode(text) {
        return OverlayFontAdvice::Standard;
    }
    let mut unsupported = Vec::new();
    if let Ok(face) = ttf_parser::Face::parse(UNICODE_FONT_BYTES, 0) {
        let mut seen = std::collections::HashSet::new();
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            if face.glyph_index(c).is_none() && seen.insert(c) {
                unsupported.push(c);
            }
        }
    }
    OverlayFontAdvice::Unicode { unsupported }
}

fn overlay_text_needs_unicode(text: &str) -> bool {
    text.chars().any(|c| (c as u32) > 0xFF)
}

fn encode_identity_h(face: &ttf_parser::Face, text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for c in text.chars() {
        if c.is_control() {
            continue;
        }
        if let Some(g) = face.glyph_index(c) {
            out.push((g.0 >> 8) as u8);
            out.push((g.0 & 0xFF) as u8);
        }
    }
    out
}

fn build_unicode_font(
    document: &mut Document,
    face: &ttf_parser::Face,
    used_gids: &std::collections::BTreeSet<u16>,
) -> Result<lopdf::ObjectId, Box<dyn std::error::Error>> {
    let upm = face.units_per_em().max(1) as f64;
    let scale = |v: i32| -> i64 { (v as f64 * 1000.0 / upm).round() as i64 };

    let bbox = face.global_bounding_box();
    let font_bbox = vec![
        Object::Integer(scale(bbox.x_min as i32)),
        Object::Integer(scale(bbox.y_min as i32)),
        Object::Integer(scale(bbox.x_max as i32)),
        Object::Integer(scale(bbox.y_max as i32)),
    ];
    let ascent = scale(face.ascender() as i32);
    let descent = scale(face.descender() as i32);
    let cap_height = face.capital_height().map(|h| scale(h as i32)).unwrap_or(ascent);

    let font_file = Stream::new(
        dictionary! { "Length1" => Object::Integer(UNICODE_FONT_BYTES.len() as i64) },
        UNICODE_FONT_BYTES.to_vec(),
    );
    let font_file_id = document.add_object(font_file);

    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => UNICODE_FONT_NAME,
        "Flags" => Object::Integer(32),
        "FontBBox" => Object::Array(font_bbox),
        "ItalicAngle" => Object::Integer(0),
        "Ascent" => Object::Integer(ascent),
        "Descent" => Object::Integer(descent),
        "CapHeight" => Object::Integer(cap_height),
        "StemV" => Object::Integer(80),
        "FontFile2" => Object::Reference(font_file_id),
    });

    let mut widths = Vec::new();
    for &gid in used_gids {
        let advance = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0);
        widths.push(Object::Integer(gid as i64));
        widths.push(Object::Array(vec![Object::Integer(scale(advance as i32))]));
    }

    let cid_system_info = dictionary! {
        "Registry" => Object::string_literal("Adobe"),
        "Ordering" => Object::string_literal("Identity"),
        "Supplement" => Object::Integer(0),
    };
    let cid_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => UNICODE_FONT_NAME,
        "CIDSystemInfo" => cid_system_info,
        "FontDescriptor" => Object::Reference(descriptor_id),
        "CIDToGIDMap" => "Identity",
        "DW" => Object::Integer(1000),
        "W" => Object::Array(widths),
    });

    let type0_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => UNICODE_FONT_NAME,
        "Encoding" => "Identity-H",
        "DescendantFonts" => Object::Array(vec![Object::Reference(cid_font_id)]),
    });

    Ok(type0_id)
}

fn standard_font_base(family: Option<&str>) -> &'static str {
    match family {
        Some("Times-Roman") | Some("Times Roman") => "Times-Roman",
        Some("Courier") => "Courier",
        _ => "Helvetica",
    }
}
fn standard_font_resource(base: &str) -> &'static str {
    match base {
        "Times-Roman" => "PFTimesRoman",
        "Courier" => "PFCourier",
        _ => "PFHelvetica",
    }
}

fn resolve_box(document: &Document, page_id: lopdf::ObjectId, key: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let value = inherited_value(document, page_id, key).ok()??;
    let value = match value {
        Object::Reference(id) => document.get_object(id).ok()?.clone(),
        other => other,
    };
    let arr = value.as_array().ok()?;
    if arr.len() != 4 {
        return None;
    }
    let a = to_f32(&arr[0]).ok()?;
    let b = to_f32(&arr[1]).ok()?;
    let c = to_f32(&arr[2]).ok()?;
    let d = to_f32(&arr[3]).ok()?;
    Some((a.min(c), b.min(d), a.max(c), b.max(d)))
}

fn enumerate_embedded_fonts(document: &Document) -> Vec<EmbeddedFontInfo> {
    use std::collections::BTreeMap;
    let mut seen: BTreeMap<(u32, u16), EmbeddedFontInfo> = BTreeMap::new();
    for (_page_number, page_id) in document.get_pages() {
        let resources = match inherited_value(document, page_id, b"Resources") {
            Ok(Some(Object::Reference(id))) => document.get_object(id).ok().and_then(|o| o.as_dict().ok()).cloned(),
            Ok(Some(Object::Dictionary(d))) => Some(d),
            _ => None,
        };
        let Some(resources) = resources else { continue };
        let font_dict = match resources.get(b"Font") {
            Ok(Object::Reference(id)) => document.get_object(*id).ok().and_then(|o| o.as_dict().ok()).cloned(),
            Ok(Object::Dictionary(d)) => Some(d.clone()),
            _ => None,
        };
        let Some(font_dict) = font_dict else { continue };
        for (_name, value) in font_dict.iter() {
            let Object::Reference(font_id) = value else { continue };
            let font_id = *font_id;
            if seen.contains_key(&font_id) {
                continue;
            }
            let Some(dict) = document.get_object(font_id).ok().and_then(|o| o.as_dict().ok()) else {
                continue;
            };
            let subtype = dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_default();
            let base_font = dict
                .get(b"BaseFont")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_else(|| "(unnamed)".to_string());
            let is_subset = is_subset_font_name(&base_font);
            let is_simple = matches!(subtype.as_str(), "Type1" | "TrueType" | "MMType1");
            let program = extract_sfnt_program(document, dict);
            seen.insert(
                font_id,
                EmbeddedFontInfo {
                    id: font_id,
                    base_font,
                    subtype,
                    is_subset,
                    is_simple,
                    program,
                },
            );
        }
    }
    dedupe_embedded_fonts(seen.into_values().collect())
}

fn is_subset_font_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 7
        && bytes[6] == b'+'
        && bytes[..6].iter().all(|c| c.is_ascii_uppercase())
}

fn strip_subset_prefix(name: &str) -> &str {
    if is_subset_font_name(name) {
        &name[7..]
    } else {
        name
    }
}

fn dedupe_embedded_fonts(fonts: Vec<EmbeddedFontInfo>) -> Vec<EmbeddedFontInfo> {
    use std::collections::BTreeMap;
    let mut best: BTreeMap<(String, String), EmbeddedFontInfo> = BTreeMap::new();
    for mut f in fonts {
        let face = strip_subset_prefix(&f.base_font).to_string();
        let key = (face.clone(), f.subtype.clone());
        f.base_font = face;
        match best.get(&key) {
            Some(existing) => {
                let existing_len = existing.program.as_ref().map_or(0, |p| p.len());
                let cand_len = f.program.as_ref().map_or(0, |p| p.len());
                let replace = match (existing.program.is_some(), f.program.is_some()) {
                    (false, true) => true,
                    (true, false) => false,
                    _ => cand_len > existing_len,
                };
                if replace {
                    best.insert(key, f);
                }
            }
            None => {
                best.insert(key, f);
            }
        }
    }
    let mut out: Vec<EmbeddedFontInfo> = best.into_values().collect();
    out.sort_by(|a, b| a.base_font.cmp(&b.base_font));
    out
}

fn extract_sfnt_program(
    document: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<std::sync::Arc<Vec<u8>>> {
    let descriptor = font_descriptor_for(document, font_dict)?;
    for key in [b"FontFile2".as_slice(), b"FontFile3".as_slice()] {
        let Ok(Object::Reference(id)) = descriptor.get(key) else { continue };
        let Ok(obj) = document.get_object(*id) else { continue };
        let Ok(stream) = obj.as_stream() else { continue };
        let Ok(bytes) = stream.decompressed_content() else { continue };
        if has_sfnt_magic(&bytes) {
            return Some(std::sync::Arc::new(bytes));
        }
    }
    None
}

fn font_descriptor_for(
    document: &Document,
    font_dict: &lopdf::Dictionary,
) -> Option<lopdf::Dictionary> {
    if let Ok(desc) = font_dict.get(b"DescendantFonts") {
        let cid_dict = match desc {
            Object::Reference(id) => document.get_object(*id).ok()?.clone(),
            other => other.clone(),
        };
        let cid_font = match cid_dict {
            Object::Array(arr) => match arr.into_iter().next()? {
                Object::Reference(id) => document.get_object(id).ok()?.as_dict().ok()?.clone(),
                Object::Dictionary(d) => d,
                _ => return None,
            },
            Object::Dictionary(d) => d,
            _ => return None,
        };
        return resolve_descriptor(document, &cid_font);
    }
    resolve_descriptor(document, font_dict)
}

fn resolve_descriptor(
    document: &Document,
    dict: &lopdf::Dictionary,
) -> Option<lopdf::Dictionary> {
    match dict.get(b"FontDescriptor") {
        Ok(Object::Reference(id)) => document.get_object(*id).ok()?.as_dict().ok().cloned(),
        Ok(Object::Dictionary(d)) => Some(d.clone()),
        _ => None,
    }
}

fn has_sfnt_magic(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(0..4),
        Some([0x00, 0x01, 0x00, 0x00])
            | Some(b"true")
            | Some(b"OTTO")
            | Some(b"ttcf")
    )
}

fn inherited_value(document: &Document, mut id: lopdf::ObjectId, key: &[u8]) -> Result<Option<Object>, lopdf::Error> {
    loop {
        let dict = document.get_object(id)?.as_dict()?;
        if let Ok(v) = dict.get(key) {
            return Ok(Some(v.clone()));
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return Ok(None),
        }
    }
}

fn inject_alpha_extgstate(
    doc: &mut Document,
    page_id: lopdf::ObjectId,
    fill_alpha: Option<u8>,
    stroke_alpha: Option<u8>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let fill_needs = fill_alpha.map_or(false, |a| a < 255);
    let stroke_needs = stroke_alpha.map_or(false, |a| a < 255);
    if !fill_needs && !stroke_needs {
        return Ok(None);
    }

    let mut gs_dict = lopdf::Dictionary::new();
    gs_dict.set("Type", "ExtGState");
    if let Some(a) = fill_alpha {
        gs_dict.set("ca", Object::Real(a as f32 / 255.0));
    }
    if let Some(a) = stroke_alpha {
        gs_dict.set("CA", Object::Real(a as f32 / 255.0));
    }
    let gs_id = doc.add_object(gs_dict);

    let page_dict = doc.get_object(page_id)?.as_dict()?.clone();
    let mut resources_dict: lopdf::Dictionary = match page_dict.get(b"Resources") {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(id)) => doc.get_object(*id)?.as_dict()?.clone(),
        _ => match inherited_value(doc, page_id, b"Resources")? {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Reference(id)) => doc.get_object(id)?.as_dict()?.clone(),
            _ => lopdf::Dictionary::new(),
        },
    };

    let mut extgstate_dict: lopdf::Dictionary = match resources_dict.get(b"ExtGState") {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(id)) => doc.get_object(*id)?.as_dict()?.clone(),
        _ => lopdf::Dictionary::new(),
    };

    let mut n: usize = 0;
    let name = loop {
        let candidate = format!("PFa{n}");
        if extgstate_dict.get(candidate.as_bytes()).is_err() {
            break candidate;
        }
        n += 1;
        if n > 10_000 {
            return Err("ExtGState name allocation overflow".into());
        }
    };
    extgstate_dict.set(name.as_bytes(), Object::Reference(gs_id));
    resources_dict.set("ExtGState", Object::Dictionary(extgstate_dict));

    let page_obj_mut = doc.objects.get_mut(&page_id).ok_or("page id missing in doc.objects")?;
    if let Object::Dictionary(page_dict_mut) = page_obj_mut {
        page_dict_mut.set("Resources", Object::Dictionary(resources_dict));
    } else {
        return Err("page object is not a dictionary".into());
    }
    Ok(Some(name))
}

fn to_f32(o: &Object) -> Result<f32, lopdf::Error> {
    match o {
        Object::Integer(v) => Ok(*v as f32),
        Object::Real(v) => Ok(*v),
        _ => Err(lopdf::Error::Type),
    }
}

fn push_rgb(ops: &mut Vec<Operation>, c: [u8; 3], op: &str) {
    let r = c[0] as f32 / 255.0;
    let g = c[1] as f32 / 255.0;
    let b = c[2] as f32 / 255.0;
    ops.push(Operation::new(op, vec![r.into(), g.into(), b.into()]));
}


#[derive(Clone, Copy, Debug, PartialEq)]
struct Mat6 {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Default for Mat6 {
    fn default() -> Self {
        Mat6::IDENTITY
    }
}

impl Mat6 {
    const IDENTITY: Mat6 = Mat6 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    fn translation(tx: f32, ty: f32) -> Self {
        Mat6 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }

    fn scaling(sx: f32, sy: f32) -> Self {
        Mat6 { a: sx, b: 0.0, c: 0.0, d: sy, e: 0.0, f: 0.0 }
    }

    fn rotation_cw(deg: f32) -> Self {
        let r = deg.to_radians();
        let (s, c) = r.sin_cos();
        Mat6 { a: c, b: -s, c: s, d: c, e: 0.0, f: 0.0 }
    }

    fn mul(self, rhs: Mat6) -> Mat6 {
        Mat6 {
            a: self.a * rhs.a + self.b * rhs.c,
            b: self.a * rhs.b + self.b * rhs.d,
            c: self.c * rhs.a + self.d * rhs.c,
            d: self.c * rhs.b + self.d * rhs.d,
            e: self.e * rhs.a + self.f * rhs.c + rhs.e,
            f: self.e * rhs.b + self.f * rhs.d + rhs.f,
        }
    }

    fn invert(self) -> Option<Mat6> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-9 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Mat6 {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }
}

fn mat_from_cm(operands: &[Object]) -> Option<Mat6> {
    if operands.len() != 6 {
        return None;
    }
    let v: Vec<f32> = operands.iter().filter_map(object_as_f32).collect();
    if v.len() != 6 {
        return None;
    }
    Some(Mat6 { a: v[0], b: v[1], c: v[2], d: v[3], e: v[4], f: v[5] })
}

fn object_as_f32(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(v) => Some(*v as f32),
        Object::Real(v) => Some(*v),
        _ => None,
    }
}

fn cm_op(m: Mat6) -> Operation {
    Operation::new(
        "cm",
        vec![
            Object::Real(m.a),
            Object::Real(m.b),
            Object::Real(m.c),
            Object::Real(m.d),
            Object::Real(m.e),
            Object::Real(m.f),
        ],
    )
}

#[derive(Clone, Copy, Debug)]
struct ContentObject {
    op_start: usize,
    op_end: usize,
    #[allow(dead_code)]
    kind: ObjectKind,
    ctm: Mat6,
}

fn top_level_pos_after(ops: &[Operation], op_end: usize) -> usize {
    let end = op_end.min(ops.len());
    let mut depth: i32 = 0;
    for op in &ops[..end] {
        match op.operator.as_str() {
            "q" => depth += 1,
            "Q" => depth -= 1,
            _ => {}
        }
    }
    if depth <= 0 {
        return end;
    }
    let mut j = end;
    while j < ops.len() {
        match ops[j].operator.as_str() {
            "q" => depth += 1,
            "Q" => depth -= 1,
            _ => {}
        }
        j += 1;
        if depth <= 0 {
            break;
        }
    }
    j
}

fn enumerate_content_objects(ops: &[Operation]) -> Vec<ContentObject> {
    let mut out = Vec::new();
    let mut ctm = Mat6::IDENTITY;
    let mut ctm_stack: Vec<Mat6> = Vec::new();
    let mut path_start: Option<usize> = None;
    let mut in_text = false;
    let mut inline_image_start: Option<usize> = None;
    for (i, op) in ops.iter().enumerate() {
        if inline_image_start.is_some() {
            if op.operator == "EI" {
                let start = inline_image_start.take().unwrap();
                out.push(ContentObject {
                    op_start: start,
                    op_end: i + 1,
                    kind: ObjectKind::Image,
                    ctm,
                });
            }
            continue;
        }
        match op.operator.as_str() {
            "q" => ctm_stack.push(ctm),
            "Q" => {
                if let Some(c) = ctm_stack.pop() {
                    ctm = c;
                }
            }
            "cm" => {
                if let Some(m) = mat_from_cm(&op.operands) {
                    ctm = m.mul(ctm);
                }
            }
            "BT" => in_text = true,
            "ET" => in_text = false,
            "Tj" | "TJ" | "'" | "\"" if in_text => {
                out.push(ContentObject {
                    op_start: i,
                    op_end: i + 1,
                    kind: ObjectKind::Text,
                    ctm,
                });
            }
            "m" | "l" | "c" | "v" | "y" | "re" | "h" if !in_text => {
                if path_start.is_none() {
                    path_start = Some(i);
                }
            }
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" if !in_text => {
                if let Some(start) = path_start.take() {
                    out.push(ContentObject {
                        op_start: start,
                        op_end: i + 1,
                        kind: ObjectKind::Path,
                        ctm,
                    });
                }
            }
            "n" if !in_text => {
                path_start = None;
            }
            "Do" => out.push(ContentObject {
                op_start: i,
                op_end: i + 1,
                kind: ObjectKind::Image,
                ctm,
            }),
            "sh" => out.push(ContentObject {
                op_start: i,
                op_end: i + 1,
                kind: ObjectKind::Shading,
                ctm,
            }),
            "BI" => inline_image_start = Some(i),
            _ => {}
        }
    }
    out
}

fn is_surgical_eligible(_edit: &ObjectEdit) -> bool {
    true
}

#[derive(Default)]
struct EditPlan {
    surgical: HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>>,
    pdfium: HashMap<usize, Vec<ObjectEdit>>,
}

fn plan_edits(doc: &PdfDocument<'_>, by_page: HashMap<usize, Vec<ObjectEdit>>) -> EditPlan {
    let mut plan = EditPlan::default();
    for (page_index, edits) in by_page {
        let page = match doc.pages().get(page_index as u16) {
            Ok(p) => p,
            Err(_) => {
                plan.pdfium.insert(page_index, edits);
                continue;
            }
        };
        let has_delete = edits.iter().any(|e| e.delete);
        let has_alpha = edits.iter().any(|e| {
            (e.fill_color.is_some() && e.fill_alpha.map_or(false, |a| a < 255))
                || (e.stroke_color.is_some() && e.stroke_alpha.map_or(false, |a| a < 255))
        });
        let has_spacing = edits
            .iter()
            .any(|e| e.char_spacing.is_some() || e.word_spacing.is_some());
        let has_dupes = edits.iter().any(|e| e.copy_seq.is_some());
        let fragile = has_delete || has_alpha || has_spacing || has_dupes || !page_is_simple(&page);
        let all_surgical = edits.iter().all(is_surgical_eligible);
        if fragile && all_surgical {
            let with_centres: Vec<(ObjectEdit, Option<(f32, f32)>)> = edits
                .into_iter()
                .map(|e| {
                    let centre = page
                        .objects()
                        .get(e.object_index)
                        .ok()
                        .and_then(|o| o.bounds().ok())
                        .map(|q| {
                            let r = q.to_rect();
                            (
                                r.left().value + r.width().value / 2.0,
                                r.bottom().value + r.height().value / 2.0,
                            )
                        });
                    (e, centre)
                })
                .collect();
            plan.surgical.insert(page_index, with_centres);
        } else {
            plan.pdfium.insert(page_index, edits);
        }
    }
    plan
}

#[allow(dead_code)]
fn apply_surgical_edits_to_doc(
    doc: &mut lopdf::Document,
    surgical: &HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>>,
) -> Result<(), Box<dyn std::error::Error>> {
    apply_surgical_edits_to_doc_with_encoded(doc, surgical, &HashMap::new())
}

fn encode_text_edits_via_pdfium(
    pdfium: &Pdfium,
    source: &Path,
    surgical: &HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>>,
) -> HashMap<(usize, usize, Option<u32>), (Vec<u8>, String)> {
    let mut out: HashMap<(usize, usize, Option<u32>), (Vec<u8>, String)> = HashMap::new();
    let mut text_edits_by_page: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
    for (page_idx, edits) in surgical {
        for (e, _) in edits {
            if e.copy_seq.is_none() {
                if let Some(t) = &e.text {
                    text_edits_by_page
                        .entry(*page_idx)
                        .or_default()
                        .push((e.object_index, t.clone()));
                }
            }
        }
    }
    for (page_idx, edits) in text_edits_by_page {
        if let Some(map) = pdfium_encode_text_for_page(pdfium, source, page_idx, &edits) {
            for ((obj_idx, _), entry) in edits.into_iter().zip(map.into_iter()) {
                if let Some(e) = entry {
                    out.insert((page_idx, obj_idx, None), e);
                }
            }
        }
    }
    for (page_idx, edits) in surgical {
        for (e, _) in edits {
            if let (Some(seq), Some(t)) = (e.copy_seq, &e.text) {
                if let Some(map) =
                    pdfium_encode_text_for_page(pdfium, source, *page_idx, &[(e.object_index, t.clone())])
                {
                    if let Some(Some(entry)) = map.into_iter().next() {
                        out.insert((*page_idx, e.object_index, Some(seq)), entry);
                    }
                }
            }
        }
    }
    out
}

fn pdfium_encode_text_for_page(
    pdfium: &Pdfium,
    source: &Path,
    page_index: usize,
    edits: &[(usize, String)],
) -> Option<Vec<Option<(Vec<u8>, String)>>> {
    let src_doc = pdfium.load_pdf_from_file(source, None).ok()?;
    let src_page = src_doc.pages().get(page_index as u16).ok()?;
    let mut text_positions: Vec<Option<usize>> = Vec::with_capacity(edits.len());
    for (object_index, _) in edits {
        let mut text_count_before: usize = 0;
        let mut is_text = false;
        for (i, o) in src_page.objects().iter().enumerate() {
            if i > *object_index {
                break;
            }
            let is_t = o.as_text_object().is_some();
            if i == *object_index {
                is_text = is_t;
                break;
            }
            if is_t {
                text_count_before += 1;
            }
        }
        if is_text {
            text_positions.push(Some(text_count_before));
        } else {
            text_positions.push(None);
        }
    }
    drop(src_page);
    drop(src_doc);

    let mut doc = pdfium.load_pdf_from_file(source, None).ok()?;
    let mut page = doc.pages_mut().get(page_index as u16).ok()?;
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    let mut orig_texts: Vec<String> = Vec::with_capacity(edits.len());
    for (object_index, new_text) in edits {
        let mut orig = String::new();
        if let Ok(mut obj) = page.objects_mut().get(*object_index) {
            if let Some(t) = obj.as_text_object_mut() {
                orig = t.text();
                let _ = t.set_text(new_text.as_str());
            }
        }
        orig_texts.push(orig);
    }
    page.regenerate_content().ok()?;
    drop(page);
    let pdf_bytes = doc.save_to_bytes().ok()?;
    drop(doc);

    let mod_doc = lopdf::Document::load_from(&pdf_bytes[..]).ok()?;
    let mod_page_id = *mod_doc.get_pages().get(&(page_index as u32 + 1))?;
    let mod_content = mod_doc.get_page_content(mod_page_id).ok()?;
    let mod_parsed = lopdf::content::Content::decode(&mod_content).ok()?;
    let mod_objects = enumerate_content_objects(&mod_parsed.operations);
    let mod_text_ops: Vec<&Operation> = mod_objects
        .iter()
        .filter(|o| o.kind == ObjectKind::Text)
        .filter_map(|o| mod_parsed.operations.get(o.op_start))
        .collect();

    let mut out: Vec<Option<(Vec<u8>, String)>> = Vec::with_capacity(edits.len());
    for (pos, orig) in text_positions.into_iter().zip(orig_texts.into_iter()) {
        out.push(
            pos.and_then(|p| mod_text_ops.get(p).and_then(|op| extract_text_bytes(op)))
                .map(|b| (b, orig)),
        );
    }
    Some(out)
}

fn extract_text_bytes(op: &Operation) -> Option<Vec<u8>> {
    match op.operator.as_str() {
        "Tj" | "'" => {
            if let Some(Object::String(b, _)) = op.operands.first() {
                return Some(b.clone());
            }
        }
        "\"" => {
            if let Some(Object::String(b, _)) = op.operands.get(2) {
                return Some(b.clone());
            }
        }
        "TJ" => {
            if let Some(Object::Array(arr)) = op.operands.first() {
                let mut buf = Vec::new();
                for entry in arr {
                    if let Object::String(b, _) = entry {
                        buf.extend_from_slice(b);
                    }
                }
                if !buf.is_empty() {
                    return Some(buf);
                }
            }
        }
        _ => {}
    }
    None
}

fn apply_surgical_edits_to_doc_with_encoded(
    doc: &mut lopdf::Document,
    surgical: &HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>>,
    encoded_text: &HashMap<(usize, usize, Option<u32>), (Vec<u8>, String)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pages = doc.get_pages();
    for (page_index, edits) in surgical {
        let Some(page_id) = pages.get(&(*page_index as u32 + 1)).copied() else { continue };
        let content_bytes = doc.get_page_content(page_id)?;
        let mut content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (dup_edits, normal_edits): (
            Vec<(ObjectEdit, Option<(f32, f32)>)>,
            Vec<(ObjectEdit, Option<(f32, f32)>)>,
        ) = edits.iter().cloned().partition(|(e, _)| e.copy_seq.is_some());
        let mut dup_gs: HashMap<(usize, u32), String> = HashMap::new();
        for (dup, _) in &dup_edits {
            let Some(seq) = dup.copy_seq else { continue };
            let needs_fill_a = dup.fill_alpha.map_or(false, |a| a < 255) && dup.fill_color.is_some();
            let needs_stroke_a = dup.stroke_alpha.map_or(false, |a| a < 255) && dup.stroke_color.is_some();
            if !needs_fill_a && !needs_stroke_a {
                continue;
            }
            let fill_a = if needs_fill_a { dup.fill_alpha } else { None };
            let stroke_a = if needs_stroke_a { dup.stroke_alpha } else { None };
            if let Ok(Some(name)) = inject_alpha_extgstate(doc, page_id, fill_a, stroke_a) {
                dup_gs.insert((dup.object_index, seq), name);
            }
        }
        let dup_blocks: Vec<(DupZOrder, usize, Vec<Operation>)> = if dup_edits.is_empty() {
            Vec::new()
        } else {
            let (dup_objs, dup_snaps) = walk_with_snapshots(&content.operations);
            let pristine_ops = content.operations.clone();
            let mut blocks: Vec<(DupZOrder, usize, Vec<Operation>)> = Vec::new();
            for (dup, centre) in &dup_edits {
                let (Some(obj), Some(snap)) =
                    (dup_objs.get(dup.object_index), dup_snaps.get(dup.object_index))
                else {
                    continue;
                };
                let enc = dup
                    .copy_seq
                    .and_then(|seq| encoded_text.get(&(*page_index, dup.object_index, Some(seq))));
                let gs = dup
                    .copy_seq
                    .and_then(|seq| dup_gs.get(&(dup.object_index, seq)))
                    .map(|s| s.as_str());
                let mut out = Vec::new();
                emit_normalized_object_with_encoded(
                    &mut out,
                    obj,
                    snap,
                    &pristine_ops,
                    Some((dup, *centre)),
                    enc.map(|(b, t)| (b.as_slice(), t.as_str())),
                    gs,
                );
                if obj.kind == ObjectKind::Image {
                    if let Some(data) = &dup.image_data {
                        if let Ok(img_id) = add_image_xobject(doc, data) {
                            let name = format!(
                                "PFDupImg{}_{}",
                                dup.object_index,
                                dup.copy_seq.unwrap_or(0)
                            );
                            if ensure_page_resource(
                                doc,
                                page_id,
                                b"XObject",
                                &name,
                                Object::Reference(img_id),
                            )
                            .is_ok()
                            {
                                for op in out.iter_mut() {
                                    if op.operator == "Do" {
                                        op.operands =
                                            vec![Object::Name(name.as_bytes().to_vec())];
                                    }
                                }
                            }
                        }
                    }
                }
                blocks.push((dup.dup_z, dup.object_index, out));
            }
            blocks
        };

        let has_arrange = normal_edits.iter().any(|(e, _)| e.arrange.is_some() && !e.delete);
        let has_transform_edit = normal_edits.iter().any(|(e, _)| has_geometric_transform(e));
        let use_phase3 = has_arrange || has_transform_edit;
        let objects = enumerate_content_objects(&content.operations);
        let mut changed = false;

        let mut gs_names: HashMap<usize, String> = HashMap::new();
        for (edit, _) in normal_edits.iter() {
            let needs_fill_a = edit.fill_alpha.map_or(false, |a| a < 255) && edit.fill_color.is_some();
            let needs_stroke_a = edit.stroke_alpha.map_or(false, |a| a < 255) && edit.stroke_color.is_some();
            if !needs_fill_a && !needs_stroke_a {
                continue;
            }
            let fill_a = if needs_fill_a { edit.fill_alpha } else { None };
            let stroke_a = if needs_stroke_a { edit.stroke_alpha } else { None };
            if let Ok(Some(name)) = inject_alpha_extgstate(doc, page_id, fill_a, stroke_a) {
                gs_names.insert(edit.object_index, name);
            }
        }

        if !use_phase3 {
            let mut sorted: Vec<&(ObjectEdit, Option<(f32, f32)>)> = normal_edits.iter().collect();
            sorted.sort_by(|a, b| b.0.object_index.cmp(&a.0.object_index));
            for (edit, centre) in sorted {
                if edit.object_index >= objects.len() {
                    continue;
                }
                let obj = objects[edit.object_index];
                let encoded = encoded_text.get(&(*page_index, edit.object_index, None));
                let gs = gs_names.get(&edit.object_index).map(|s| s.as_str());
                splice_surgical_edit_with_encoded(
                    &mut content.operations,
                    &obj,
                    edit,
                    *centre,
                    encoded.map(|(b, t)| (b.as_slice(), t.as_str())),
                    gs,
                );
                changed = true;
            }
        }
        for (edit, _) in &normal_edits {
            if let Some(data) = &edit.image_data {
                if edit.object_index < objects.len() {
                    let obj = objects[edit.object_index];
                    if obj.kind == ObjectKind::Image {
                        if replace_image_xobject(doc, page_id, &content.operations, obj.op_start, data).is_ok() {
                        }
                    }
                }
            }
        }
        if use_phase3 {
            let page_encoded: HashMap<usize, (Vec<u8>, String)> = encoded_text
                .iter()
                .filter(|((p, _, seq), _)| p == page_index && seq.is_none())
                .map(|((_, obj_idx, _), entry)| (*obj_idx, entry.clone()))
                .collect();
            content.operations =
                rebuild_normalized_with_encoded(&content.operations, &normal_edits, &page_encoded, &gs_names);
            changed = true;
        }
        if !dup_blocks.is_empty() {
            changed = true;
            let final_objs = if use_phase3 {
                Vec::new()
            } else {
                enumerate_content_objects(&content.operations)
            };
            let mut above: Vec<(usize, &Vec<Operation>)> = Vec::new();
            let mut behind: Vec<&Vec<Operation>> = Vec::new();
            let mut on_top: Vec<&Vec<Operation>> = Vec::new();
            for (z, src_oi, block) in &dup_blocks {
                match z {
                    DupZOrder::Behind => behind.push(block),
                    DupZOrder::AboveSource => match final_objs.get(*src_oi) {
                        Some(o) => above.push((top_level_pos_after(&content.operations, o.op_end), block)),
                        None => on_top.push(block),
                    },
                    DupZOrder::OnTop => on_top.push(block),
                }
            }
            above.sort_by(|a, b| b.0.cmp(&a.0));
            for (pos, block) in above {
                let pos = pos.min(content.operations.len());
                content.operations.splice(pos..pos, block.iter().cloned());
            }
            if !behind.is_empty() {
                let mut prefix: Vec<Operation> = Vec::new();
                for block in behind {
                    prefix.extend(block.iter().cloned());
                }
                content.operations.splice(0..0, prefix);
            }
            for block in on_top {
                content.operations.extend(block.iter().cloned());
            }
        }
        if !changed {
            continue;
        }
        let new_bytes = content.encode()?;
        replace_page_content(doc, page_id, new_bytes)?;
    }
    Ok(())
}

fn replace_image_xobject(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    ops: &[Operation],
    do_op_index: usize,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let op = ops.get(do_op_index).ok_or("Do op out of range")?;
    if op.operator != "Do" {
        return Err("expected Do operator".into());
    }
    let name = match op.operands.first() {
        Some(Object::Name(n)) => n.clone(),
        _ => return Err("Do operator without name".into()),
    };
    let xobject_id = resolve_xobject_reference(doc, page_id, &name)?;
    let img = image::load_from_memory(data)?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.to_rgba8();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    let mut has_alpha = false;
    for p in rgba.pixels() {
        rgb.extend_from_slice(&p.0[0..3]);
        alpha.push(p.0[3]);
        if p.0[3] < 255 {
            has_alpha = true;
        }
    }
    let smask = if has_alpha {
        let mut s = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            alpha,
        );
        s.compress()?;
        Some(doc.add_object(s))
    } else {
        None
    };
    let mut dict = dictionary! {
        "Type" => "XObject", "Subtype" => "Image",
        "Width" => w as i64, "Height" => h as i64,
        "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
    };
    if let Some(id) = smask {
        dict.set("SMask", Object::Reference(id));
    }
    let mut stream = Stream::new(dict, rgb);
    stream.compress()?;
    *doc.objects.get_mut(&xobject_id).ok_or("XObject id missing")? = Object::Stream(stream);
    Ok(())
}

fn resolve_xobject_reference(
    doc: &lopdf::Document,
    page_id: lopdf::ObjectId,
    name: &[u8],
) -> Result<lopdf::ObjectId, Box<dyn std::error::Error>> {
    let resources: lopdf::Dictionary = match inherited_value(doc, page_id, b"Resources")? {
        Some(Object::Reference(id)) => doc.get_object(id)?.as_dict()?.clone(),
        Some(Object::Dictionary(d)) => d,
        _ => return Err("page has no Resources".into()),
    };
    let xobjects: lopdf::Dictionary = match resources.get(b"XObject") {
        Ok(Object::Reference(id)) => doc.get_object(*id)?.as_dict()?.clone(),
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => return Err("no /XObject in Resources".into()),
    };
    let entry = xobjects.get(name)?;
    if let Object::Reference(id) = entry {
        Ok(*id)
    } else {
        Err("XObject entry is not a reference".into())
    }
}

#[allow(dead_code)]
fn rebuild_normalized(
    ops: &[Operation],
    edits: &[(ObjectEdit, Option<(f32, f32)>)],
) -> Vec<Operation> {
    rebuild_normalized_with_encoded(ops, edits, &HashMap::new(), &HashMap::new())
}

fn transform_point(x: f32, y: f32, m: &Mat6) -> (f32, f32) {
    (x * m.a + y * m.c + m.e, x * m.b + y * m.d + m.f)
}


fn transform_path_ops(ops: &[Operation], m: &Mat6) -> Vec<Operation> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match op.operator.as_str() {
            "m" | "l" => {
                if op.operands.len() == 2 {
                    let x = to_f32(&op.operands[0]).unwrap_or(0.0);
                    let y = to_f32(&op.operands[1]).unwrap_or(0.0);
                    let (px, py) = transform_point(x, y, m);
                    out.push(Operation::new(
                        &op.operator,
                        vec![Object::Real(px), Object::Real(py)],
                    ));
                } else {
                    out.push(op.clone());
                }
            }
            "c" => {
                if op.operands.len() == 6 {
                    let pts: Vec<f32> = (0..6).map(|i| to_f32(&op.operands[i]).unwrap_or(0.0)).collect();
                    let (x1, y1) = transform_point(pts[0], pts[1], m);
                    let (x2, y2) = transform_point(pts[2], pts[3], m);
                    let (x3, y3) = transform_point(pts[4], pts[5], m);
                    out.push(Operation::new(
                        "c",
                        vec![
                            Object::Real(x1), Object::Real(y1),
                            Object::Real(x2), Object::Real(y2),
                            Object::Real(x3), Object::Real(y3),
                        ],
                    ));
                } else {
                    out.push(op.clone());
                }
            }
            "v" | "y" => {
                if op.operands.len() == 4 {
                    let pts: Vec<f32> = (0..4).map(|i| to_f32(&op.operands[i]).unwrap_or(0.0)).collect();
                    let (x1, y1) = transform_point(pts[0], pts[1], m);
                    let (x2, y2) = transform_point(pts[2], pts[3], m);
                    out.push(Operation::new(
                        &op.operator,
                        vec![
                            Object::Real(x1), Object::Real(y1),
                            Object::Real(x2), Object::Real(y2),
                        ],
                    ));
                } else {
                    out.push(op.clone());
                }
            }
            "re" => {
                if op.operands.len() == 4 {
                    let x = to_f32(&op.operands[0]).unwrap_or(0.0);
                    let y = to_f32(&op.operands[1]).unwrap_or(0.0);
                    let w = to_f32(&op.operands[2]).unwrap_or(0.0);
                    let h = to_f32(&op.operands[3]).unwrap_or(0.0);
                    let p0 = transform_point(x, y, m);
                    let p1 = transform_point(x + w, y, m);
                    let p2 = transform_point(x + w, y + h, m);
                    let p3 = transform_point(x, y + h, m);
                    out.push(Operation::new("m", vec![Object::Real(p0.0), Object::Real(p0.1)]));
                    out.push(Operation::new("l", vec![Object::Real(p1.0), Object::Real(p1.1)]));
                    out.push(Operation::new("l", vec![Object::Real(p2.0), Object::Real(p2.1)]));
                    out.push(Operation::new("l", vec![Object::Real(p3.0), Object::Real(p3.1)]));
                    out.push(Operation::new("h", vec![]));
                } else {
                    out.push(op.clone());
                }
            }
            "h" => out.push(op.clone()),
            _ => out.push(op.clone()),
        }
    }
    out
}

fn rebuild_normalized_with_encoded(
    ops: &[Operation],
    edits: &[(ObjectEdit, Option<(f32, f32)>)],
    encoded_text: &HashMap<usize, (Vec<u8>, String)>,
    gs_names: &HashMap<usize, String>,
) -> Vec<Operation> {
    let (objects, snapshots) = walk_with_snapshots(ops);
    let n = objects.len();
    if n == 0 {
        return ops.to_vec();
    }

    let edits_by_idx: HashMap<usize, (&ObjectEdit, Option<(f32, f32)>)> = edits
        .iter()
        .map(|(e, c)| (e.object_index, (e, *c)))
        .collect();

    let deleted: Vec<usize> = edits
        .iter()
        .filter(|(e, _)| e.delete)
        .map(|(e, _)| e.object_index)
        .collect();
    let current_order: Vec<usize> = (0..n).filter(|i| !deleted.contains(i)).collect();
    let mut target_order = current_order.clone();
    for (e, _) in edits {
        if let Some(action) = e.arrange {
            if !e.delete {
                target_order = arrange_object_index_order(&target_order, e.object_index, action);
            }
        }
    }

    let mut out: Vec<Operation> = Vec::new();

    for orig_idx in target_order {
        let obj = &objects[orig_idx];
        let snap = &snapshots[orig_idx];
        let edit_opt = edits_by_idx.get(&orig_idx).map(|t| (t.0, t.1));
        let enc = encoded_text.get(&orig_idx).map(|(b, t)| (b.as_slice(), t.as_str()));
        let gs = gs_names.get(&orig_idx).map(|s| s.as_str());
        emit_normalized_object_with_encoded(&mut out, obj, snap, ops, edit_opt, enc, gs);
    }

    out
}

#[derive(Clone, Debug, Default)]
struct StateSnapshot {
    fill: Vec<Operation>,
    stroke: Vec<Operation>,
    line_width: Option<Operation>,
    line_cap: Option<Operation>,
    line_join: Option<Operation>,
    miter: Option<Operation>,
    dash: Option<Operation>,
    ext_gstate: Option<Operation>,
    rendering_intent: Option<Operation>,
    flatness: Option<Operation>,
    font: Option<Operation>,
    char_spacing: Option<Operation>,
    word_spacing: Option<Operation>,
    leading: Option<Operation>,
    render_mode: Option<Operation>,
    h_scale: Option<Operation>,
    rise: Option<Operation>,
    text_matrix: Mat6,
    clip_stack: Vec<ClipEntry>,
}

#[derive(Clone, Debug)]
struct ClipEntry {
    path_ops: Vec<Operation>,
    kind: String,
    ctm: Mat6,
}

fn walk_with_snapshots(ops: &[Operation]) -> (Vec<ContentObject>, Vec<StateSnapshot>) {
    let mut objects = Vec::new();
    let mut snapshots = Vec::new();
    let mut ctm = Mat6::IDENTITY;
    let mut state = StateSnapshot::default();
    let mut ctm_stack: Vec<Mat6> = Vec::new();
    let mut state_stack: Vec<StateSnapshot> = Vec::new();
    let mut path_start: Option<usize> = None;
    let mut in_text = false;
    let mut text_matrix = Mat6::IDENTITY;
    let mut line_matrix = Mat6::IDENTITY;
    let mut inline_image_start: Option<usize> = None;
    let mut current_path_ops: Vec<Operation> = Vec::new();
    let mut current_path_ctm = Mat6::IDENTITY;
    let mut pending_clip: Option<String> = None;
    for (i, op) in ops.iter().enumerate() {
        if inline_image_start.is_some() {
            if op.operator == "EI" {
                let start = inline_image_start.take().unwrap();
                let mut snap = state.clone();
                snap.text_matrix = text_matrix;
                objects.push(ContentObject {
                    op_start: start,
                    op_end: i + 1,
                    kind: ObjectKind::Image,
                    ctm,
                });
                snapshots.push(snap);
            }
            continue;
        }
        match op.operator.as_str() {
            "q" => {
                ctm_stack.push(ctm);
                state_stack.push(state.clone());
            }
            "Q" => {
                if let Some(c) = ctm_stack.pop() {
                    ctm = c;
                }
                if let Some(s) = state_stack.pop() {
                    state = s;
                }
            }
            "cm" => {
                if let Some(m) = mat_from_cm(&op.operands) {
                    ctm = m.mul(ctm);
                }
            }
            "rg" | "g" | "k" => state.fill = vec![op.clone()],
            "sc" | "scn" => {
                if let Some(pos) = state.fill.iter().position(|o| o.operator == "cs") {
                    state.fill.truncate(pos + 1);
                    state.fill.push(op.clone());
                } else {
                    state.fill = vec![op.clone()];
                }
            }
            "cs" => state.fill = vec![op.clone()],
            "RG" | "G" | "K" => state.stroke = vec![op.clone()],
            "SC" | "SCN" => {
                if let Some(pos) = state.stroke.iter().position(|o| o.operator == "CS") {
                    state.stroke.truncate(pos + 1);
                    state.stroke.push(op.clone());
                } else {
                    state.stroke = vec![op.clone()];
                }
            }
            "CS" => state.stroke = vec![op.clone()],
            "w" => state.line_width = Some(op.clone()),
            "J" => state.line_cap = Some(op.clone()),
            "j" => state.line_join = Some(op.clone()),
            "M" => state.miter = Some(op.clone()),
            "d" => state.dash = Some(op.clone()),
            "gs" => state.ext_gstate = Some(op.clone()),
            "ri" => state.rendering_intent = Some(op.clone()),
            "i" => state.flatness = Some(op.clone()),
            "Tf" => state.font = Some(op.clone()),
            "Tc" => state.char_spacing = Some(op.clone()),
            "Tw" => state.word_spacing = Some(op.clone()),
            "TL" => state.leading = Some(op.clone()),
            "Tr" => state.render_mode = Some(op.clone()),
            "Tz" => state.h_scale = Some(op.clone()),
            "Ts" => state.rise = Some(op.clone()),
            "BT" => {
                in_text = true;
                text_matrix = Mat6::IDENTITY;
                line_matrix = Mat6::IDENTITY;
            }
            "ET" => in_text = false,
            "Tm" => {
                if let Some(m) = mat_from_cm(&op.operands) {
                    text_matrix = m;
                    line_matrix = m;
                }
            }
            "Td" | "TD" => {
                if op.operands.len() == 2 {
                    if let (Some(tx), Some(ty)) =
                        (object_as_f32(&op.operands[0]), object_as_f32(&op.operands[1]))
                    {
                        let shift = Mat6::translation(tx, ty);
                        line_matrix = shift.mul(line_matrix);
                        text_matrix = line_matrix;
                    }
                }
            }
            "T*" => {
                if let Some(lead) = state.leading.as_ref() {
                    if let Some(ty) = lead.operands.first().and_then(object_as_f32) {
                        let shift = Mat6::translation(0.0, -ty);
                        line_matrix = shift.mul(line_matrix);
                        text_matrix = line_matrix;
                    }
                }
            }
            "Tj" | "TJ" | "'" | "\"" if in_text => {
                let mut snap = state.clone();
                snap.text_matrix = text_matrix;
                objects.push(ContentObject {
                    op_start: i,
                    op_end: i + 1,
                    kind: ObjectKind::Text,
                    ctm,
                });
                snapshots.push(snap);
            }
            "m" | "l" | "c" | "v" | "y" | "re" | "h" if !in_text => {
                if path_start.is_none() {
                    path_start = Some(i);
                    current_path_ctm = ctm;
                    current_path_ops.clear();
                }
                current_path_ops.push(op.clone());
            }
            "W" | "W*" if !in_text => {
                pending_clip = Some(op.operator.clone());
            }
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" if !in_text => {
                if let Some(kind) = pending_clip.take() {
                    state.clip_stack.push(ClipEntry {
                        path_ops: current_path_ops.clone(),
                        kind,
                        ctm: current_path_ctm,
                    });
                }
                current_path_ops.clear();
                if let Some(start) = path_start.take() {
                    let mut snap = state.clone();
                    snap.text_matrix = text_matrix;
                    objects.push(ContentObject {
                        op_start: start,
                        op_end: i + 1,
                        kind: ObjectKind::Path,
                        ctm,
                    });
                    snapshots.push(snap);
                }
            }
            "n" if !in_text => {
                if let Some(kind) = pending_clip.take() {
                    state.clip_stack.push(ClipEntry {
                        path_ops: current_path_ops.clone(),
                        kind,
                        ctm: current_path_ctm,
                    });
                }
                current_path_ops.clear();
                path_start = None;
            }
            "Do" => {
                let mut snap = state.clone();
                snap.text_matrix = text_matrix;
                objects.push(ContentObject {
                    op_start: i,
                    op_end: i + 1,
                    kind: ObjectKind::Image,
                    ctm,
                });
                snapshots.push(snap);
            }
            "sh" => {
                let mut snap = state.clone();
                snap.text_matrix = text_matrix;
                objects.push(ContentObject {
                    op_start: i,
                    op_end: i + 1,
                    kind: ObjectKind::Shading,
                    ctm,
                });
                snapshots.push(snap);
            }
            "BI" => inline_image_start = Some(i),
            _ => {}
        }
    }
    (objects, snapshots)
}

#[allow(dead_code)]
fn emit_normalized_object(
    out: &mut Vec<Operation>,
    obj: &ContentObject,
    snap: &StateSnapshot,
    src_ops: &[Operation],
    edit: Option<(&ObjectEdit, Option<(f32, f32)>)>,
) {
    emit_normalized_object_with_encoded(out, obj, snap, src_ops, edit, None, None);
}

fn emit_normalized_object_with_encoded(
    out: &mut Vec<Operation>,
    obj: &ContentObject,
    snap: &StateSnapshot,
    src_ops: &[Operation],
    edit: Option<(&ObjectEdit, Option<(f32, f32)>)>,
    encoded_text: Option<(&[u8], &str)>,
    gs_name: Option<&str>,
) {
    let (edit, centre) = match edit {
        Some(x) => x,
        None => (
            &ObjectEdit {
                page_index: 0,
                object_index: obj.op_start,
                dx: 0.0,
                dy: 0.0,
                scale_x: 1.0,
                scale_y: 1.0,
                rotation: 0.0,
                flip_horizontal: false,
                flip_vertical: false,
                text: None,
                fill_color: None,
                fill_alpha: None,
                stroke_color: None,
                stroke_alpha: None,
                stroke_width: None,
                font_size: None,
                char_spacing: None,
                word_spacing: None,
                image_data: None,
                arrange: None,
                delete: false,
                copy_seq: None,
                dup_z: DupZOrder::OnTop,
            },
            None,
        ),
    };

    out.push(Operation::new("q", vec![]));
    let m_edit = compute_m_edit(edit, centre);
    for clip in &snap.clip_stack {
        let clip_ctm = clip.ctm.mul(m_edit);
        let transformed = transform_path_ops(&clip.path_ops, &clip_ctm);
        for op in transformed {
            out.push(op);
        }
        out.push(Operation::new(&clip.kind, vec![]));
        out.push(Operation::new("n", vec![]));
    }
    let effective_ctm = compose_with_edit(snap, obj, edit, centre);
    out.push(cm_op(effective_ctm));

    for op in &snap.fill {
        out.push(op.clone());
    }
    for op in &snap.stroke {
        out.push(op.clone());
    }
    if let Some(c) = edit.fill_color {
        out.push(Operation::new(
            "rg",
            vec![
                Object::Real(c[0] as f32 / 255.0),
                Object::Real(c[1] as f32 / 255.0),
                Object::Real(c[2] as f32 / 255.0),
            ],
        ));
    }
    if let Some(c) = edit.stroke_color {
        out.push(Operation::new(
            "RG",
            vec![
                Object::Real(c[0] as f32 / 255.0),
                Object::Real(c[1] as f32 / 255.0),
                Object::Real(c[2] as f32 / 255.0),
            ],
        ));
    }
    if let Some(w) = edit.stroke_width {
        out.push(Operation::new("w", vec![Object::Real(w.max(0.0))]));
    } else if let Some(op) = &snap.line_width {
        out.push(op.clone());
    }
    if let Some(op) = &snap.line_cap {
        out.push(op.clone());
    }
    if let Some(op) = &snap.line_join {
        out.push(op.clone());
    }
    if let Some(op) = &snap.miter {
        out.push(op.clone());
    }
    if let Some(op) = &snap.dash {
        out.push(op.clone());
    }
    if let Some(op) = &snap.ext_gstate {
        out.push(op.clone());
    }
    if let Some(name) = gs_name {
        out.push(Operation::new(
            "gs",
            vec![Object::Name(name.as_bytes().to_vec())],
        ));
    }
    if let Some(op) = &snap.rendering_intent {
        out.push(op.clone());
    }
    if let Some(op) = &snap.flatness {
        out.push(op.clone());
    }

    if obj.kind == ObjectKind::Text {
        out.push(Operation::new("BT", vec![]));
        if let Some(op) = &snap.font {
            if let (Some(Object::Name(name)), Some(new_size)) =
                (op.operands.first(), edit.font_size)
            {
                out.push(Operation::new(
                    "Tf",
                    vec![Object::Name(name.clone()), Object::Real(new_size.max(0.5))],
                ));
            } else {
                out.push(op.clone());
            }
        }
        if let Some(tc) = edit.char_spacing {
            out.push(Operation::new("Tc", vec![Object::Real(tc)]));
        } else if let Some(op) = &snap.char_spacing {
            out.push(op.clone());
        }
        if let Some(tw) = edit.word_spacing {
            out.push(Operation::new("Tw", vec![Object::Real(tw)]));
        } else if let Some(op) = &snap.word_spacing {
            out.push(op.clone());
        }
        if let Some(op) = &snap.leading {
            out.push(op.clone());
        }
        if let Some(op) = &snap.h_scale {
            out.push(op.clone());
        }
        if let Some(op) = &snap.rise {
            out.push(op.clone());
        }
        if let Some(op) = &snap.render_mode {
            out.push(op.clone());
        }
        out.push(Operation::new(
            "Tm",
            vec![
                Object::Real(snap.text_matrix.a),
                Object::Real(snap.text_matrix.b),
                Object::Real(snap.text_matrix.c),
                Object::Real(snap.text_matrix.d),
                Object::Real(snap.text_matrix.e),
                Object::Real(snap.text_matrix.f),
            ],
        ));
        if let Some(op) = src_ops.get(obj.op_start).cloned() {
            let mut shown = op;
            if let Some(new_text) = &edit.text {
                if let Some((bytes, orig)) = encoded_text {
                    replace_text_operand_bytes(
                        &mut shown,
                        bytes,
                        Some(new_text.as_str()),
                        Some(orig),
                    );
                } else {
                    replace_text_operand(&mut shown, new_text);
                }
            }
            out.push(shown);
        }
        out.push(Operation::new("ET", vec![]));
    } else {
        for op in &src_ops[obj.op_start..obj.op_end] {
            out.push(op.clone());
        }
    }

    out.push(Operation::new("Q", vec![]));
}

fn has_geometric_transform(edit: &ObjectEdit) -> bool {
    edit.dx != 0.0
        || edit.dy != 0.0
        || edit.scale_x != 1.0
        || edit.scale_y != 1.0
        || edit.rotation != 0.0
        || edit.flip_horizontal
        || edit.flip_vertical
}

fn compute_m_edit(edit: &ObjectEdit, centre: Option<(f32, f32)>) -> Mat6 {
    if !has_geometric_transform(edit) {
        return Mat6::IDENTITY;
    }
    let (cx, cy) = centre.unwrap_or((0.0, 0.0));
    let sx = edit.scale_x.max(0.01) * if edit.flip_horizontal { -1.0 } else { 1.0 };
    let sy = edit.scale_y.max(0.01) * if edit.flip_vertical { -1.0 } else { 1.0 };
    Mat6::translation(-cx, -cy)
        .mul(Mat6::scaling(sx, sy))
        .mul(Mat6::rotation_cw(edit.rotation))
        .mul(Mat6::translation(cx + edit.dx, cy - edit.dy))
}

fn compose_with_edit(
    snap: &StateSnapshot,
    obj: &ContentObject,
    edit: &ObjectEdit,
    centre: Option<(f32, f32)>,
) -> Mat6 {
    let _ = snap;
    obj.ctm.mul(compute_m_edit(edit, centre))
}

fn replace_page_content(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    content: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = Stream::new(dictionary! {}, content);
    let _ = stream.compress();
    let new_id = doc.add_object(stream);
    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Contents", Object::Reference(new_id));
    Ok(())
}

#[allow(dead_code)]
fn splice_surgical_edit(
    ops: &mut Vec<Operation>,
    obj: &ContentObject,
    edit: &ObjectEdit,
    page_centre: Option<(f32, f32)>,
) {
    splice_surgical_edit_with_encoded(ops, obj, edit, page_centre, None, None);
}

fn splice_surgical_edit_with_encoded(
    ops: &mut Vec<Operation>,
    obj: &ContentObject,
    edit: &ObjectEdit,
    page_centre: Option<(f32, f32)>,
    encoded_text: Option<(&[u8], &str)>,
    gs_name: Option<&str>,
) {
    if obj.op_end > ops.len() {
        return;
    }

    if let Some(new_text) = &edit.text {
        if obj.kind == ObjectKind::Text {
            if let Some(op) = ops.get_mut(obj.op_start) {
                if let Some((bytes, orig)) = encoded_text {
                    replace_text_operand_bytes(
                        op,
                        bytes,
                        Some(new_text.as_str()),
                        Some(orig),
                    );
                } else {
                    replace_text_operand(op, new_text);
                }
            }
        }
    }

    if edit.delete {
        if obj.kind == ObjectKind::Text {
            if let Some(op) = ops.get_mut(obj.op_start) {
                empty_text_operand(op);
            }
        } else {
            ops.drain(obj.op_start..obj.op_end);
        }
        return;
    }

    let has_transform = edit.dx != 0.0
        || edit.dy != 0.0
        || edit.scale_x != 1.0
        || edit.scale_y != 1.0
        || edit.rotation != 0.0
        || edit.flip_horizontal
        || edit.flip_vertical;
    let has_fill = edit.fill_color.is_some();
    let has_stroke = edit.stroke_color.is_some();
    let has_stroke_w = edit.stroke_width.is_some();
    let has_font_size = edit.font_size.is_some() && obj.kind == ObjectKind::Text;
    let has_spacing = (edit.char_spacing.is_some() || edit.word_spacing.is_some())
        && obj.kind == ObjectKind::Text;
    let has_alpha = gs_name.is_some();
    if !has_transform
        && !has_fill
        && !has_stroke
        && !has_stroke_w
        && !has_font_size
        && !has_spacing
        && !has_alpha
    {
        return;
    }

    let mut prefix: Vec<Operation> = vec![Operation::new("q", vec![])];

    if let Some(name) = gs_name {
        prefix.push(Operation::new(
            "gs",
            vec![Object::Name(name.as_bytes().to_vec())],
        ));
    }

    if has_transform {
        let (cx, cy) = page_centre.unwrap_or((0.0, 0.0));
        let sx = edit.scale_x.max(0.01) * if edit.flip_horizontal { -1.0 } else { 1.0 };
        let sy = edit.scale_y.max(0.01) * if edit.flip_vertical { -1.0 } else { 1.0 };
        let m_edit = Mat6::translation(-cx, -cy)
            .mul(Mat6::scaling(sx, sy))
            .mul(Mat6::rotation_cw(edit.rotation))
            .mul(Mat6::translation(cx + edit.dx, cy - edit.dy));
        if let Some(m_inv) = obj.ctm.invert() {
            prefix.push(cm_op(m_inv));
            prefix.push(cm_op(m_edit));
            prefix.push(cm_op(obj.ctm));
        } else {
            prefix.push(cm_op(m_edit));
        }
    }
    if let Some(c) = edit.fill_color {
        let r = c[0] as f32 / 255.0;
        let g = c[1] as f32 / 255.0;
        let b = c[2] as f32 / 255.0;
        prefix.push(Operation::new(
            "rg",
            vec![Object::Real(r), Object::Real(g), Object::Real(b)],
        ));
    }
    if let Some(c) = edit.stroke_color {
        let r = c[0] as f32 / 255.0;
        let g = c[1] as f32 / 255.0;
        let b = c[2] as f32 / 255.0;
        prefix.push(Operation::new(
            "RG",
            vec![Object::Real(r), Object::Real(g), Object::Real(b)],
        ));
    }
    if let Some(w) = edit.stroke_width {
        prefix.push(Operation::new("w", vec![Object::Real(w.max(0.0))]));
    }
    if let Some(new_size) = edit.font_size {
        if obj.kind == ObjectKind::Text {
            if let Some(font_name) = find_current_font_name(ops, obj.op_start) {
                prefix.push(Operation::new(
                    "Tf",
                    vec![Object::Name(font_name), Object::Real(new_size.max(0.5))],
                ));
            }
        }
    }
    if obj.kind == ObjectKind::Text {
        if let Some(tc) = edit.char_spacing {
            prefix.push(Operation::new("Tc", vec![Object::Real(tc)]));
        }
        if let Some(tw) = edit.word_spacing {
            prefix.push(Operation::new("Tw", vec![Object::Real(tw)]));
        }
    }

    ops.splice(obj.op_end..obj.op_end, [Operation::new("Q", vec![])]);
    ops.splice(obj.op_start..obj.op_start, prefix);
}

fn replace_text_operand(op: &mut Operation, new_text: &str) -> bool {
    let new_bytes: Vec<u8> = new_text.bytes().collect();
    replace_text_operand_bytes(op, &new_bytes, Some(new_text), None)
}

fn empty_text_operand(op: &mut Operation) {
    match op.operator.as_str() {
        "Tj" | "'" => {
            if let Some(Object::String(ref mut bytes, _)) = op.operands.first_mut() {
                bytes.clear();
            }
        }
        "\"" => {
            if let Some(Object::String(ref mut bytes, _)) = op.operands.get_mut(2) {
                bytes.clear();
            }
        }
        "TJ" => {
            if let Some(Object::Array(ref mut arr)) = op.operands.first_mut() {
                arr.clear();
            }
        }
        _ => {}
    }
}

fn replace_text_operand_bytes(
    op: &mut Operation,
    new_bytes: &[u8],
    user_text: Option<&str>,
    original_text: Option<&str>,
) -> bool {
    match op.operator.as_str() {
        "Tj" | "'" => {
            if let Some(first) = op.operands.first_mut() {
                if let Object::String(ref mut bytes, _) = first {
                    let hybrid = match (user_text, original_text) {
                        (Some(ut), Some(ot)) => {
                            mix_bytes_char_level(bytes, new_bytes, ot, ut, None)
                                .unwrap_or_else(|| new_bytes.to_vec())
                        }
                        _ => new_bytes.to_vec(),
                    };
                    *bytes = hybrid;
                    return true;
                }
            }
        }
        "\"" => {
            if let Some(third) = op.operands.get_mut(2) {
                if let Object::String(ref mut bytes, _) = third {
                    let hybrid = match (user_text, original_text) {
                        (Some(ut), Some(ot)) => {
                            mix_bytes_char_level(bytes, new_bytes, ot, ut, None)
                                .unwrap_or_else(|| new_bytes.to_vec())
                        }
                        _ => new_bytes.to_vec(),
                    };
                    *bytes = hybrid;
                    return true;
                }
            }
        }
        "TJ" => {
            let per_word: Option<Vec<Vec<u8>>> = user_text.and_then(|ut| {
                match op.operands.first() {
                    Some(Object::Array(arr)) => split_encoded_text_by_space(new_bytes, ut, arr),
                    _ => None,
                }
            });
            let unchanged_mask: Option<Vec<bool>> =
                user_text.zip(original_text).and_then(|(ut, ot)| {
                    let nw: Vec<&str> = ut.split(' ').collect();
                    let ow: Vec<&str> = ot.split(' ').collect();
                    if nw.len() != ow.len() {
                        return None;
                    }
                    Some(nw.iter().zip(ow.iter()).map(|(a, b)| a == b).collect())
                });
            let per_word_hybrid: Option<Vec<Vec<u8>>> =
                per_word.as_ref().zip(user_text.zip(original_text)).and_then(
                    |(new_per_word, (ut, ot))| match op.operands.first() {
                        Some(Object::Array(arr)) => {
                            let orig_per_word = extract_tj_word_groups(arr);
                            let new_words: Vec<&str> = ut.split(' ').collect();
                            let orig_words: Vec<&str> = ot.split(' ').collect();
                            if orig_per_word.len() != new_per_word.len()
                                || new_words.len() != new_per_word.len()
                                || orig_words.len() != new_per_word.len()
                            {
                                return None;
                            }
                            let global_dict =
                                build_tj_global_char_dict(&orig_words, &orig_per_word);
                            Some(
                                new_per_word
                                    .iter()
                                    .enumerate()
                                    .map(|(i, n_bytes)| {
                                        mix_word_bytes(
                                            orig_words[i],
                                            new_words[i],
                                            &orig_per_word[i],
                                            n_bytes,
                                            Some(&global_dict),
                                        )
                                    })
                                    .collect(),
                            )
                        }
                        _ => None,
                    },
                );
            let final_per_word = per_word_hybrid.or(per_word);
            if let Some(words) = final_per_word {
                if redistribute_tj_by_words(op, &words, unchanged_mask.as_deref()) {
                    return true;
                }
            }
            if let (Some(ut), Some(ot)) = (user_text, original_text) {
                if substitute_tj_bytes_preserving_structure(op, new_bytes, ut, ot) {
                    return true;
                }
            }
            if let Some(first) = op.operands.first_mut() {
                if let Object::Array(ref mut arr) = first {
                    let mut placed = false;
                    for entry in arr.iter_mut() {
                        if let Object::String(ref mut bytes, _) = entry {
                            if !placed {
                                *bytes = new_bytes.to_vec();
                                placed = true;
                            } else {
                                bytes.clear();
                            }
                        }
                    }
                    if placed {
                        return true;
                    }
                }
            }
        }
        _ => {}
    }
    false
}

fn split_encoded_text_by_space(
    new_bytes: &[u8],
    user_text: &str,
    _arr: &[Object],
) -> Option<Vec<Vec<u8>>> {
    let char_count = user_text.chars().count();
    if char_count == 0 {
        return None;
    }
    if new_bytes.len() % char_count != 0 {
        return None;
    }
    let stride = new_bytes.len() / char_count;
    if stride == 0 {
        return None;
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut byte_idx = 0usize;
    for ch in user_text.chars() {
        if ch == ' ' {
            out.push(std::mem::take(&mut current));
        } else {
            let end = (byte_idx + stride).min(new_bytes.len());
            current.extend_from_slice(&new_bytes[byte_idx..end]);
        }
        byte_idx += stride;
    }
    out.push(current);
    Some(out)
}

fn extract_tj_word_groups(arr: &[Object]) -> Vec<Vec<u8>> {
    const JUSTIFY_THRESHOLD: f32 = -100.0;
    let mut groups: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for entry in arr {
        let v: Option<f32> = match entry {
            Object::Integer(n) => Some(*n as f32),
            Object::Real(n) => Some(*n),
            _ => None,
        };
        if let Some(v) = v {
            if v <= JUSTIFY_THRESHOLD {
                groups.push(std::mem::take(&mut current));
                continue;
            }
            continue;
        }
        if let Object::String(b, _) = entry {
            current.extend_from_slice(b);
        }
    }
    groups.push(current);
    groups
}

fn mix_word_bytes(
    orig_text: &str,
    new_text: &str,
    orig_bytes: &[u8],
    new_bytes: &[u8],
    supplemental_dict: Option<&std::collections::HashMap<char, Vec<u8>>>,
) -> Vec<u8> {
    mix_bytes_char_level(orig_bytes, new_bytes, orig_text, new_text, supplemental_dict)
        .unwrap_or_else(|| new_bytes.to_vec())
}

fn build_tj_global_char_dict(
    orig_words: &[&str],
    orig_word_bytes: &[Vec<u8>],
) -> std::collections::HashMap<char, Vec<u8>> {
    let mut dict: std::collections::HashMap<char, Vec<u8>> =
        std::collections::HashMap::new();
    for (text, bytes) in orig_words.iter().zip(orig_word_bytes.iter()) {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() || bytes.is_empty() {
            continue;
        }
        if bytes.len() % chars.len() != 0 {
            continue;
        }
        let stride = bytes.len() / chars.len();
        if stride == 0 {
            continue;
        }
        for (i, ch) in chars.iter().enumerate() {
            let start = i * stride;
            let end = start + stride;
            if end <= bytes.len() {
                dict.entry(*ch).or_insert_with(|| bytes[start..end].to_vec());
            }
        }
    }
    dict
}

fn mix_bytes_char_level(
    orig_bytes: &[u8],
    new_bytes: &[u8],
    orig_text: &str,
    new_text: &str,
    supplemental_dict: Option<&std::collections::HashMap<char, Vec<u8>>>,
) -> Option<Vec<u8>> {
    let orig_text = {
        let direct = orig_text.chars().count();
        if direct > 0 && orig_bytes.len() % direct == 0 {
            orig_text
        } else {
            let trimmed = orig_text.trim();
            let trimmed_count = trimmed.chars().count();
            if trimmed_count > 0 && orig_bytes.len() % trimmed_count == 0 {
                trimmed
            } else {
                return None;
            }
        }
    };
    let orig_chars: Vec<char> = orig_text.chars().collect();
    let new_chars: Vec<char> = new_text.chars().collect();
    if orig_chars.is_empty() || new_chars.is_empty() {
        return None;
    }
    if new_bytes.len() % new_chars.len() != 0 {
        return None;
    }
    let orig_stride = orig_bytes.len() / orig_chars.len();
    let new_stride = new_bytes.len() / new_chars.len();
    if orig_stride != new_stride || orig_stride == 0 {
        return None;
    }
    let stride = new_stride;

    let mut char_to_orig: std::collections::HashMap<char, Vec<u8>> =
        std::collections::HashMap::new();
    for (i, ch) in orig_chars.iter().enumerate() {
        let start = i * stride;
        let end = start + stride;
        if end <= orig_bytes.len() {
            char_to_orig
                .entry(*ch)
                .or_insert_with(|| orig_bytes[start..end].to_vec());
        }
    }
    if let Some(extra) = supplemental_dict {
        for (ch, bytes) in extra {
            if bytes.len() == stride {
                char_to_orig.entry(*ch).or_insert_with(|| bytes.clone());
            }
        }
    }
    let lookup = |ch: char| -> Option<&[u8]> {
        if let Some(b) = char_to_orig.get(&ch) {
            return Some(b.as_slice());
        }
        let alt: Vec<char> = if ch.is_lowercase() {
            ch.to_uppercase().collect()
        } else if ch.is_uppercase() {
            ch.to_lowercase().collect()
        } else {
            Vec::new()
        };
        for c in alt {
            if let Some(b) = char_to_orig.get(&c) {
                return Some(b.as_slice());
            }
        }
        None
    };

    let chars_eq = |a: &char, b: &char| -> bool {
        a == b
            || a.to_lowercase().eq(b.to_lowercase())
    };
    let prefix_match = orig_chars
        .iter()
        .zip(new_chars.iter())
        .take_while(|(a, b)| chars_eq(a, b))
        .count();
    let max_suffix = orig_chars.len().min(new_chars.len()).saturating_sub(prefix_match);
    let suffix_match = orig_chars
        .iter()
        .rev()
        .zip(new_chars.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| chars_eq(a, b))
        .count();

    let mut out = Vec::with_capacity(new_bytes.len());
    out.extend_from_slice(&orig_bytes[..prefix_match * stride]);
    let new_middle_end = new_chars.len() - suffix_match;
    for (j, ch) in new_chars[prefix_match..new_middle_end].iter().enumerate() {
        let global = prefix_match + j;
        let pdfium_start = global * stride;
        let pdfium_end = pdfium_start + stride;
        if let Some(src) = lookup(*ch) {
            out.extend_from_slice(src);
        } else {
            out.extend_from_slice(&new_bytes[pdfium_start..pdfium_end]);
        }
    }
    let orig_suffix_start = orig_chars.len() - suffix_match;
    out.extend_from_slice(&orig_bytes[orig_suffix_start * stride..]);
    Some(out)
}

fn substitute_tj_bytes_preserving_structure(
    op: &mut Operation,
    new_bytes: &[u8],
    new_text: &str,
    orig_text: &str,
) -> bool {
    if op.operator != "TJ" {
        return false;
    }
    let arr = match op.operands.first_mut() {
        Some(Object::Array(arr)) => arr,
        _ => return false,
    };

    let mut orig_total: Vec<u8> = Vec::new();
    let mut slot_indices: Vec<usize> = Vec::new();
    let mut slot_orig_lens: Vec<usize> = Vec::new();
    for (i, entry) in arr.iter().enumerate() {
        if let Object::String(b, _) = entry {
            slot_indices.push(i);
            slot_orig_lens.push(b.len());
            orig_total.extend_from_slice(b);
        }
    }
    if slot_indices.is_empty() {
        return false;
    }
    let hybrid = match mix_bytes_char_level(&orig_total, new_bytes, orig_text, new_text, None) {
        Some(h) => h,
        None => return false,
    };

    if hybrid.len() == orig_total.len() {
        let mut offset = 0usize;
        for (slot_idx, slot_len) in slot_indices.iter().zip(slot_orig_lens.iter()) {
            if let Object::String(ref mut b, _) = arr[*slot_idx] {
                let end = offset + slot_len;
                *b = hybrid[offset..end].to_vec();
                offset = end;
            }
        }
        return true;
    }

    if let Some(&first_idx) = slot_indices.first() {
        if let Object::String(ref mut b, _) = arr[first_idx] {
            *b = hybrid;
        }
        for &slot_idx in slot_indices.iter().skip(1) {
            if let Object::String(ref mut b, _) = arr[slot_idx] {
                b.clear();
            }
        }
        for entry in arr.iter_mut() {
            match entry {
                Object::Integer(n) => *n = 0,
                Object::Real(n) => *n = 0.0,
                _ => {}
            }
        }
        return true;
    }
    false
}

fn redistribute_tj_by_words(
    op: &mut Operation,
    per_word_bytes: &[Vec<u8>],
    unchanged_mask: Option<&[bool]>,
) -> bool {
    const JUSTIFY_THRESHOLD: f32 = -100.0;
    if op.operator != "TJ" {
        return false;
    }
    let arr = match op.operands.first_mut() {
        Some(Object::Array(arr)) => arr,
        _ => return false,
    };

    let separator_idxs: Vec<usize> = arr
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let v: Option<f32> = match e {
                Object::Integer(n) => Some(*n as f32),
                Object::Real(n) => Some(*n),
                _ => None,
            };
            v.and_then(|v| if v <= JUSTIFY_THRESHOLD { Some(i) } else { None })
        })
        .collect();
    let expected_word_count = separator_idxs.len() + 1;
    if per_word_bytes.len() != expected_word_count {
        return false;
    }

    let mut group_ranges: Vec<(usize, usize)> = Vec::with_capacity(expected_word_count);
    let mut prev = 0usize;
    for &sep in &separator_idxs {
        group_ranges.push((prev, sep));
        prev = sep + 1;
    }
    group_ranges.push((prev, arr.len()));

    for (idx, ((start, end), new_bytes)) in group_ranges
        .iter()
        .zip(per_word_bytes.iter())
        .enumerate()
    {
        if unchanged_mask
            .and_then(|m| m.get(idx).copied())
            .unwrap_or(false)
        {
            continue;
        }
        let mut placed = false;
        for entry in &mut arr[*start..*end] {
            match entry {
                Object::String(ref mut bytes, _) => {
                    if !placed {
                        *bytes = new_bytes.to_vec();
                        placed = true;
                    } else {
                        bytes.clear();
                    }
                }
                Object::Integer(ref mut n) => *n = 0,
                Object::Real(ref mut n) => *n = 0.0,
                _ => {}
            }
        }
        if !placed {
            return false;
        }
    }
    true
}

fn find_current_font_name(ops: &[Operation], before: usize) -> Option<Vec<u8>> {
    let end = before.min(ops.len());
    for op in ops[..end].iter().rev() {
        if op.operator == "Tf" {
            if let Some(Object::Name(name)) = op.operands.first() {
                return Some(name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_font_name_detection() {
        assert!(is_subset_font_name("ABCDEF+Helvetica"));
        assert!(is_subset_font_name("QWERTY+Minion-Bold"));
        assert!(!is_subset_font_name("Helvetica"));
        assert!(!is_subset_font_name("ABCDE+Short"));
        assert!(!is_subset_font_name("abcdef+lower"));
        assert!(!is_subset_font_name("ABCDEF"));
        assert!(!is_subset_font_name("Times+Roman"));
    }

    #[test]
    fn strip_subset_prefix_removes_tag() {
        assert_eq!(strip_subset_prefix("ABCDEF+NimbusRomNo9L-Medi"), "NimbusRomNo9L-Medi");
        assert_eq!(strip_subset_prefix("Helvetica"), "Helvetica");
        assert_eq!(strip_subset_prefix("Times+Roman"), "Times+Roman");
    }

    #[test]
    fn dedupe_collapses_same_face_subsets() {
        let mk = |tag: &str, prog: Option<usize>| EmbeddedFontInfo {
            id: (1, 0),
            base_font: format!("{tag}+NimbusRomNo9L-Medi"),
            subtype: "Type1".into(),
            is_subset: true,
            is_simple: true,
            program: prog.map(|n| std::sync::Arc::new(vec![0u8; n])),
        };
        let input = vec![
            mk("AAAAAA", Some(100)),
            mk("BBBBBB", Some(400)),
            mk("CCCCCC", None),
        ];
        let out = dedupe_embedded_fonts(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base_font, "NimbusRomNo9L-Medi");
        assert_eq!(out[0].program.as_ref().map(|p| p.len()), Some(400));
        let mixed = vec![
            mk("AAAAAA", Some(100)),
            EmbeddedFontInfo {
                id: (2, 0),
                base_font: "DDDDDD+NimbusRomNo9L-Medi".into(),
                subtype: "TrueType".into(),
                is_subset: true,
                is_simple: true,
                program: Some(std::sync::Arc::new(vec![0u8; 50])),
            },
        ];
        assert_eq!(dedupe_embedded_fonts(mixed).len(), 2);
    }

    #[test]
    fn sfnt_magic_detection() {
        assert!(has_sfnt_magic(&[0x00, 0x01, 0x00, 0x00, 0x99]));
        assert!(has_sfnt_magic(b"true...."));
        assert!(has_sfnt_magic(b"OTTO...."));
        assert!(has_sfnt_magic(b"ttcf...."));
        assert!(!has_sfnt_magic(b"\x01\x00\x04"));
        assert!(!has_sfnt_magic(b"%!PS-Adobe"));
        assert!(!has_sfnt_magic(&[]));
    }

    #[test]
    fn overlay_text_single_byte_encoding() {
        assert_eq!(encode_overlay_text_bytes("Hi!"), b"Hi!".to_vec());
        assert_eq!(encode_overlay_text_bytes("é"), vec![0xE9]);
        assert_eq!(encode_overlay_text_bytes("a€b"), b"ab".to_vec());
    }

    #[test]
    fn overlay_text_needs_unicode_detection() {
        assert!(!overlay_text_needs_unicode("Hello, café!"));
        assert!(overlay_text_needs_unicode("Привет"));
        assert!(overlay_text_needs_unicode("€100"));
        assert!(overlay_text_needs_unicode("汉字"));
    }

    #[test]
    fn overlay_font_advice_classifies() {
        assert!(matches!(overlay_font_advice("Hello"), OverlayFontAdvice::Standard));
        match overlay_font_advice("Привет") {
            OverlayFontAdvice::Unicode { unsupported } => assert!(unsupported.is_empty()),
            _ => panic!("expected unicode advice for Cyrillic"),
        }
        match overlay_font_advice("Hi 汉") {
            OverlayFontAdvice::Unicode { unsupported } => assert_eq!(unsupported, vec!['汉']),
            _ => panic!("expected unicode advice with unsupported char"),
        }
    }

    #[test]
    fn identity_h_encodes_two_bytes_per_glyph() {
        let face = ttf_parser::Face::parse(UNICODE_FONT_BYTES, 0).unwrap();
        assert_eq!(encode_identity_h(&face, "Ав").len(), 4);
        assert_eq!(encode_identity_h(&face, "A\nB").len(), 4);
        assert_eq!(encode_identity_h(&face, "汉").len(), 0);
    }

    #[test]
    fn overlay_export_embeds_unicode_font_for_cyrillic() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 400.into(), 600.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let dir = std::env::temp_dir().join(format!("paper-uni-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        doc.save(&input).unwrap();

        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let edit = OverlayEdit {
            page_index: 0,
            kind: OverlayKind::Text,
            x: 10.0,
            y: 15.0,
            width: 200.0,
            height: 40.0,
            ref_width: 200.0,
            ref_height: 40.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: Some("Привет".into()),
            font_size: Some(18.0),
            color: Some([0, 0, 0]),
            font_family: Some("Helvetica".into()),
            font_embedded_id: None,
            image_data: None,
        };
        export_overlays(&pdfium, &input, &output, &[edit]).unwrap();

        let exported = Document::load(&output).unwrap();
        let mut found_type0 = false;
        for obj in exported.objects.values() {
            if let Object::Dictionary(d) = obj {
                let sub = d.get(b"Subtype").ok().and_then(|o| o.as_name().ok());
                let enc = d.get(b"Encoding").ok().and_then(|o| o.as_name().ok());
                if sub.map(|n| n == &b"Type0"[..]).unwrap_or(false)
                    && enc.map(|n| n == &b"Identity-H"[..]).unwrap_or(false)
                {
                    found_type0 = true;
                }
            }
        }
        assert!(found_type0, "expected an embedded Type0/Identity-H font");

        let (_, page_id) = exported.get_pages().into_iter().next().unwrap();
        let content = Content::decode(&exported.get_page_content(page_id).unwrap()).unwrap();
        let tj = content
            .operations
            .iter()
            .find(|op| op.operator == "Tj")
            .expect("expected a Tj operator");
        let bytes = match &tj.operands[0] {
            Object::String(b, _) => b.clone(),
            other => panic!("Tj operand was not a string: {other:?}"),
        };
        assert!(!bytes.is_empty(), "identity-h string should not be empty");
        assert_eq!(bytes.len() % 2, 0, "identity-h uses two bytes per glyph");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_export_falls_back_to_unicode_with_embedded_font_selected() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 400.into(), 600.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let dir = std::env::temp_dir().join(format!("paper-uni-emb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        doc.save(&input).unwrap();

        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let edit = OverlayEdit {
            page_index: 0,
            kind: OverlayKind::Text,
            x: 10.0,
            y: 15.0,
            width: 200.0,
            height: 40.0,
            ref_width: 200.0,
            ref_height: 40.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: Some("Привет".into()),
            font_size: Some(18.0),
            color: Some([0, 0, 0]),
            font_family: Some("LucidaGrande".into()),
            font_embedded_id: Some((999, 0)),
            image_data: None,
        };
        export_overlays(&pdfium, &input, &output, &[edit]).unwrap();

        let exported = Document::load(&output).unwrap();
        let mut found_type0 = false;
        for obj in exported.objects.values() {
            if let Object::Dictionary(d) = obj {
                let sub = d.get(b"Subtype").ok().and_then(|o| o.as_name().ok());
                if sub.map(|n| n == &b"Type0"[..]).unwrap_or(false) {
                    found_type0 = true;
                }
            }
        }
        assert!(
            found_type0,
            "non-Latin text should embed the Unicode font even when a document font is selected"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_export_emits_one_tj_per_line() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 400.into(), 600.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let dir = std::env::temp_dir().join(format!("paper-multiline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        doc.save(&input).unwrap();

        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let edit = OverlayEdit {
            page_index: 0,
            kind: OverlayKind::Text,
            x: 10.0,
            y: 15.0,
            width: 200.0,
            height: 60.0,
            ref_width: 200.0,
            ref_height: 60.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: Some("hola\nmundo".into()),
            font_size: Some(18.0),
            color: Some([0, 0, 0]),
            font_family: Some("Helvetica".into()),
            font_embedded_id: None,
            image_data: None,
        };
        export_overlays(&pdfium, &input, &output, &[edit]).unwrap();

        let exported = Document::load(&output).unwrap();
        let (_, page_id) = exported.get_pages().into_iter().next().unwrap();
        let content = Content::decode(&exported.get_page_content(page_id).unwrap()).unwrap();
        let tj = content.operations.iter().filter(|op| op.operator == "Tj").count();
        assert_eq!(tj, 2, "each text line should be shown with its own Tj");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore]
    fn overlay_cyrillic_renders_visible_pixels() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 400.into(), 200.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let dir = std::env::temp_dir().join(format!("paper-uni-render-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        doc.save(&input).unwrap();

        let pdfium = load_pdfium().expect("pdfium");
        let edit = OverlayEdit {
            page_index: 0,
            kind: OverlayKind::Text,
            x: 20.0,
            y: 80.0,
            width: 300.0,
            height: 60.0,
            ref_width: 300.0,
            ref_height: 60.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: Some("Привет, мир".into()),
            font_size: Some(40.0),
            color: Some([0, 0, 0]),
            font_family: Some("Helvetica".into()),
            font_embedded_id: None,
            image_data: None,
        };
        export_overlays(&pdfium, &input, &output, &[edit]).unwrap();

        let dark = |p: &std::path::Path| -> usize {
            let doc = pdfium.load_pdf_from_file(p, None).expect("load");
            let page = doc.pages().get(0).expect("page");
            let cfg = PdfRenderConfig::new().set_target_width(400).set_maximum_height(8000);
            let px = page.render_with_config(&cfg).expect("render").as_image().into_rgb8().into_raw();
            px.chunks(3).filter(|c| c.iter().any(|&v| v < 128)).count()
        };
        let before = dark(&input);
        let after = dark(&output);
        eprintln!("dark pixels before={before} after={after}");
        assert!(
            after > before + 50,
            "Cyrillic overlay should add visible pixels (before={before}, after={after})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enumerate_objects_matches_pdfium_order() {
        let ops = vec![
            Operation::new("BDC", vec![Object::Name(b"Artifact".to_vec())]),
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(50.0),
                    Object::Real(60.0),
                ],
            ),
            Operation::new("rg", vec![Object::Real(1.0), Object::Real(0.0), Object::Real(0.0)]),
            Operation::new("m", vec![Object::Real(0.0), Object::Real(0.0)]),
            Operation::new("l", vec![Object::Real(10.0), Object::Real(10.0)]),
            Operation::new("S", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Real(12.0)]),
            Operation::new("Tj", vec![Object::string_literal(b"hi".to_vec())]),
            Operation::new("ET", vec![]),
            Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
            Operation::new("sh", vec![Object::Name(b"Sh1".to_vec())]),
            Operation::new("EMC", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        let kinds: Vec<ObjectKind> = objs.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            vec![ObjectKind::Path, ObjectKind::Text, ObjectKind::Image, ObjectKind::Shading]
        );
        assert_eq!(objs[0].op_start, 4);
        assert_eq!(objs[0].op_end, 7);
        assert_eq!(objs[1].op_start, 10);
        assert_eq!(objs[1].op_end, 11);
        assert!((objs[0].ctm.e - 50.0).abs() < 0.01);
        assert!((objs[0].ctm.f - 60.0).abs() < 0.01);
    }

    #[test]
    fn splice_surgical_translate_wraps_only_target() {
        let mut ops = vec![
            Operation::new("BDC", vec![]),
            Operation::new("m", vec![Object::Real(0.0), Object::Real(0.0)]),
            Operation::new("l", vec![Object::Real(10.0), Object::Real(0.0)]),
            Operation::new("S", vec![]),
            Operation::new("m", vec![Object::Real(20.0), Object::Real(0.0)]),
            Operation::new("l", vec![Object::Real(30.0), Object::Real(0.0)]),
            Operation::new("S", vec![]),
            Operation::new("EMC", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        assert_eq!(objs.len(), 2);
        let mut edit = ObjectEdit::new(0, 1);
        edit.dx = 5.0;
        edit.dy = -7.0;
        splice_surgical_edit(&mut ops, &objs[1], &edit, Some((0.0, 0.0)));
        assert_eq!(ops[0].operator, "BDC");
        assert_eq!(ops[1].operator, "m");
        assert_eq!(ops[2].operator, "l");
        assert_eq!(ops[3].operator, "S");
        assert_eq!(ops[4].operator, "q");
        let q_count = ops.iter().filter(|o| o.operator == "Q").count();
        assert_eq!(q_count, 1);
        assert_eq!(ops.last().unwrap().operator, "EMC");
    }

    #[test]
    fn walker_captures_nested_clip_stack() {
        let ops = vec![
            Operation::new("q", vec![]),
            Operation::new("re", vec![
                Object::Real(0.0), Object::Real(0.0),
                Object::Real(100.0), Object::Real(100.0),
            ]),
            Operation::new("W", vec![]),
            Operation::new("n", vec![]),
            Operation::new("q", vec![]),
            Operation::new("re", vec![
                Object::Real(10.0), Object::Real(10.0),
                Object::Real(40.0), Object::Real(40.0),
            ]),
            Operation::new("W*", vec![]),
            Operation::new("n", vec![]),
            Operation::new("m", vec![Object::Real(20.0), Object::Real(20.0)]),
            Operation::new("l", vec![Object::Real(30.0), Object::Real(30.0)]),
            Operation::new("S", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("Q", vec![]),
        ];
        let (objs, snaps) = walk_with_snapshots(&ops);
        assert_eq!(objs.len(), 1);
        let snap = &snaps[0];
        assert_eq!(snap.clip_stack.len(), 2, "expected page clip + nested clip");
        assert_eq!(snap.clip_stack[0].kind, "W");
        assert_eq!(snap.clip_stack[1].kind, "W*");
    }

    #[test]
    fn splice_surgical_emits_gs_for_alpha() {
        let mut ops = vec![
            Operation::new("m", vec![Object::Real(0.0), Object::Real(0.0)]),
            Operation::new("l", vec![Object::Real(10.0), Object::Real(0.0)]),
            Operation::new("S", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        assert_eq!(objs.len(), 1);
        let mut edit = ObjectEdit::new(0, 0);
        edit.fill_color = Some([255, 0, 0]);
        edit.fill_alpha = Some(128);
        splice_surgical_edit_with_encoded(
            &mut ops,
            &objs[0],
            &edit,
            Some((0.0, 0.0)),
            None,
            Some("PFa0"),
        );
        let operators: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(operators, vec!["q", "gs", "rg", "m", "l", "S", "Q"]);
        match &ops[1].operands[0] {
            Object::Name(n) => assert_eq!(n.as_slice(), b"PFa0"),
            _ => panic!("expected /Name operand on gs"),
        }
    }

    #[test]
    fn splice_surgical_emits_tc_tw_for_spacing_override() {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tj", vec![Object::string_literal(b"hello".to_vec())]),
            Operation::new("ET", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        assert_eq!(objs.len(), 1);
        let mut edit = ObjectEdit::new(0, 0);
        edit.char_spacing = Some(0.5);
        edit.word_spacing = Some(2.0);
        splice_surgical_edit_with_encoded(
            &mut ops,
            &objs[0],
            &edit,
            Some((0.0, 0.0)),
            None,
            None,
        );
        let operators: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(operators, vec!["BT", "q", "Tc", "Tw", "Tj", "Q", "ET"]);
        match &ops[2].operands[0] {
            Object::Real(v) => assert!((v - 0.5).abs() < f32::EPSILON),
            _ => panic!("expected Real operand on Tc"),
        }
        match &ops[3].operands[0] {
            Object::Real(v) => assert!((v - 2.0).abs() < f32::EPSILON),
            _ => panic!("expected Real operand on Tw"),
        }
    }

    #[test]
    fn splice_surgical_ignores_spacing_on_path_objects() {
        let mut ops = vec![
            Operation::new("m", vec![Object::Real(0.0), Object::Real(0.0)]),
            Operation::new("l", vec![Object::Real(10.0), Object::Real(0.0)]),
            Operation::new("S", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        let mut edit = ObjectEdit::new(0, 0);
        edit.char_spacing = Some(1.0);
        edit.word_spacing = Some(1.0);
        splice_surgical_edit_with_encoded(
            &mut ops,
            &objs[0],
            &edit,
            Some((0.0, 0.0)),
            None,
            None,
        );
        let operators: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(operators, vec!["m", "l", "S"]);
    }

    #[test]
    fn top_level_pos_after_exits_q_block() {
        let ops = vec![
            Operation::new("q", vec![]),
            Operation::new("cm", vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()]),
            Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
            Operation::new("Q", vec![]),
            Operation::new("re", vec![0.into(), 0.into(), 1.into(), 1.into()]),
        ];
        assert_eq!(top_level_pos_after(&ops, 3), 4);
        assert_eq!(top_level_pos_after(&ops, 5), 5);
        let flat = vec![
            Operation::new("re", vec![0.into(), 0.into(), 1.into(), 1.into()]),
            Operation::new("f", vec![]),
        ];
        assert_eq!(top_level_pos_after(&flat, 2), 2);
    }

    #[test]
    fn surgical_duplicate_appends_drawing_block() {
        use lopdf::content::Content;
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let ops = vec![
            Operation::new("m", vec![0.into(), 0.into()]),
            Operation::new("l", vec![10.into(), 0.into()]),
            Operation::new("S", vec![]),
        ];
        let content_bytes = Content { operations: ops }.encode().unwrap();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let mut dup = ObjectEdit::new(0, 0);
        dup.copy_seq = Some(0);
        dup.dx = 50.0;
        let mut surgical: HashMap<usize, Vec<(ObjectEdit, Option<(f32, f32)>)>> = HashMap::new();
        surgical.insert(0, vec![(dup, Some((5.0, 5.0)))]);
        let encoded: HashMap<(usize, usize, Option<u32>), (Vec<u8>, String)> = HashMap::new();
        apply_surgical_edits_to_doc_with_encoded(&mut doc, &surgical, &encoded).unwrap();

        let new_content = Content::decode(&doc.get_page_content(page_id).unwrap()).unwrap();
        let q = new_content.operations.iter().filter(|o| o.operator == "q").count();
        let qq = new_content.operations.iter().filter(|o| o.operator == "Q").count();
        assert!(q >= 1 && q == qq, "expected balanced appended q/Q (q={q}, Q={qq})");
        let strokes = new_content.operations.iter().filter(|o| o.operator == "S").count();
        assert_eq!(strokes, 2, "duplicate should re-emit the stroked path");
    }

    #[test]
    fn replace_text_operand_redistributes_tj_when_word_counts_match() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"T".to_vec()),
                Object::Integer(74),
                Object::string_literal(b"raining".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"Generative".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"Networks".to_vec()),
            ])],
        );
        let new_bytes = b"Hello big Network";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("Hello big Network"),
            None,
        ));
        assert_eq!(op.operator, "TJ");
        let arr = match &op.operands[0] {
            Object::Array(a) => a,
            _ => panic!("expected Array"),
        };
        assert!(matches!(arr[1], Object::Integer(0)));
        assert!(matches!(arr[3], Object::Integer(-250)));
        assert!(matches!(arr[5], Object::Integer(-250)));
        match &arr[0] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"Hello"),
            _ => panic!("expected String at arr[0]"),
        }
        match &arr[2] {
            Object::String(b, _) => assert!(b.is_empty()),
            _ => panic!("expected String at arr[2]"),
        }
        match &arr[4] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"big"),
            _ => panic!("expected String at arr[4]"),
        }
        match &arr[6] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"Network"),
            _ => panic!("expected String at arr[6]"),
        }
    }

    #[test]
    fn replace_text_operand_falls_back_when_word_counts_differ() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"One".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"Two".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"Three".to_vec()),
            ])],
        );
        let new_bytes = b"Hello world";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("Hello world"),
            None,
        ));
        assert_eq!(op.operator, "TJ");
        let arr = match &op.operands[0] {
            Object::Array(a) => a,
            _ => panic!(),
        };
        match &arr[0] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"Hello world"),
            _ => panic!(),
        }
        match &arr[2] {
            Object::String(b, _) => assert!(b.is_empty()),
            _ => panic!(),
        }
    }

    #[test]
    fn replace_text_operand_keeps_unchanged_word_slots_untouched() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"Francisco".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"Martinez".to_vec()),
            ])],
        );
        let new_bytes = b"XXXXXXXXX YYYYYYYY";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("Francisca Martinez"),
            Some("Francisco Martinez"),
        ));
        let arr = match &op.operands[0] {
            Object::Array(a) => a,
            _ => panic!(),
        };
        match &arr[2] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"Martinez"),
            _ => panic!("expected unchanged orig bytes at arr[2]"),
        }
        match &arr[0] {
            Object::String(b, _) => {
                assert_eq!(b.len(), 9);
                assert_eq!(b.as_slice(), b"Francisca");
            }
            _ => panic!("expected hybrid bytes at arr[0]"),
        }
    }

    #[test]
    fn mix_word_bytes_keeps_orig_for_common_prefix_and_suffix() {
        let orig = b"AAAAA";
        let newb = b"NNNNN";
        let got = mix_word_bytes("color", "colur", orig, newb, None);
        assert_eq!(got.as_slice(), b"AAANA");
    }

    #[test]
    fn redistribute_uses_global_tj_dict_for_cross_word_chars() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"hi".to_vec()),
                Object::Integer(-250),
                Object::string_literal(b"No".to_vec()),
            ])],
        );
        let new_bytes = b"hX No";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("ho No"),
            Some("hi No"),
        ));
        let arr = match &op.operands[0] {
            Object::Array(a) => a,
            _ => panic!(),
        };
        match &arr[0] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"ho"),
            _ => panic!("expected hybrid bytes at arr[0]"),
        }
        match &arr[2] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"No"),
            _ => panic!("expected unchanged orig bytes at arr[2]"),
        }
    }

    #[test]
    fn substitute_tj_preserves_slot_widths_and_kernings() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"F".to_vec()),
                Object::Integer(22),
                Object::string_literal(b"rancisc".to_vec()),
                Object::Integer(7),
                Object::string_literal(b"o Mart\xEDnez C.".to_vec()),
            ])],
        );
        let new_bytes = b"FRANcIScA MART\xEDNEz C.";
        let new_text = "Francisca Martínez C.";
        let orig_text = "Francisco Martínez C.";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some(new_text),
            Some(orig_text),
        ));
        let arr = match &op.operands[0] {
            Object::Array(a) => a,
            _ => panic!(),
        };
        match &arr[0] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"F"),
            _ => panic!("expected String at arr[0]"),
        }
        assert!(matches!(arr[1], Object::Integer(22)));
        match &arr[2] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"rancisc"),
            _ => panic!("expected String at arr[2]"),
        }
        assert!(matches!(arr[3], Object::Integer(7)));
        match &arr[4] {
            Object::String(b, _) => {
                assert_eq!(b.len(), 13);
                assert_eq!(b[0], b'a');
                assert_eq!(&b[1..], b" Mart\xEDnez C.");
            }
            _ => panic!("expected String at arr[4]"),
        }
    }

    #[test]
    fn replace_text_operand_tj_uses_char_level_mixing() {
        let mut op = Operation::new(
            "Tj",
            vec![Object::string_literal(b"para la vida humana.".to_vec())],
        );
        let new_bytes = b"BADBADBADBADBADBADBA";
        assert_eq!(new_bytes.len(), "para la vida animal.".len());
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("para la vida animal."),
            Some("para la vida humana."),
        ));
        match &op.operands[0] {
            Object::String(b, _) => assert_eq!(b.as_slice(), b"para la vida animal."),
            _ => panic!("expected String at operand 0"),
        }
    }

    #[test]
    fn mix_bytes_handles_case_mismatch_in_pdfium_decoded_text() {
        let orig_bytes = b"Directora:";
        let new_bytes_pdfium = b"BADBADBADX";
        let got = mix_bytes_char_level(
            orig_bytes,
            new_bytes_pdfium,
            "DirectOra:",
            "Directoro:",
            None,
        )
        .expect("mix should succeed despite case mismatch");
        assert_eq!(got.as_slice(), b"Directoro:");
    }

    #[test]
    fn mix_bytes_normalizes_trailing_whitespace_from_pdfium_text() {
        let orig_bytes = b"Directora:";
        let new_bytes_pdfium = b"BADBADBADXY";
        let got = mix_bytes_char_level(
            orig_bytes,
            new_bytes_pdfium,
            "Directora: ",
            "Directoro: ",
            None,
        )
        .expect("mix should normalize whitespace and succeed");
        assert_eq!(got.len(), 11);
        assert_eq!(&got[..10], b"Directoro:");
        assert_eq!(got[10], b'Y');
    }

    #[test]
    fn substitute_tj_falls_back_for_unseen_chars() {
        let mut op = Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(b"abc".to_vec()),
            ])],
        );
        let new_bytes = b"NQN";
        assert!(replace_text_operand_bytes(
            &mut op,
            new_bytes,
            Some("aQc"),
            Some("abc"),
        ));
        match &op.operands[0] {
            Object::Array(a) => match &a[0] {
                Object::String(b, _) => assert_eq!(b.as_slice(), b"aQc"),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn mix_word_bytes_handles_length_mismatch() {
        let got = mix_word_bytes("cat", "cart", b"AAA", b"NNNN", None);
        assert_eq!(got.as_slice(), b"AANA");
        let got = mix_word_bytes("cart", "cat", b"AAAA", b"NNN", None);
        assert_eq!(got.as_slice(), b"AAA");
    }

    #[test]
    fn splice_surgical_delete_text_empties_operand() {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tj", vec![Object::string_literal(b"a".to_vec())]),
            Operation::new("Tj", vec![Object::string_literal(b"b".to_vec())]),
            Operation::new("ET", vec![]),
        ];
        let objs = enumerate_content_objects(&ops);
        assert_eq!(objs.len(), 2);
        let mut edit = ObjectEdit::new(0, 0);
        edit.delete = true;
        splice_surgical_edit(&mut ops, &objs[0], &edit, Some((0.0, 0.0)));
        let operators: Vec<&str> = ops.iter().map(|o| o.operator.as_str()).collect();
        assert_eq!(operators, vec!["BT", "Tj", "Tj", "ET"]);
        let first_tj_bytes = match &ops[1].operands[0] {
            Object::String(b, _) => b.clone(),
            _ => panic!("expected Tj string operand"),
        };
        assert!(first_tj_bytes.is_empty(), "deleted Tj should have an empty string operand");
        let second_tj_bytes = match &ops[2].operands[0] {
            Object::String(b, _) => b.clone(),
            _ => panic!("expected Tj string operand"),
        };
        assert_eq!(second_tj_bytes, b"b", "neighbouring Tj must stay intact");
    }

    #[test]
    fn arrange_shift_accumulates() {
        let order = [0, 1, 2, 3, 4];
        let r = arrange_object_index_order(&order, 1, ArrangeAction::Shift(1));
        assert_eq!(r, vec![0, 2, 1, 3, 4]);
        let r = arrange_object_index_order(&order, 1, ArrangeAction::Shift(2));
        assert_eq!(r, vec![0, 2, 3, 1, 4]);
        let r = arrange_object_index_order(&order, 1, ArrangeAction::Shift(99));
        assert_eq!(r, vec![0, 2, 3, 4, 1]);
        let r = arrange_object_index_order(&order, 2, ArrangeAction::Shift(-1));
        assert_eq!(r, vec![0, 2, 1, 3, 4]);
        let r = arrange_object_index_order(&order, 3, ArrangeAction::Shift(-99));
        assert_eq!(r, vec![3, 0, 1, 2, 4]);
        let r = arrange_object_index_order(&order, 2, ArrangeAction::BringToFront);
        assert_eq!(r, vec![0, 1, 3, 4, 2]);
        let r = arrange_object_index_order(&order, 2, ArrangeAction::SendToBack);
        assert_eq!(r, vec![2, 0, 1, 3, 4]);
    }

    #[test]
    #[ignore]
    fn surgery_text_vs_pdfium() {
        surgery_diff("text", |e| e.text = Some("xxxxxxx".into()));
    }

    #[test]
    #[ignore]
    fn surgery_arrange_vs_pdfium() {
        surgery_diff("arrange", |e| e.arrange = Some(ArrangeAction::BringToFront));
    }

    #[test]
    #[ignore]
    fn surgery_send_to_back_vs_pdfium() {
        surgery_diff("send-to-back", |e| e.arrange = Some(ArrangeAction::SendToBack));
    }

    #[test]
    #[ignore]
    fn surgery_translate_x_minus_86() {
        surgery_diff("translate-x-minus-86", |e| {
            e.dx = -86.0;
        });
    }

    #[test]
    #[ignore]
    fn surgery_arrange_plus_translate_no_double() {
        surgery_diff("arrange+translate", |e| {
            e.dx = 20.0;
            e.arrange = Some(ArrangeAction::BringToFront);
        });
    }

    #[test]
    #[ignore]
    fn surgery_delete_vs_pdfium() {
        surgery_diff("delete", |e| e.delete = true);
    }

    fn surgery_diff(label: &str, mut configure: impl FnMut(&mut ObjectEdit)) {
        let Ok(path) = std::env::var("PAPER_TEST_PDF") else {
            eprintln!("set PAPER_TEST_PDF to run this");
            return;
        };
        let page_index: usize = std::env::var("PAPER_TEST_PAGE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let target: usize = std::env::var("PAPER_TEST_OBJECT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let render = |doc: &PdfDocument<'_>| -> Vec<u8> {
            let page = doc.pages().get(page_index as u16).expect("page");
            let cfg = PdfRenderConfig::new().set_target_width(800).set_maximum_height(12000);
            let out = page
                .render_with_config(&cfg)
                .expect("render")
                .as_image()
                .into_rgb8()
                .into_raw();
            out
        };
        let baseline = {
            let doc = pdfium.load_pdf_from_file(&path, None).expect("load");
            render(&doc)
        };
        let plan = {
            let doc = pdfium.load_pdf_from_file(&path, None).expect("load");
            let mut e = ObjectEdit::new(page_index, target);
            configure(&mut e);
            let mut by_page = HashMap::new();
            by_page.insert(page_index, vec![e]);
            plan_edits(&doc, by_page)
        };
        let after_surgery = {
            let encoded = encode_text_edits_via_pdfium(
                &pdfium,
                std::path::Path::new(&path),
                &plan.surgical,
            );
            let mut lopdf_doc = lopdf::Document::load(&path).expect("lopdf load");
            apply_surgical_edits_to_doc_with_encoded(&mut lopdf_doc, &plan.surgical, &encoded)
                .expect("surgery");
            let mut bytes = Vec::new();
            lopdf_doc.save_to(&mut bytes).expect("save");
            let doc = pdfium.load_pdf_from_byte_vec(bytes, None).expect("reload");
            render(&doc)
        };
        let n = baseline.len() / 3;
        let count_changed = |a: &[u8], b: &[u8]| -> usize {
            let m = a.len().min(b.len()) / 3;
            (0..m)
                .filter(|i| {
                    (0..3).any(|c| (a[i * 3 + c] as i16 - b[i * 3 + c] as i16).abs() > 16)
                })
                .count()
        };
        let count_blackened = |a: &[u8], b: &[u8]| -> usize {
            let m = a.len().min(b.len()) / 3;
            (0..m)
                .filter(|i| {
                    let was_dark = a[i * 3] < 32 && a[i * 3 + 1] < 32 && a[i * 3 + 2] < 32;
                    let is_dark = b[i * 3] < 32 && b[i * 3 + 1] < 32 && b[i * 3 + 2] < 32;
                    !was_dark && is_dark
                })
                .count()
        };
        let ds = count_changed(&baseline, &after_surgery);
        let blacked = count_blackened(&baseline, &after_surgery);
        let center_idx = (after_surgery.len() / 2 / 3) * 3;
        eprintln!(
            "[{label}] page {} obj #{}: surgery changed {}/{} px, {} newly-black; center pixel rgb=({},{},{})",
            page_index + 1,
            target,
            ds,
            n,
            blacked,
            after_surgery[center_idx],
            after_surgery[center_idx + 1],
            after_surgery[center_idx + 2],
        );
        let w = 800u32;
        let h = (after_surgery.len() as u32 / 3 / w) as u32;
        if let Some(img) = image::RgbImage::from_raw(w, h, after_surgery.clone()) {
            let path = std::env::temp_dir().join(format!("paper-surgery-{label}.png"));
            let _ = img.save(&path);
            eprintln!("  -> wrote {}", path.display());
        }
        if let Some(img) = image::RgbImage::from_raw(w, h, baseline.clone()) {
            let path = std::env::temp_dir().join("paper-baseline.png");
            let _ = img.save(&path);
        }
    }

    #[test]
    #[ignore]
    fn surgery_vs_pdfium() {
        let Ok(path) = std::env::var("PAPER_TEST_PDF") else {
            eprintln!("set PAPER_TEST_PDF to run this");
            return;
        };
        let page_index: usize = std::env::var("PAPER_TEST_PAGE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let target: usize = std::env::var("PAPER_TEST_OBJECT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };

        let render = |doc: &PdfDocument<'_>| -> Vec<u8> {
            let page = doc.pages().get(page_index as u16).expect("page");
            let cfg = PdfRenderConfig::new().set_target_width(800).set_maximum_height(12000);
            let out = page
                .render_with_config(&cfg)
                .expect("render")
                .as_image()
                .into_rgb8()
                .into_raw();
            out
        };

        let baseline = {
            let doc = pdfium.load_pdf_from_file(&path, None).expect("load");
            render(&doc)
        };

        let after_pdfium = {
            let mut doc = pdfium.load_pdf_from_file(&path, None).expect("load");
            let mut page = doc.pages_mut().get(page_index as u16).expect("page");
            let mut e = ObjectEdit::new(page_index, target);
            e.dx = 5.0;
            let mut edits: Vec<&ObjectEdit> = vec![&e];
            apply_object_edits_to_page(&mut page, &mut edits).expect("pdfium apply");
            drop(page);
            render(&doc)
        };

        let after_surgery = {
            let plan = {
                let doc = pdfium.load_pdf_from_file(&path, None).expect("load");
                let mut e = ObjectEdit::new(page_index, target);
                e.dx = 5.0;
                let mut by_page = HashMap::new();
                by_page.insert(page_index, vec![e]);
                plan_edits(&doc, by_page)
            };
            let mut lopdf_doc = lopdf::Document::load(&path).expect("lopdf load");
            apply_surgical_edits_to_doc(&mut lopdf_doc, &plan.surgical).expect("surgery");
            let mut bytes = Vec::new();
            lopdf_doc.save_to(&mut bytes).expect("save");
            let doc = pdfium.load_pdf_from_byte_vec(bytes, None).expect("reload");
            render(&doc)
        };

        let count_changed = |a: &[u8], b: &[u8]| -> usize {
            let n = a.len().min(b.len()) / 3;
            (0..n)
                .filter(|i| {
                    (0..3).any(|c| (a[i * 3 + c] as i16 - b[i * 3 + c] as i16).abs() > 16)
                })
                .count()
        };
        let n = baseline.len() / 3;
        let dp = count_changed(&baseline, &after_pdfium);
        let ds = count_changed(&baseline, &after_surgery);
        eprintln!("page {} object #{}: PDFium changed {}/{} px, surgery changed {}/{} px",
                  page_index + 1, target, dp, n, ds, n);
        assert!(
            ds <= dp,
            "surgery shouldn't change more pixels than PDFium regen (surgery {ds} vs pdfium {dp})"
        );
    }

    #[test]
    #[ignore]
    fn edit_render_diff() {
        let Ok(path) = std::env::var("PAPER_TEST_PDF") else {
            eprintln!("set PAPER_TEST_PDF to run this");
            return;
        };
        let page_index: usize = std::env::var("PAPER_TEST_PAGE").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let pdfium = load_pdfium().expect("pdfium");
        let target: usize = std::env::var("PAPER_TEST_OBJECT").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let kind = std::env::var("PAPER_TEST_EDIT").unwrap_or_else(|_| "move".into());
        let render = |edit_it: bool| -> Vec<u8> {
            let mut doc = pdfium.load_pdf_from_file(&path, None).expect("load");
            if edit_it {
                let mut page = doc.pages_mut().get(page_index as u16).expect("page");
                let mut e = ObjectEdit::new(page_index, target);
                match kind.as_str() {
                    "text" => e.text = Some("Hello".into()),
                    "color" => e.fill_color = Some([200, 0, 0]),
                    _ => e.dx = 5.0,
                }
                let mut edits: Vec<&ObjectEdit> = vec![&e];
                apply_object_edits_to_page(&mut page, &mut edits).expect("apply");
            }
            let page = doc.pages().get(page_index as u16).expect("page");
            let cfg = PdfRenderConfig::new().set_target_width(420).set_maximum_height(8000);
            let out = page.render_with_config(&cfg).expect("render").as_image().into_rgb8().into_raw();
            out
        };
        let a = render(false);
        let b = render(true);
        let n = a.len().min(b.len()) / 3;
        let changed = (0..n)
            .filter(|i| (0..3).any(|c| (a[i * 3 + c] as i16 - b[i * 3 + c] as i16).abs() > 16))
            .count();
        eprintln!("page {}: {changed}/{n} px changed by a small object edit", page_index + 1);
    }

    fn op_f32(o: &Object) -> Option<f32> {
        match o {
            Object::Integer(v) => Some(*v as f32),
            Object::Real(v) => Some(*v),
            _ => None,
        }
    }

    #[test]
    #[ignore]
    fn load_page_text_smoke() {
        let Ok(path) = std::env::var("PAPER_TEST_PDF") else {
            eprintln!("set PAPER_TEST_PDF to run this");
            return;
        };
        let page_index: usize = std::env::var("PAPER_TEST_PAGE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let doc = pdfium.load_pdf_from_file(&path, None).expect("load");
        let page = doc.pages().get(page_index as u16).expect("page");
        let vb = visible_box(&page);
        let (vleft, vtop) = (vb.left().value, vb.top().value);
        let text_page = page.text().expect("text-page");
        let mut chars: Vec<PageTextChar> = Vec::new();
        for ch in text_page.chars().iter() {
            let unicode = match ch.unicode_char() {
                Some(c) if !c.is_control() => c,
                _ => continue,
            };
            if let Ok(b) = ch.loose_bounds() {
                let x = b.left().value - vleft;
                let y = vtop - b.top().value;
                let width = (b.right().value - b.left().value).max(0.0);
                let height = (b.top().value - b.bottom().value).max(0.0);
                if width >= 0.1 && height >= 0.1 {
                    chars.push(PageTextChar { ch: unicode, x, y, width, height });
                }
            }
        }
        eprintln!("page {}: {} characters", page_index, chars.len());
        let first_few: String = chars.iter().take(80).map(|c| c.ch).collect();
        eprintln!("  first 80 chars: {first_few:?}");
        if chars.is_empty() {
            return;
        }
        let max_x = chars.iter().map(|c| c.x + c.width).fold(0.0_f32, f32::max);
        let max_y = chars.iter().map(|c| c.y + c.height).fold(0.0_f32, f32::max);
        let rect = (0.0_f32, 0.0_f32, max_x + 1.0, max_y * 0.5 + 1.0);
        let mut selected = String::new();
        for c in &chars {
            let intersects = c.x < rect.0 + rect.2
                && c.x + c.width > rect.0
                && c.y < rect.1 + rect.3
                && c.y + c.height > rect.1;
            if intersects {
                selected.push(c.ch);
            }
        }
        eprintln!("  selected ({} chars): {selected:?}", selected.chars().count());
        assert!(!selected.is_empty(), "expected text in the top half of the page");
    }

    #[test]
    fn overlay_export_respects_page_box_origin() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        doc.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![20.into(), 30.into(), 420.into(), 630.into()],
                "Resources" => dictionary! {},
                "Contents" => content_id,
            }
            .into(),
        );
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }.into(),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let dir = std::env::temp_dir().join(format!("paper-box-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        doc.save(&input).unwrap();

        let pdfium = match load_pdfium() {
            Ok(p) => p,
            Err(_) => return,
        };
        let edit = OverlayEdit {
            page_index: 0,
            kind: OverlayKind::Text,
            x: 10.0,
            y: 15.0,
            width: 100.0,
            height: 40.0,
            ref_width: 100.0,
            ref_height: 40.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: Some("hi".into()),
            font_size: Some(18.0),
            color: Some([0, 0, 0]),
            font_family: Some("Helvetica".into()),
            font_embedded_id: None,
            image_data: None,
        };
        export_overlays(&pdfium, &input, &output, &[edit]).unwrap();

        let exported = Document::load(&output).unwrap();
        let (_, page_id) = exported.get_pages().into_iter().next().unwrap();
        let content = Content::decode(&exported.get_page_content(page_id).unwrap()).unwrap();
        let centre = content
            .operations
            .iter()
            .filter(|op| op.operator == "cm" && op.operands.len() == 6)
            .filter_map(|op| {
                let v: Vec<f32> = op.operands.iter().filter_map(op_f32).collect();
                if v.len() == 6 && v[0] == 1.0 && v[1] == 0.0 && v[2] == 0.0 && v[3] == 1.0 {
                    Some((v[4], v[5]))
                } else {
                    None
                }
            })
            .next()
            .expect("expected an identity-translate cm from append_transform");
        assert!((centre.0 - 80.0).abs() < 0.5, "x translate was {} (want 80)", centre.0);
        assert!((centre.1 - 595.0).abs() < 0.5, "y translate was {} (want 595)", centre.1);
    }
}
