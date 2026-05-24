use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use egui::{
    epaint, Align, Align2, Color32, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Shape,
    Stroke, TextureHandle, TextureOptions, Vec2,
};
use egui_phosphor::regular as ph;

use crate::pdf::{
    ArrangeAction, DocInfo, DupZOrder, EmbeddedFontInfo, Event, ObjectDetails, ObjectEdit, ObjectInfo,
    ObjectKind, OverlayEdit, OverlayKind, PageTextChar, PdfHandle, RenderPurpose,
};

const RENDER_MARGIN_SCREENS: f32 = 1.0;
const KEEP_MARGIN_SCREENS: f32 = 4.0;
const SCALE_DRIFT_TOLERANCE: f32 = 0.18;

const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
const PAGE_GAP: f32 = 16.0;
const PAGE_PAD_TOP: f32 = 8.0;

const HANDLE_PX: f32 = 8.0;
const HANDLE_HIT_PX: f32 = 10.0;
const ROTATE_OFFSET_PX: f32 = 22.0;
const MIN_EDIT_SIZE: f32 = 6.0;

const SETTINGS_KEY: &str = "paper_settings";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    default_text_font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { default_text_font_size: 18.0 }
    }
}

const FONT_FAMILIES: [&str; 3] = ["Helvetica", "Times-Roman", "Courier"];

const THUMB_W: f32 = 140.0;


struct PageView {
    texture: Option<TextureHandle>,
    tex_scale: f32,
    requested_scale: f32,
    rgba: Option<RgbaSnapshot>,
}
impl Default for PageView {
    fn default() -> Self {
        Self { texture: None, tex_scale: 0.0, requested_scale: 0.0, rgba: None }
    }
}

#[derive(Clone)]
struct RgbaSnapshot {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EyedropperTarget {
    OverlayText(usize),
    ObjectFill(usize, usize),
    ObjectStroke(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorFieldOutcome {
    Unchanged,
    Changed,
    ToggleEyedropper,
}

const MAX_RECENT_COLORS: usize = 12;

fn push_recent_color(recents: &mut VecDeque<[u8; 3]>, c: [u8; 3]) {
    if recents.front() == Some(&c) {
        return;
    }
    if let Some(pos) = recents.iter().position(|x| *x == c) {
        recents.remove(pos);
    }
    recents.push_front(c);
    while recents.len() > MAX_RECENT_COLORS {
        recents.pop_back();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Pan,
    Text,
    Image,
    Objects,
}

#[derive(Clone, Default, PartialEq)]
struct EditState {
    overlays: Vec<OverlayEdit>,
    objects: HashMap<(usize, usize), ObjectEdit>,
    dupes: Vec<ObjectEdit>,
}

enum AfterExport {
    Close,
    Open(PathBuf),
}

#[derive(Default)]
struct History {
    undo: Vec<EditState>,
    redo: Vec<EditState>,
    pending: Option<EditState>,
    navigated: bool,
}

const HISTORY_LIMIT: usize = 200;

impl History {
    fn note_change(&mut self, before: EditState) {
        if self.pending.is_none() {
            self.pending = Some(before);
        }
    }
    fn commit(&mut self, current: &EditState) {
        if let Some(p) = self.pending.take() {
            if &p != current {
                self.undo.push(p);
                if self.undo.len() > HISTORY_LIMIT {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
        }
    }
    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn undo(&mut self, current: EditState) -> Option<EditState> {
        self.pending = None;
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.navigated = true;
        Some(prev)
    }
    fn redo(&mut self, current: EditState) -> Option<EditState> {
        self.pending = None;
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.navigated = true;
        Some(next)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    Move,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Rotate,
}

struct Drag {
    edit: usize,
    handle: Handle,
    page: usize,
    orig: OverlayEdit,
    start_page_pt: Pos2,
}

#[derive(Clone)]
struct TextDrag {
    page: usize,
    start_page_pt: Pos2,
    current_page_pt: Pos2,
}

struct TextSelection {
    page: usize,
    rect: (f32, f32, f32, f32),
    chars: Vec<usize>,
}

struct ObjectDrag {
    page: usize,
    object_index: usize,
    handle: Handle,
    obj_x: f32,
    obj_y: f32,
    obj_w: f32,
    obj_h: f32,
    orig: ObjectEdit,
    start_page_pt: Pos2,
    dupe_index: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Orig(usize),
    Dupe(usize),
}

struct PagePreview {
    page: usize,
    texture: TextureHandle,
    size_px: [f32; 2],
}

pub struct PaperApp {
    engine: PdfHandle,
    doc: Option<DocInfo>,
    generation: u64,
    pages: Vec<PageView>,
    pending: HashSet<usize>,

    zoom: f32,
    fit_width_pending: bool,
    fit_jump_to_top: bool,
    pending_scroll_offset: Option<egui::Vec2>,
    pending_thumb_scroll_offset: Option<egui::Vec2>,
    pending_zoom: Option<f32>,

    edits: Vec<OverlayEdit>,
    selected: Option<usize>,
    drag: Option<Drag>,
    tool: Tool,
    history: History,
    saved_state: EditState,
    pending_export_state: Option<EditState>,
    confirm_close: bool,
    allow_close: bool,
    pending_open: Option<PathBuf>,
    pending_after_export: Option<AfterExport>,
    show_about: bool,
    about_icon: Option<TextureHandle>,
    show_settings: bool,
    settings: Settings,
    image_cache: HashMap<usize, TextureHandle>,

    show_thumbnails: bool,
    thumbs: Vec<Option<TextureHandle>>,
    thumb_pending: HashSet<usize>,

    serif: Option<egui::FontFamily>,
    base_font_defs: egui::FontDefinitions,
    registered_embedded: HashMap<(u32, u16), egui::FontFamily>,
    embedded_fonts_registered_gen: Option<u64>,
    preview: Option<PagePreview>,
    preview_pending: bool,
    preview_zoom: f32,
    preview_pan: egui::Vec2,

    objects: HashMap<usize, Vec<ObjectInfo>>,
    objects_requested: HashSet<usize>,
    unsafe_object_pages: HashSet<usize>,
    selected_object: Option<(usize, usize)>,
    selected_dupe: Option<usize>,
    object_details: Option<(usize, usize, ObjectDetails)>,
    object_edits: HashMap<(usize, usize), ObjectEdit>,
    object_dupes: Vec<ObjectEdit>,
    object_edits_dirty: bool,
    pages_with_object_edits: HashSet<usize>,
    object_drag: Option<ObjectDrag>,

    text_editing: Option<usize>,
    text_editing_just_started: bool,

    text_drag: Option<TextDrag>,
    text_selection: Option<TextSelection>,
    page_text: HashMap<usize, Vec<PageTextChar>>,
    page_text_requested: HashSet<usize>,

    current_page: usize,
    scroll_to_page: Option<usize>,

    link_object: bool,
    link_image: bool,
    link_text: bool,
    unit_pct_overlay: bool,
    unit_pct_object: bool,

    recent_colors: VecDeque<[u8; 3]>,
    eyedropper: Option<EyedropperTarget>,

    status: String,
    error: Option<String>,
}

impl PaperApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (base_font_defs, serif) = setup_fonts(&cc.egui_ctx);
        let settings = cc
            .storage
            .and_then(|s| eframe::get_value::<Settings>(s, SETTINGS_KEY))
            .unwrap_or_default();
        let wake_ctx = cc.egui_ctx.clone();
        let mut app = Self {
            engine: PdfHandle::spawn(Box::new(move || wake_ctx.request_repaint())),
            doc: None,
            generation: 0,
            pages: Vec::new(),
            pending: HashSet::new(),
            zoom: 1.0,
            fit_width_pending: false,
            fit_jump_to_top: false,
            pending_scroll_offset: None,
            pending_thumb_scroll_offset: None,
            pending_zoom: None,
            edits: Vec::new(),
            selected: None,
            drag: None,
            tool: Tool::Select,
            history: History::default(),
            saved_state: EditState::default(),
            pending_export_state: None,
            confirm_close: false,
            allow_close: false,
            pending_open: None,
            pending_after_export: None,
            show_about: false,
            about_icon: None,
            show_settings: false,
            settings,
            image_cache: HashMap::new(),
            show_thumbnails: true,
            thumbs: Vec::new(),
            thumb_pending: HashSet::new(),
            serif,
            base_font_defs,
            registered_embedded: HashMap::new(),
            embedded_fonts_registered_gen: None,
            preview: None,
            preview_pending: false,
            preview_zoom: 1.0,
            preview_pan: egui::Vec2::ZERO,
            objects: HashMap::new(),
            objects_requested: HashSet::new(),
            unsafe_object_pages: HashSet::new(),
            selected_object: None,
            selected_dupe: None,
            object_details: None,
            object_edits: HashMap::new(),
            object_dupes: Vec::new(),
            object_edits_dirty: false,
            pages_with_object_edits: HashSet::new(),
            text_editing: None,
            text_editing_just_started: false,
            text_drag: None,
            text_selection: None,
            page_text: HashMap::new(),
            page_text_requested: HashSet::new(),
            object_drag: None,
            current_page: 0,
            scroll_to_page: None,
            link_object: true,
            link_image: true,
            link_text: false,
            unit_pct_overlay: false,
            unit_pct_object: false,
            recent_colors: VecDeque::new(),
            eyedropper: None,
            status: "Open a PDF to begin.".to_string(),
            error: None,
        };
        if let Some(path) = std::env::args().nth(1).map(PathBuf::from) {
            if path.exists() {
                app.open_path(path);
            }
        }
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        self.status = format!("Opening {}…", path.display());
        self.error = None;
        self.engine.open(path);
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file() {
            self.request_open(path);
        }
    }

    fn request_preview(&mut self) {
        let object_edits = self.all_object_edits();
        let Some(doc) = &self.doc else { return };
        const TARGET_PX: f32 = 2200.0;
        let page_w_pt = doc
            .pages
            .get(self.current_page)
            .map(|p| p.width)
            .unwrap_or(595.0)
            .max(1.0);
        let scale = (TARGET_PX / page_w_pt).clamp(2.0, 6.0);
        let path = doc.path.clone();
        self.preview_pending = true;
        self.engine.render_preview(
            path,
            self.current_page,
            scale,
            self.generation,
            self.edits.clone(),
            object_edits,
        );
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        if self.preview.is_none() && !self.preview_pending {
            return;
        }
        const MIN: f32 = 0.1;
        const MAX: f32 = 8.0;
        let mut open = true;
        let mut refresh = false;
        egui::Window::new("Page preview")
            .open(&mut open)
            .default_size([560.0, 680.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(p) = &self.preview {
                        ui.label(format!("Page {} (exported result)", p.page + 1));
                    } else {
                        ui.label("Rendering…");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.preview_pending,
                                egui::Button::new(format!("{}  Refresh", ph::ARROWS_CLOCKWISE)),
                            )
                            .on_hover_text("Re-render the current page with the latest edits")
                            .clicked()
                        {
                            refresh = true;
                        }
                        if self.preview_pending {
                            ui.spinner();
                        }
                        ui.separator();
                        if ui.button(ph::MAGNIFYING_GLASS_PLUS).on_hover_text("Zoom in").clicked() {
                            self.preview_zoom = (self.preview_zoom * 1.25).clamp(MIN, MAX);
                        }
                        let mut pct = self.preview_zoom * 100.0;
                        if ui
                            .add(
                                egui::DragValue::new(&mut pct)
                                    .speed(1.0)
                                    .range(MIN * 100.0..=MAX * 100.0)
                                    .suffix("%"),
                            )
                            .changed()
                        {
                            self.preview_zoom = (pct / 100.0).clamp(MIN, MAX);
                        }
                        if ui.button(ph::MAGNIFYING_GLASS_MINUS).on_hover_text("Zoom out").clicked() {
                            self.preview_zoom = (self.preview_zoom / 1.25).clamp(MIN, MAX);
                        }
                        if ui.button(format!("{}  Fit", ph::CORNERS_OUT)).on_hover_text("Fit to width").clicked() {
                            self.preview_zoom = 1.0;
                            self.preview_pan = egui::Vec2::ZERO;
                        }
                    });
                });
                ui.separator();
                if self.preview.is_some() {
                    let avail = ui.available_size();
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(avail.x.max(64.0), avail.y.max(64.0)),
                        egui::Sense::click_and_drag(),
                    );
                    let p = self.preview.as_ref().unwrap();
                    let aspect = p.size_px[1] / p.size_px[0].max(1.0);
                    let fit_w = rect.width();

                    if resp.hovered() {
                        let scroll_y = ctx.input(|i| i.raw_scroll_delta.y);
                        if scroll_y.abs() > 0.0 {
                            let old = self.preview_zoom;
                            let new = (old * (scroll_y * 0.0015).exp()).clamp(MIN, MAX);
                            if (new - old).abs() > f32::EPSILON {
                                if let Some(cursor) = ctx.input(|i| i.pointer.hover_pos()) {
                                    let center = rect.center() + self.preview_pan;
                                    let rel = cursor - center;
                                    let k = new / old;
                                    self.preview_pan -= rel * (k - 1.0);
                                }
                                self.preview_zoom = new;
                            }
                        }
                    }
                    if resp.dragged() {
                        self.preview_pan += resp.drag_delta();
                        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                    } else if resp.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::Grab);
                    }

                    let disp_w = (fit_w * self.preview_zoom).max(1.0);
                    let disp_h = disp_w * aspect;
                    let center = rect.center() + self.preview_pan;
                    let img_rect = egui::Rect::from_center_size(center, egui::vec2(disp_w, disp_h));
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
                    painter.image(
                        p.texture.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.spinner();
                        ui.label("Rendering the page through the export pipeline…");
                    });
                    ui.add_space(40.0);
                }
            });
        if !open {
            self.preview = None;
            self.preview_pending = false;
        } else if refresh {
            self.request_preview();
        }
    }

    fn export_dialog(&mut self) -> bool {
        let object_edits = self.all_object_edits();
        let Some(doc) = &self.doc else { return false };
        let stem = doc.path.file_stem().and_then(|s| s.to_str()).unwrap_or("document");
        if let Some(out) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-edited.pdf"))
            .save_file()
        {
            let path = doc.path.clone();
            self.status = format!("Exporting to {}…", out.display());
            self.pending_export_state = Some(self.edit_state());
            self.engine.export(path, out, self.edits.clone(), object_edits);
            true
        } else {
            false
        }
    }

    fn request_open(&mut self, path: PathBuf) {
        if self.has_unexported_changes() {
            self.pending_open = Some(path);
        } else {
            self.open_path(path);
        }
    }

    fn ensure_embedded_fonts_registered(&mut self, ctx: &egui::Context) {
        if self.embedded_fonts_registered_gen == Some(self.generation) {
            return;
        }
        self.registered_embedded.clear();
        let fonts = self.doc.as_ref().map(|d| d.fonts.clone()).unwrap_or_default();
        let loadable: Vec<_> = fonts.iter().filter(|f| f.program.is_some()).collect();
        if loadable.is_empty() {
            self.embedded_fonts_registered_gen = Some(self.generation);
            return;
        }
        let mut defs = self.base_font_defs.clone();
        for f in loadable {
            let Some(program) = &f.program else { continue };
            if ab_glyph::FontVec::try_from_vec(program.as_ref().clone()).is_err() {
                continue;
            }
            let key = format!("emb-{}-{}", f.id.0, f.id.1);
            defs.font_data.insert(
                key.clone(),
                egui::FontData::from_owned(program.as_ref().clone()),
            );
            let family = egui::FontFamily::Name(key.clone().into());
            defs.families.insert(family.clone(), vec![key.clone()]);
            self.registered_embedded.insert(f.id, family);
        }
        ctx.set_fonts(defs);
        self.embedded_fonts_registered_gen = Some(self.generation);
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        for ev in self.engine.drain() {
            match ev {
                Event::Opened(info) => {
                    self.generation += 1;
                    self.pages = (0..info.page_count()).map(|_| PageView::default()).collect();
                    self.thumbs = (0..info.page_count()).map(|_| None).collect();
                    self.thumb_pending.clear();
                    self.pending.clear();
                    self.edits.clear();
                    self.selected = None;
                    self.drag = None;
                    self.history = History::default();
                    self.saved_state = EditState::default();
                    self.pending_export_state = None;
                    self.confirm_close = false;
                    self.pending_open = None;
                    self.pending_after_export = None;
                    self.image_cache.clear();
                    self.objects.clear();
                    self.objects_requested.clear();
                    self.unsafe_object_pages.clear();
                    self.selected_object = None;
                    self.selected_dupe = None;
                    self.object_details = None;
                    self.object_edits.clear();
                    self.object_dupes.clear();
                    self.object_edits_dirty = false;
                    self.pages_with_object_edits.clear();
                    self.object_drag = None;
                    self.text_drag = None;
                    self.text_selection = None;
                    self.page_text.clear();
                    self.page_text_requested.clear();
                    self.text_editing = None;
                    self.text_editing_just_started = false;
                    self.eyedropper = None;
                    self.preview = None;
                    self.preview_pending = false;
                    self.status = format!(
                        "{} ({} page{}, {})",
                        info.file_name,
                        info.page_count(),
                        if info.page_count() == 1 { "" } else { "s" },
                        human_size(info.file_size),
                    );
                    self.doc = Some(info);
                    self.fit_width_pending = true;
                    self.fit_jump_to_top = true;
                    self.current_page = 0;
                    self.scroll_to_page = None;
                    self.pending_zoom = None;
                    self.pending_scroll_offset = Some(Vec2::ZERO);
                    self.pending_thumb_scroll_offset = Some(Vec2::ZERO);
                    ctx.request_repaint();
                }
                Event::Rendered(page) => {
                    if page.generation != self.generation {
                        continue;
                    }
                    let img = ColorImage::from_rgba_unmultiplied(
                        [page.width_px as usize, page.height_px as usize],
                        &page.rgba,
                    );
                    match page.purpose {
                        RenderPurpose::Page => {
                            self.pending.remove(&page.index);
                            let width_px = page.width_px;
                            let height_px = page.height_px;
                            let rgba = page.rgba;
                            if let Some(slot) = self.pages.get_mut(page.index) {
                                slot.texture =
                                    Some(ctx.load_texture(format!("page-{}", page.index), img, TextureOptions::LINEAR));
                                slot.tex_scale = page.scale;
                                slot.rgba = Some(RgbaSnapshot {
                                    width: width_px,
                                    height: height_px,
                                    data: rgba,
                                });
                            }
                        }
                        RenderPurpose::Thumbnail => {
                            self.thumb_pending.remove(&page.index);
                            if let Some(slot) = self.thumbs.get_mut(page.index) {
                                *slot = Some(ctx.load_texture(
                                    format!("thumb-{}", page.index),
                                    img,
                                    TextureOptions::LINEAR,
                                ));
                            }
                        }
                        RenderPurpose::Preview => {
                            self.preview_pending = false;
                            let same_page = self.preview.as_ref().map_or(false, |p| p.page == page.index);
                            if !same_page {
                                self.preview_zoom = 1.0;
                                self.preview_pan = egui::Vec2::ZERO;
                            }
                            let preview_opts = TextureOptions {
                                magnification: egui::TextureFilter::Linear,
                                minification: egui::TextureFilter::Linear,
                                wrap_mode: egui::TextureWrapMode::ClampToEdge,
                                mipmap_mode: Some(egui::TextureFilter::Linear),
                            };
                            self.preview = Some(PagePreview {
                                page: page.index,
                                texture: ctx.load_texture(
                                    format!("preview-{}", page.index),
                                    img,
                                    preview_opts,
                                ),
                                size_px: [page.width_px as f32, page.height_px as f32],
                            });
                        }
                    }
                    ctx.request_repaint();
                }
                Event::Objects { page, generation, objects, safe_to_edit } => {
                    if generation == self.generation {
                        self.objects.insert(page, objects);
                        if safe_to_edit {
                            self.unsafe_object_pages.remove(&page);
                        } else {
                            self.unsafe_object_pages.insert(page);
                        }
                        ctx.request_repaint();
                    }
                }
                Event::ObjectDetails { page, object_index, generation, details } => {
                    if generation == self.generation {
                        self.object_details = Some((page, object_index, details));
                        ctx.request_repaint();
                    }
                }
                Event::Exported(path) => {
                    self.status = format!("Exported {}", path.display());
                    self.saved_state =
                        self.pending_export_state.take().unwrap_or_else(|| self.edit_state());
                    match self.pending_after_export.take() {
                        Some(AfterExport::Close) => {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Some(AfterExport::Open(p)) => self.open_path(p),
                        None => {}
                    }
                }
                Event::ObjectImageExported(path) => {
                    self.status = format!("Exported image to {}", path.display());
                }
                Event::PageText { page, generation, chars } => {
                    if generation == self.generation {
                        self.page_text.insert(page, chars);
                        self.page_text_requested.remove(&page);
                        self.recompute_text_selection();
                        ctx.request_repaint();
                    }
                }
                Event::Error(msg) => {
                    self.error = Some(msg.clone());
                    self.status = msg;
                }
            }
        }
    }


    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(format!("{}  Open", ph::FOLDER_OPEN)).clicked() {
                    self.open_dialog();
                }
                ui.add_enabled_ui(self.doc.is_some(), |ui| {
                    ui.toggle_value(&mut self.show_thumbnails, format!("{}  Pages", ph::LIST))
                        .on_hover_text("Toggle the page thumbnail sidebar");
                    ui.separator();
                    if ui.button(ph::MAGNIFYING_GLASS_MINUS).on_hover_text("Zoom out").clicked() {
                        self.pending_zoom = Some((self.zoom / 1.25).clamp(MIN_ZOOM, MAX_ZOOM));
                    }
                    let mut zoom_pct = self.zoom * 100.0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut zoom_pct)
                            .range((MIN_ZOOM * 100.0)..=(MAX_ZOOM * 100.0))
                            .speed(1.0)
                            .suffix("%")
                            .max_decimals(0)
                            .min_decimals(0),
                    )
                    .on_hover_text(format!(
                        "Zoom: click to type, or drag (range {}% to {}%)",
                        (MIN_ZOOM * 100.0) as i32,
                        (MAX_ZOOM * 100.0) as i32,
                    ));
                    if resp.changed() {
                        self.pending_zoom = Some((zoom_pct / 100.0).clamp(MIN_ZOOM, MAX_ZOOM));
                    }
                    if ui.button(ph::MAGNIFYING_GLASS_PLUS).on_hover_text("Zoom in").clicked() {
                        self.pending_zoom = Some((self.zoom * 1.25).clamp(MIN_ZOOM, MAX_ZOOM));
                    }
                    if ui.button(format!("{}  Fit", ph::ARROWS_OUT_LINE_HORIZONTAL)).on_hover_text("Fit page width").clicked() {
                        self.fit_width_pending = true;
                    }
                    ui.separator();
                    if ui.add_enabled(self.history.can_undo(), egui::Button::new(ph::ARROW_COUNTER_CLOCKWISE)).on_hover_text("Undo (Ctrl+Z)").clicked() {
                        self.do_undo();
                    }
                    if ui.add_enabled(self.history.can_redo(), egui::Button::new(ph::ARROW_CLOCKWISE)).on_hover_text("Redo (Ctrl+Shift+Z / Ctrl+Y)").clicked() {
                        self.do_redo();
                    }
                    ui.separator();
                    ui.selectable_value(&mut self.tool, Tool::Select, format!("{}  Select", ph::CURSOR)).on_hover_text("Select / move / resize edits");
                    ui.selectable_value(&mut self.tool, Tool::Pan, format!("{}  Pan", ph::HAND)).on_hover_text("Drag the page to scroll");
                    ui.selectable_value(&mut self.tool, Tool::Text, format!("{}  Text", ph::TEXT_T)).on_hover_text("Click a page to drop a text block");
                    ui.selectable_value(&mut self.tool, Tool::Image, format!("{}  Image", ph::IMAGE)).on_hover_text("Click a page to place an image file");
                    ui.selectable_value(&mut self.tool, Tool::Objects, format!("{}  Objects", ph::BOUNDING_BOX)).on_hover_text("Inspect & edit the PDF's own page objects");
                    ui.separator();
                    if let Some(doc) = &self.doc {
                        let n = doc.page_count().max(1);
                        if ui.button(ph::CARET_LEFT).on_hover_text("Previous page").clicked() {
                            self.scroll_to_page = Some(self.current_page.saturating_sub(1));
                        }
                        let mut p = self.current_page + 1;
                        let resp = ui.add(
                            egui::DragValue::new(&mut p)
                                .range(1..=n)
                                .speed(0.2)
                                .suffix(format!(" / {n}")),
                        );
                        if resp.changed() {
                            self.scroll_to_page = Some(p.saturating_sub(1));
                        }
                        if ui.button(ph::CARET_RIGHT).on_hover_text("Next page").clicked() {
                            self.scroll_to_page = Some((self.current_page + 1).min(n - 1));
                        }
                    }
                    ui.separator();
                    let has_doc = self.doc.is_some();
                    let preview_label = if self.preview_pending {
                        format!("{}  Rendering…", ph::EYE)
                    } else {
                        format!("{}  Preview page", ph::EYE)
                    };
                    if ui
                        .add_enabled(has_doc && !self.preview_pending, egui::Button::new(preview_label))
                        .on_hover_text("Render the current page exactly as it will export (true fonts, baked overlays)")
                        .clicked()
                    {
                        self.request_preview();
                    }
                    ui.separator();
                    let can_export = !self.edits.is_empty()
                        || self.object_edits.values().any(|e| !e.is_noop())
                        || !self.object_dupes.is_empty();
                    if ui
                        .add_enabled(can_export, egui::Button::new(format!("{}  Export…", ph::EXPORT)))
                        .on_hover_text(if can_export {
                            "Write a flattened copy with your edits baked in"
                        } else {
                            "Add an overlay or edit a page object first"
                        })
                        .clicked()
                    {
                        self.export_dialog();
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button(ph::INFO)
                            .on_hover_text("About Paper")
                            .clicked()
                        {
                            self.show_about = true;
                        }
                        if ui
                            .button(ph::GEAR)
                            .on_hover_text("Settings")
                            .clicked()
                        {
                            self.show_settings = true;
                        }
                    });
                });
                self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
            });
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(err) = self.error.clone() {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), err);
                    if ui.small_button(ph::X).clicked() {
                        self.error = None;
                    }
                } else {
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if !self.pending.is_empty() {
                        ui.spinner();
                        ui.label(format!("rendering {}…", self.pending.len()));
                    }
                });
            });
        });
    }

    fn thumbnail_panel(&mut self, ctx: &egui::Context) {
        if !self.show_thumbnails {
            if self.thumbs.iter().any(Option::is_some) {
                self.thumbs.iter_mut().for_each(|t| *t = None);
                self.thumb_pending.clear();
            }
            return;
        }
        let Some(doc) = self.doc.clone() else { return };
        egui::SidePanel::left("thumbnails")
            .resizable(true)
            .default_width(THUMB_W + 36.0)
            .width_range((THUMB_W + 22.0)..=(THUMB_W + 180.0))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Pages");
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{}", doc.page_count()));
                    });
                });
                ui.separator();
                let ppp = ctx.pixels_per_point();
                let mut thumb_scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(off) = self.pending_thumb_scroll_offset.take() {
                    thumb_scroll = thumb_scroll.scroll_offset(off);
                }
                thumb_scroll.show(ui, |ui| {
                    let viewport = ui.clip_rect();
                    let render_band = viewport.expand2(Vec2::new(0.0, viewport.height()));
                    let keep_band = viewport.expand2(Vec2::new(0.0, viewport.height() * 4.0));
                    let mut goto: Option<usize> = None;
                    ui.vertical_centered(|ui| {
                        for (i, page) in doc.pages.iter().enumerate() {
                            ui.add_space(6.0);
                            let h = THUMB_W * (page.height / page.width).clamp(0.05, 20.0);
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(THUMB_W, h), Sense::click());
                            let painter = ui.painter_at(rect);

                            if render_band.intersects(rect) {
                                if self.thumbs[i].is_none() && !self.thumb_pending.contains(&i) {
                                    let scale = (THUMB_W * ppp / page.width).clamp(0.02, 4.0);
                                    self.engine.request_render(i, scale, self.generation, RenderPurpose::Thumbnail);
                                    self.thumb_pending.insert(i);
                                }
                            } else if !keep_band.intersects(rect) {
                                self.thumbs[i] = None;
                            }

                            if let Some(tex) = &self.thumbs[i] {
                                painter.image(
                                    tex.id(),
                                    rect,
                                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
                                painter.rect_filled(rect, 0.0, Color32::from_gray(240));
                            }
                            let border = if i == self.current_page {
                                Stroke::new(2.0, Color32::from_rgb(40, 110, 220))
                            } else if resp.hovered() {
                                Stroke::new(1.5, Color32::from_gray(120))
                            } else {
                                Stroke::new(1.0, Color32::from_gray(185))
                            };
                            painter.rect_stroke(rect, 0.0, border);
                            ui.small(format!("{}", i + 1));
                            if resp.clicked() {
                                goto = Some(i);
                            }
                        }
                        ui.add_space(8.0);
                    });
                    if let Some(i) = goto {
                        self.scroll_to_page = Some(i);
                    }
                });
            });
    }

    fn properties_panel(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.selected else { return };
        if idx >= self.edits.len() {
            self.selected = None;
            return;
        }
        let mut delete_me = false;
        let embedded_fonts = self.doc.as_ref().map(|d| d.fonts.clone()).unwrap_or_default();
        let registered_fonts = self.registered_embedded.clone();
        let doc_gen = self.generation;
        egui::SidePanel::right("props").resizable(true).default_width(264.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
            let recents = &mut self.recent_colors;
            let eyedropper = &mut self.eyedropper;
            let edit = &mut self.edits[idx];
            ui.heading(match edit.kind {
                OverlayKind::Text => "Text block",
                OverlayKind::Image => "Image block",
            });
            ui.label(format!("on page {}", edit.page_index + 1));
            ui.separator();

            egui::Grid::new("edit-pos").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("X");
                ui.add(egui::DragValue::new(&mut edit.x).speed(0.5).suffix(" pt"));
                ui.end_row();
                ui.label("Y");
                ui.add(egui::DragValue::new(&mut edit.y).speed(0.5).suffix(" pt"));
                ui.end_row();
            });
            let link_field: &mut bool = match edit.kind {
                OverlayKind::Image => &mut self.link_image,
                OverlayKind::Text => &mut self.link_text,
            };
            ui.horizontal(|ui| {
                ui.label("Size");
                ui.selectable_value(&mut self.unit_pct_overlay, false, "pt");
                ui.selectable_value(&mut self.unit_pct_overlay, true, "%");
                ui.add_space(6.0);
                let icon = if *link_field { ph::LINK } else { ph::LINK_BREAK };
                ui.toggle_value(link_field, icon)
                    .on_hover_text("Keep width / height proportional");
            });
            let linked = *link_field;
            edit_size_row(
                ui,
                "edit-size",
                &mut edit.width,
                &mut edit.height,
                edit.ref_width.max(1.0),
                edit.ref_height.max(1.0),
                self.unit_pct_overlay,
                linked,
            );
            egui::Grid::new("edit-rot-flip").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Rotation");
                ui.add(egui::DragValue::new(&mut edit.rotation).speed(0.5).suffix("°"));
                ui.end_row();
                ui.label("Flip");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut edit.flip_horizontal, format!("{} H", ph::FLIP_HORIZONTAL));
                    ui.checkbox(&mut edit.flip_vertical, format!("{} V", ph::FLIP_VERTICAL));
                });
                ui.end_row();
            });
            ui.separator();

            match edit.kind {
                OverlayKind::Text => {
                    ui.label("Text");
                    let mut text = edit.text.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2)).changed() {
                        edit.text = Some(text);
                    }
                    ui.add_space(6.0);
                    let fallback_active = edit
                        .text
                        .as_deref()
                        .map(|t| {
                            matches!(
                                crate::pdf::overlay_font_advice(t),
                                crate::pdf::OverlayFontAdvice::Unicode { .. }
                            )
                        })
                        .unwrap_or(false);
                    egui::Grid::new("edit-text").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                        ui.label("Font size");
                        let mut fs = edit.font_size.unwrap_or(18.0);
                        if ui.add(egui::DragValue::new(&mut fs).speed(0.2).range(4.0..=400.0).suffix(" pt")).changed() {
                            edit.font_size = Some(fs);
                        }
                        ui.end_row();
                        ui.label("Font");
                        if fallback_active {
                            ui.add_enabled_ui(false, |ui| {
                                egui::ComboBox::from_id_salt("font-family")
                                    .selected_text("DejaVu Sans (Unicode)")
                                    .show_ui(ui, |_| {});
                            });
                        } else {
                            let mut fam = edit.font_family.clone().unwrap_or_else(|| "Helvetica".into());
                            let using_embedded = edit.font_embedded_id.is_some();
                            let shown = if using_embedded { "(document font)".to_string() } else { fam.clone() };
                            egui::ComboBox::from_id_salt("font-family").selected_text(shown).show_ui(ui, |ui| {
                                for f in FONT_FAMILIES {
                                    if ui.selectable_label(!using_embedded && fam == f, f).clicked() {
                                        fam = f.to_string();
                                        edit.font_embedded_id = None;
                                    }
                                }
                            });
                            edit.font_family = Some(fam);
                        }
                        ui.end_row();
                        ui.label("Colour");
                        let mut c = edit.color.unwrap_or([17, 24, 39]);
                        let target = EyedropperTarget::OverlayText(idx);
                        let active = *eyedropper == Some(target);
                        match color_field(ui, &mut c, recents, active) {
                            ColorFieldOutcome::Changed => {
                                edit.color = Some(c);
                            }
                            ColorFieldOutcome::ToggleEyedropper => {
                                *eyedropper = if active { None } else { Some(target) };
                            }
                            ColorFieldOutcome::Unchanged => {}
                        }
                        ui.end_row();
                    });
                    if let Some(t) = edit.text.as_deref() {
                        if let crate::pdf::OverlayFontAdvice::Unicode { unsupported } =
                            crate::pdf::overlay_font_advice(t)
                        {
                            ui.add_space(4.0);
                            ui.small(
                                egui::RichText::new(format!(
                                    "{}  Non-Latin text is rendered with a bundled Unicode font (DejaVu Sans) on export.",
                                    ph::INFO
                                ))
                                .color(egui::Color32::from_rgb(70, 130, 180)),
                            );
                            if !unsupported.is_empty() {
                                let sample: String = unsupported.iter().take(8).collect();
                                ui.small(
                                    egui::RichText::new(format!(
                                        "⚠ These characters are not supported yet and will not appear: {sample}",
                                    ))
                                    .color(egui::Color32::from_rgb(180, 130, 30)),
                                );
                            }
                        }
                    }
                    if !embedded_fonts.is_empty() {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(egui::RichText::new("Document fonts").strong());
                        if fallback_active {
                            ui.small(
                                egui::RichText::new(format!(
                                    "{}  Not used for non-Latin text: it falls back to DejaVu Sans (these fonts can't render it yet).",
                                    ph::INFO
                                ))
                                .color(egui::Color32::from_rgb(180, 130, 30)),
                            );
                        }
                        let selected_label = match edit.font_embedded_id {
                            Some(id) => embedded_fonts
                                .iter()
                                .find(|f| f.id == id)
                                .map(|f| font_choice_label(f))
                                .unwrap_or_else(|| "(missing)".to_string()),
                            None => "(use standard font)".to_string(),
                        };
                        let sample: String = {
                            let typed: String = edit
                                .text
                                .as_deref()
                                .unwrap_or("")
                                .chars()
                                .take(5)
                                .collect();
                            if typed.trim().is_empty() {
                                "Abcde".to_string()
                            } else {
                                typed
                            }
                        };
                        let text_color = ui.visuals().text_color();
                        let weak_color = ui.visuals().weak_text_color();
                        egui::ComboBox::from_id_salt(("doc-font", doc_gen))
                            .width(240.0)
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(
                                        edit.font_embedded_id.is_none(),
                                        "(use standard font)",
                                    )
                                    .clicked()
                                {
                                    edit.font_embedded_id = None;
                                    edit.font_family = Some(FONT_FAMILIES[0].to_string());
                                }
                                for f in &embedded_fonts {
                                    let chosen = edit.font_embedded_id == Some(f.id);
                                    let mut job = egui::text::LayoutJob::default();
                                    if let Some(family) = registered_fonts.get(&f.id) {
                                        job.append(
                                            &sample,
                                            0.0,
                                            egui::TextFormat {
                                                font_id: egui::FontId::new(16.0, family.clone()),
                                                color: text_color,
                                                ..Default::default()
                                            },
                                        );
                                        job.append("   ", 0.0, egui::TextFormat::default());
                                    }
                                    job.append(
                                        &font_choice_label(f),
                                        0.0,
                                        egui::TextFormat {
                                            font_id: egui::FontId::new(
                                                13.0,
                                                egui::FontFamily::Proportional,
                                            ),
                                            color: weak_color,
                                            ..Default::default()
                                        },
                                    );
                                    if ui.selectable_label(chosen, job).clicked() {
                                        edit.font_embedded_id = Some(f.id);
                                        edit.font_family = Some(f.base_font.clone());
                                    }
                                }
                            });
                        ui.small(
                            egui::RichText::new(
                                "⚠ Embedded fonts are usually subsets. Characters the document didn't already use may show as blank boxes.",
                            )
                            .color(egui::Color32::from_rgb(180, 130, 30)),
                        );
                        if let Some(id) = edit.font_embedded_id {
                            let previewable = registered_fonts.contains_key(&id);
                            if !previewable {
                                ui.small(
                                    egui::RichText::new(format!(
                                        "{}  This font can't be drawn in the editor (Type1/CFF), so the canvas shows a generic stand-in. The export is correct; use “Preview page” to see how it really looks.",
                                        ph::INFO
                                    ))
                                    .color(egui::Color32::from_rgb(70, 130, 190)),
                                );
                            }
                            if let Some(f) = embedded_fonts.iter().find(|f| f.id == id) {
                                if !f.is_simple {
                                    ui.small(
                                        egui::RichText::new(format!(
                                            "⚠ {} is a {} font. Typing new text into it isn't supported yet and may render incorrectly.",
                                            f.base_font, f.subtype
                                        ))
                                        .color(egui::Color32::from_rgb(200, 80, 60)),
                                    );
                                }
                            }
                        }
                    }
                }
                OverlayKind::Image => {}
            }

            ui.separator();
            if ui.button(format!("{}  Delete", ph::TRASH)).clicked() {
                delete_me = true;
            }
            ui.add_space(4.0);
            ui.small("Tip: drag the box to move, corner squares to resize, the circle to rotate. Delete key removes it.");
            });
        });

        if delete_me {
            self.edits.remove(idx);
            self.selected = None;
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        if self.tool == Tool::Pan {
            let cursor = if ctx.input(|i| i.pointer.primary_down()) {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            };
            ctx.set_cursor_icon(cursor);
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            let (esc, del, undo, redo, nudge) = ctx.input(|i| {
                let cmd = i.modifiers.command || i.modifiers.ctrl;
                let undo = cmd && !i.modifiers.shift && i.key_pressed(Key::Z);
                let redo = cmd && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z)));
                let step = if i.modifiers.shift { 10.0 } else { 1.0 };
                let mut nudge = Vec2::ZERO;
                if i.key_pressed(Key::ArrowLeft) { nudge.x -= step; }
                if i.key_pressed(Key::ArrowRight) { nudge.x += step; }
                if i.key_pressed(Key::ArrowUp) { nudge.y -= step; }
                if i.key_pressed(Key::ArrowDown) { nudge.y += step; }
                (i.key_pressed(Key::Escape), i.key_pressed(Key::Delete), undo, redo, nudge)
            });
            if esc {
                if self.eyedropper.is_some() {
                    self.eyedropper = None;
                    self.status = "Eyedropper cancelled.".into();
                } else {
                    self.selected = None;
                    self.selected_object = None;
                    self.selected_dupe = None;
                    self.object_details = None;
                    self.text_selection = None;
                }
            }
            if undo {
                self.do_undo();
            }
            if redo {
                self.do_redo();
            }
            if del {
                if let Some(idx) = self.selected.take() {
                    if idx < self.edits.len() {
                        self.edits.remove(idx);
                    }
                }
                if self.tool == Tool::Objects {
                    if let Some((p, o)) = self.selected_object {
                        self.object_edits.entry((p, o)).or_insert_with(|| ObjectEdit::new(p, o)).delete = true;
                    }
                }
            }
            if nudge != Vec2::ZERO {
                if let Some(idx) = self.selected {
                    if idx < self.edits.len() {
                        self.edits[idx].x += nudge.x;
                        self.edits[idx].y += nudge.y;
                    }
                }
                if self.tool == Tool::Objects {
                    if let Some((p, o)) = self.selected_object {
                        let e = self.object_edits.entry((p, o)).or_insert_with(|| ObjectEdit::new(p, o));
                        e.dx += nudge.x;
                        e.dy += nudge.y;
                    }
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(doc) = self.doc.clone() else {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a PDF (or drag one onto the window) to start. Large scanned files render lazily as you scroll.");
                });
                return;
            };

            if self.fit_width_pending {
                let avail = ui.available_width().max(64.0) - 24.0;
                let max_w = doc.pages.iter().map(|p| p.width).fold(1.0_f32, f32::max);
                let target = (avail / max_w).clamp(MIN_ZOOM, MAX_ZOOM);
                if self.fit_jump_to_top {
                    self.zoom = target;
                    self.pending_scroll_offset = Some(Vec2::ZERO);
                    self.fit_jump_to_top = false;
                } else {
                    self.pending_zoom = Some(target);
                }
                self.fit_width_pending = false;
            }

            let wheel_zoom: Option<(f32, Option<Pos2>)> = ctx.input_mut(|i| {
                let cmd = i.modifiers.command || i.modifiers.ctrl;
                if !cmd {
                    return None;
                }
                let mut units = 0.0_f32;
                i.events.retain(|e| match e {
                    egui::Event::MouseWheel { delta, unit, .. } => {
                        let scale = match unit {
                            egui::MouseWheelUnit::Point => 1.0 / 20.0,
                            egui::MouseWheelUnit::Line => 1.0,
                            egui::MouseWheelUnit::Page => 6.0,
                        };
                        units += delta.y * scale;
                        false
                    }
                    _ => true,
                });
                if units == 0.0 {
                    units = i.raw_scroll_delta.y / 20.0;
                }
                i.raw_scroll_delta = egui::Vec2::ZERO;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                if units == 0.0 {
                    None
                } else {
                    Some((units, i.pointer.hover_pos().or(i.pointer.latest_pos())))
                }
            });
            ui.input(|i| {
                if i.modifiers.command || i.modifiers.ctrl {
                    if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                        self.pending_zoom = Some((self.zoom * 1.25).clamp(MIN_ZOOM, MAX_ZOOM));
                    }
                    if i.key_pressed(Key::Minus) {
                        self.pending_zoom = Some((self.zoom / 1.25).clamp(MIN_ZOOM, MAX_ZOOM));
                    }
                }
            });

            let ppp = ctx.pixels_per_point();
            let zoom = self.zoom;
            let interactive = self.tool != Tool::Pan;

            {
                let cache = &mut self.image_cache;
                for edit in &self.edits {
                    if edit.kind == OverlayKind::Image {
                        if let Some(data) = &edit.image_data {
                            let key = Arc::as_ptr(data) as usize;
                            cache.entry(key).or_insert_with(|| decode_image_texture(ctx, data));
                        }
                    }
                }
            }

            let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]);
            scroll = scroll.drag_to_scroll(self.tool == Tool::Pan);
            if let Some(off) = self.pending_scroll_offset.take() {
                scroll = scroll.scroll_offset(off);
            }
            let scroll_output = scroll.show(ui, |ui| {
                let viewport = ui.clip_rect();
                let render_band = viewport.expand2(Vec2::new(0.0, viewport.height() * RENDER_MARGIN_SCREENS));
                let keep_band = viewport.expand2(Vec2::new(0.0, viewport.height() * KEEP_MARGIN_SCREENS));
                let mut best_page = (0.0f32, 0usize);

                let mut new_edit: Option<(OverlayKind, usize, f32, f32)> = None;
                let mut new_image: Option<(usize, f32, f32)> = None;
                let mut new_drag: Option<Drag> = None;
                let mut new_text_focus: Option<usize> = None;
                let mut new_text_drag: Option<TextDrag> = None;
                let mut text_drag_update: Option<Pos2> = None;
                let mut text_drag_finish: bool = false;
                let mut click_select: Option<Option<usize>> = None;
                let mut click_select_object: Option<Option<(usize, usize)>> = None;
                let mut click_select_dupe: Option<Option<usize>> = None;
                let mut ctx_duplicate: Option<(usize, usize)> = None;
                let mut new_object_drag: Option<ObjectDrag> = None;
                let mut object_drag_pos: Option<Pos2> = None;
                let mut stop_object_drag = false;
                let mut stop_drag = false;
                let mut eyedropper_sample: Option<(usize, Rect, Pos2)> = None;
                let objects_mode = self.tool == Tool::Objects;
                let mut editing_page_rect: Option<Rect> = None;

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let body_w = ui.available_width();
                    ui.add_space(PAGE_PAD_TOP);
                    for (i, page) in doc.pages.iter().enumerate() {
                        let pw_zoomed = page.width * zoom;
                        let ph_zoomed = page.height * zoom;
                        let left_pad = ((body_w - pw_zoomed) * 0.5).max(0.0);
                        let row_w = pw_zoomed.max(body_w);
                        let sense = if interactive { Sense::click_and_drag() } else { Sense::hover() };
                        let (row_rect, _) = ui.allocate_exact_size(
                            Vec2::new(row_w, ph_zoomed),
                            Sense::hover(),
                        );
                        let rect = Rect::from_min_size(
                            Pos2::new(row_rect.min.x + left_pad, row_rect.min.y),
                            Vec2::new(pw_zoomed, ph_zoomed),
                        );
                        let response = ui.interact(rect, ui.id().with(("page", i)), sense);

                        let visible = (rect.bottom().min(viewport.bottom())
                            - rect.top().max(viewport.top()))
                        .max(0.0);
                        if visible > best_page.0 {
                            best_page = (visible, i);
                        }
                        if self.scroll_to_page == Some(i) {
                            ui.scroll_to_rect(rect, Some(Align::TOP));
                            self.scroll_to_page = None;
                        }

                        let visible = ui.is_rect_visible(rect);
                        let in_render_band = render_band.intersects(rect);
                        let in_keep_band = keep_band.intersects(rect);
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 0.0, Color32::WHITE);

                        {
                            let slot = &mut self.pages[i];
                            if in_render_band {
                                let desired = (zoom * ppp).max(0.05);
                                let needs = slot.tex_scale == 0.0
                                    || ((desired / slot.tex_scale) - 1.0).abs() > SCALE_DRIFT_TOLERANCE;
                                let already = self.pending.contains(&i)
                                    && ((desired / slot.requested_scale.max(1e-6)) - 1.0).abs() <= SCALE_DRIFT_TOLERANCE;
                                if needs && !already {
                                    self.engine.request_render(i, desired, self.generation, RenderPurpose::Page);
                                    self.pending.insert(i);
                                    slot.requested_scale = desired;
                                }
                            } else if !in_keep_band {
                                slot.texture = None;
                                slot.tex_scale = 0.0;
                                slot.rgba = None;
                            }
                            if let Some(tex) = &slot.texture {
                                painter.image(
                                    tex.id(),
                                    rect,
                                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else if visible {
                                painter.rect_filled(rect, 0.0, Color32::from_gray(238));
                                painter.text(
                                    rect.center(),
                                    Align2::CENTER_CENTER,
                                    format!("Page {}", i + 1),
                                    FontId::proportional(16.0),
                                    Color32::from_gray(140),
                                );
                            }
                        }

                        if visible {
                            for (ei, edit) in self.edits.iter().enumerate() {
                                if edit.page_index != i {
                                    continue;
                                }
                                if Some(ei) == self.text_editing {
                                    continue;
                                }
                                draw_edit(ui, rect, zoom, edit, Some(ei) == self.selected, &self.image_cache, self.serif.as_ref(), &self.registered_embedded);
                            }
                        }
                        if let Some(ei) = self.text_editing {
                            if let Some(e) = self.edits.get(ei) {
                                if e.page_index == i {
                                    editing_page_rect = Some(rect);
                                }
                            }
                        }
                        if visible {
                            if let Some(sel) = &self.text_selection {
                                if sel.page == i {
                                    if let Some(chars) = self.page_text.get(&i) {
                                        let fill = Color32::from_rgba_unmultiplied(80, 140, 230, 96);
                                        for &ci in &sel.chars {
                                            if let Some(c) = chars.get(ci) {
                                                let r = Rect::from_min_size(
                                                    rect.min + Vec2::new(c.x, c.y) * zoom,
                                                    Vec2::new(c.width, c.height) * zoom,
                                                );
                                                painter.rect_filled(r, 0.0, fill);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(180)));

                        if objects_mode {
                            if in_render_band && !self.objects.contains_key(&i) && !self.objects_requested.contains(&i) {
                                self.engine.list_objects(i, self.generation);
                                self.objects_requested.insert(i);
                            }
                            let fragile_page = self.unsafe_object_pages.contains(&i);
                            if let Some(list) = self.objects.get(&i) {
                                let hover_idx = response
                                    .hover_pos()
                                    .and_then(|pp| topmost_object_at(list, &self.object_edits, i, rect, zoom, pp));
                                let chrome = ui.painter_at(rect.expand(ROTATE_OFFSET_PX + HANDLE_PX + 6.0));
                                for o in list {
                                    let edit = self.object_edits.get(&(i, o.object_index));
                                    let quad = object_quad_screen(rect, zoom, o, edit);
                                    if edit.map_or(false, |e| e.delete) {
                                        let c = Color32::from_rgba_unmultiplied(200, 60, 60, 130);
                                        chrome.add(Shape::closed_line(quad.to_vec(), Stroke::new(1.0, c)));
                                        chrome.line_segment([quad[0], quad[2]], Stroke::new(1.0, c));
                                        chrome.line_segment([quad[1], quad[3]], Stroke::new(1.0, c));
                                        continue;
                                    }
                                    let edited = edit.map_or(false, |e| !e.is_noop());
                                    let col = object_color(o.kind);
                                    let selected = self.selected_object == Some((i, o.object_index));
                                    let stroke = if selected {
                                        Stroke::new(2.0, col)
                                    } else if hover_idx == Some(o.object_index) {
                                        Stroke::new(1.6, col)
                                    } else if edited {
                                        Stroke::new(1.3, col)
                                    } else {
                                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 110))
                                    };
                                    chrome.add(Shape::closed_line(quad.to_vec(), stroke));
                                    if selected {
                                        for h in [Handle::TopLeft, Handle::TopRight, Handle::BottomRight, Handle::BottomLeft] {
                                            let p = object_handle_screen(rect, zoom, o, edit, h);
                                            let r = Rect::from_center_size(p, Vec2::splat(HANDLE_PX * 2.0));
                                            chrome.rect_filled(r, 1.0, Color32::WHITE);
                                            chrome.rect_stroke(r, 1.0, Stroke::new(1.2, col));
                                        }
                                        let (c, half, a) = object_transform(o, edit);
                                        let top_mid = object_local_to_screen(rect, zoom, c, a, Vec2::new(0.0, -half.y));
                                        let rot_h = object_handle_screen(rect, zoom, o, edit, Handle::Rotate);
                                        chrome.line_segment([top_mid, rot_h], Stroke::new(1.2, col));
                                        chrome.circle_filled(rot_h, HANDLE_PX, Color32::WHITE);
                                        chrome.circle_stroke(rot_h, HANDLE_PX, Stroke::new(1.2, col));
                                        chrome.text(
                                            quad[0] + Vec2::new(3.0, 2.0),
                                            Align2::LEFT_TOP,
                                            format!("#{} {}{}", o.object_index, object_kind_name(o.kind), if edited { " *" } else { "" }),
                                            FontId::monospace(11.0),
                                            col,
                                        );
                                    }
                                }
                                for (gi, d) in self.object_dupes.iter().enumerate() {
                                    if d.page_index != i {
                                        continue;
                                    }
                                    let Some(o) = list.iter().find(|o| o.object_index == d.object_index) else {
                                        continue;
                                    };
                                    let quad = object_quad_screen(rect, zoom, o, Some(d));
                                    let dup_col = Color32::from_rgb(15, 160, 150);
                                    let sel = self.selected_dupe == Some(gi);
                                    chrome.add(Shape::closed_line(
                                        quad.to_vec(),
                                        Stroke::new(if sel { 2.6 } else { 1.6 }, dup_col),
                                    ));
                                    if sel {
                                        for p in quad.iter() {
                                            chrome.circle_filled(*p, 3.0, dup_col);
                                        }
                                    }
                                    chrome.text(
                                        quad[0] + Vec2::new(3.0, 2.0),
                                        Align2::LEFT_TOP,
                                        format!("copy of #{}{}", d.object_index, if sel { " (selected)" } else { "" }),
                                        FontId::monospace(10.0),
                                        dup_col,
                                    );
                                }
                                let _ = fragile_page;
                            }

                            if let Some((sp, soi)) = self.selected_object {
                                if sp == i {
                                    response.context_menu(|ui| {
                                        if ui
                                            .button(format!("{}  Duplicate object #{}", ph::COPY, soi))
                                            .clicked()
                                        {
                                            ctx_duplicate = Some((sp, soi));
                                            ui.close_menu();
                                        }
                                    });
                                }
                            }
                        }

                        if interactive && self.eyedropper.is_some() {
                            ctx.set_cursor_icon(CursorIcon::Crosshair);
                            if response.clicked() {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    eyedropper_sample = Some((i, rect, pp));
                                }
                            }
                        } else if interactive && objects_mode {
                            if let (Some((sp, soi)), Some(pp)) = (self.selected_object, response.hover_pos()) {
                                if sp == i {
                                    if let Some(o) = self.objects.get(&i).and_then(|l| l.iter().find(|o| o.object_index == soi)) {
                                        let e = self.object_edits.get(&(i, soi));
                                        if !e.map_or(false, |e| e.delete) {
                                            if let Some(h) = hit_object_handle(rect, zoom, o, e, pp) {
                                                ctx.set_cursor_icon(cursor_for(h));
                                            }
                                        }
                                    }
                                }
                            }
                            if response.drag_started() {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    let mut started = false;
                                    if let Some((sp, soi)) = self.selected_object {
                                        if sp == i {
                                            if let Some(o) = self.objects.get(&i).and_then(|l| l.iter().find(|o| o.object_index == soi)) {
                                                let e = self.object_edits.get(&(i, soi));
                                                if !e.map_or(false, |e| e.delete) {
                                                    if let Some(h) = hit_object_handle(rect, zoom, o, e, pp) {
                                                        let pt = (pp - rect.min) / zoom;
                                                        new_object_drag = Some(ObjectDrag {
                                                            page: i,
                                                            object_index: soi,
                                                            handle: h,
                                                            obj_x: o.x,
                                                            obj_y: o.y,
                                                            obj_w: o.width,
                                                            obj_h: o.height,
                                                            orig: e.cloned().unwrap_or_else(|| ObjectEdit::new(i, soi)),
                                                            start_page_pt: Pos2::new(pt.x, pt.y),
                                                            dupe_index: None,
                                                        });
                                                        started = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if !started {
                                        if let Some(gi) = self.selected_dupe {
                                            let dupe = self.object_dupes.get(gi).filter(|d| d.page_index == i).cloned();
                                            if let Some(d) = dupe {
                                                if let Some(o) = self.objects.get(&i).and_then(|l| l.iter().find(|o| o.object_index == d.object_index)) {
                                                    if point_in_object(rect, zoom, o, Some(&d), pp) {
                                                        let pt = (pp - rect.min) / zoom;
                                                        new_object_drag = Some(ObjectDrag {
                                                            page: i,
                                                            object_index: d.object_index,
                                                            handle: Handle::Move,
                                                            obj_x: o.x,
                                                            obj_y: o.y,
                                                            obj_w: o.width,
                                                            obj_h: o.height,
                                                            orig: d,
                                                            start_page_pt: Pos2::new(pt.x, pt.y),
                                                            dupe_index: Some(gi),
                                                        });
                                                        started = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if !started {
                                        match self.picks_at(i, rect, zoom, pp).first().copied() {
                                            Some(Pick::Dupe(gi)) => {
                                                click_select_dupe = Some(Some(gi));
                                                click_select_object = Some(None);
                                                let dupe = self.object_dupes.get(gi).cloned();
                                                if let Some(d) = dupe {
                                                    if let Some(o) = self.objects.get(&i).and_then(|l| l.iter().find(|o| o.object_index == d.object_index)) {
                                                        let pt = (pp - rect.min) / zoom;
                                                        new_object_drag = Some(ObjectDrag {
                                                            page: i,
                                                            object_index: d.object_index,
                                                            handle: Handle::Move,
                                                            obj_x: o.x,
                                                            obj_y: o.y,
                                                            obj_w: o.width,
                                                            obj_h: o.height,
                                                            orig: d,
                                                            start_page_pt: Pos2::new(pt.x, pt.y),
                                                            dupe_index: Some(gi),
                                                        });
                                                    }
                                                }
                                            }
                                            Some(Pick::Orig(oi)) => {
                                                click_select_object = Some(Some((i, oi)));
                                                click_select_dupe = Some(None);
                                                if let Some(o) = self.objects.get(&i).and_then(|l| l.iter().find(|o| o.object_index == oi)) {
                                                    let pt = (pp - rect.min) / zoom;
                                                    new_object_drag = Some(ObjectDrag {
                                                        page: i,
                                                        object_index: oi,
                                                        handle: Handle::Move,
                                                        obj_x: o.x,
                                                        obj_y: o.y,
                                                        obj_w: o.width,
                                                        obj_h: o.height,
                                                        orig: self.object_edits.get(&(i, oi)).cloned().unwrap_or_else(|| ObjectEdit::new(i, oi)),
                                                        start_page_pt: Pos2::new(pt.x, pt.y),
                                                        dupe_index: None,
                                                    });
                                                }
                                            }
                                            None => {
                                                click_select_object = Some(None);
                                                click_select_dupe = Some(None);
                                            }
                                        }
                                    }
                                }
                            } else if response.clicked() {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    let picks = self.picks_at(i, rect, zoom, pp);
                                    let cur_pos = picks.iter().position(|p| match p {
                                        Pick::Orig(oi) => self.selected_object == Some((i, *oi)),
                                        Pick::Dupe(gi) => self.selected_dupe == Some(*gi),
                                    });
                                    let next = match cur_pos {
                                        Some(pos) if !picks.is_empty() => picks.get((pos + 1) % picks.len()).copied(),
                                        _ => picks.first().copied(),
                                    };
                                    match next {
                                        Some(Pick::Orig(oi)) => {
                                            click_select_object = Some(Some((i, oi)));
                                            click_select_dupe = Some(None);
                                        }
                                        Some(Pick::Dupe(gi)) => {
                                            click_select_dupe = Some(Some(gi));
                                            click_select_object = Some(None);
                                        }
                                        None => {
                                            click_select_object = Some(None);
                                            click_select_dupe = Some(None);
                                        }
                                    }
                                }
                            }
                            if let Some(d) = &self.object_drag {
                                if d.page == i && response.dragged() {
                                    if let Some(pp) = response.interact_pointer_pos() {
                                        let pt = (pp - rect.min) / zoom;
                                        object_drag_pos = Some(Pos2::new(pt.x, pt.y));
                                    }
                                }
                                if response.drag_stopped() {
                                    stop_object_drag = true;
                                }
                            }
                        } else if interactive {
                            let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());

                            if let (Some(sel), Some(pp)) = (self.selected, pointer) {
                                if sel < self.edits.len() && self.edits[sel].page_index == i {
                                    if let Some(h) = hit_handle(&self.edits[sel], rect, zoom, pp) {
                                        ctx.set_cursor_icon(cursor_for(h));
                                    }
                                }
                            }
                            if response.hovered() {
                                match self.tool {
                                    Tool::Text => ctx.set_cursor_icon(CursorIcon::Text),
                                    Tool::Image => ctx.set_cursor_icon(CursorIcon::Crosshair),
                                    Tool::Pan => {
                                        let cursor = if ctx.input(|i| i.pointer.primary_down()) {
                                            CursorIcon::Grabbing
                                        } else {
                                            CursorIcon::Grab
                                        };
                                        ctx.set_cursor_icon(cursor);
                                    }
                                    Tool::Select => {
                                        if !self.page_text.contains_key(&i)
                                            && !self.page_text_requested.contains(&i)
                                        {
                                            self.engine.load_page_text(i, self.generation);
                                            self.page_text_requested.insert(i);
                                        }
                                        let drag_active = self
                                            .text_drag
                                            .as_ref()
                                            .map_or(false, |td| td.page == i);
                                        let over_glyph = if drag_active {
                                            true
                                        } else if let (Some(pp), Some(chars)) = (
                                            response.hover_pos(),
                                            self.page_text.get(&i),
                                        ) {
                                            let pt = (pp - rect.min) / zoom;
                                            chars.iter().any(|c| {
                                                pt.x >= c.x
                                                    && pt.x <= c.x + c.width
                                                    && pt.y >= c.y
                                                    && pt.y <= c.y + c.height
                                            })
                                        } else {
                                            false
                                        };
                                        if over_glyph {
                                            ctx.set_cursor_icon(CursorIcon::Text);
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            if response.drag_started() {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    let page_pt = (pp - rect.min) / zoom;
                                    let page_pt = Pos2::new(page_pt.x, page_pt.y);
                                    let mut started = false;
                                    if let Some(sel) = self.selected {
                                        if sel < self.edits.len() && self.edits[sel].page_index == i {
                                            if let Some(h) = hit_handle(&self.edits[sel], rect, zoom, pp) {
                                                new_drag = Some(Drag {
                                                    edit: sel,
                                                    handle: h,
                                                    page: i,
                                                    orig: self.edits[sel].clone(),
                                                    start_page_pt: page_pt,
                                                });
                                                started = true;
                                            }
                                        }
                                    }
                                    if !started {
                                        if let Some(ei) = topmost_edit_at(&self.edits, i, rect, zoom, pp) {
                                            click_select = Some(Some(ei));
                                            new_drag = Some(Drag {
                                                edit: ei,
                                                handle: Handle::Move,
                                                page: i,
                                                orig: self.edits[ei].clone(),
                                                start_page_pt: page_pt,
                                            });
                                            started = true;
                                        }
                                    }
                                    if !started {
                                        match self.tool {
                                            Tool::Text => {
                                                let (nx, ny) = new_text_click_pos(page_pt.x, page_pt.y);
                                                new_edit = Some((OverlayKind::Text, i, nx, ny));
                                            }
                                            Tool::Image => new_image = Some((i, page_pt.x, page_pt.y)),
                                            Tool::Select => {
                                                click_select = Some(None);
                                                new_text_drag = Some(TextDrag {
                                                    page: i,
                                                    start_page_pt: page_pt,
                                                    current_page_pt: page_pt,
                                                });
                                            }
                                            _ => click_select = Some(None),
                                        }
                                    }
                                }
                            } else if response.clicked() {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    let page_pt = (pp - rect.min) / zoom;
                                    match self.tool {
                                        Tool::Text => {
                                            let hit_text = topmost_edit_at(&self.edits, i, rect, zoom, pp)
                                                .filter(|&ei| self.edits[ei].kind == OverlayKind::Text);
                                            if let Some(ei) = hit_text {
                                                click_select = Some(Some(ei));
                                                new_text_focus = Some(ei);
                                            } else {
                                                let (nx, ny) = new_text_click_pos(page_pt.x, page_pt.y);
                                                new_edit = Some((OverlayKind::Text, i, nx, ny));
                                            }
                                        }
                                        Tool::Image => new_image = Some((i, page_pt.x, page_pt.y)),
                                        _ => {
                                            let hit = topmost_edit_at(&self.edits, i, rect, zoom, pp);
                                            click_select = Some(hit);
                                            if self.tool == Tool::Select && hit.is_none() {
                                                self.text_selection = None;
                                            }
                                        }
                                    }
                                }
                            }
                            if response.double_clicked() && self.tool == Tool::Select {
                                if let Some(pp) = response.interact_pointer_pos() {
                                    let hit_text = topmost_edit_at(&self.edits, i, rect, zoom, pp)
                                        .filter(|&ei| self.edits[ei].kind == OverlayKind::Text);
                                    if let Some(ei) = hit_text {
                                        click_select = Some(Some(ei));
                                        new_text_focus = Some(ei);
                                    }
                                }
                            }

                            if let Some(d) = &self.drag {
                                if d.page == i && response.dragged() {
                                    if let Some(pp) = response.interact_pointer_pos() {
                                        let page_pt = (pp - rect.min) / zoom;
                                        let page_pt = Pos2::new(page_pt.x, page_pt.y);
                                        if d.edit < self.edits.len() {
                                            let link = match self.edits[d.edit].kind {
                                                OverlayKind::Image => self.link_image,
                                                OverlayKind::Text => self.link_text,
                                            };
                                            let aspect_lock =
                                                link || ctx.input(|i| i.modifiers.shift);
                                            apply_drag(
                                                &mut self.edits[d.edit],
                                                d,
                                                page_pt,
                                                aspect_lock,
                                            );
                                        }
                                    }
                                }
                                if response.drag_stopped() {
                                    stop_drag = true;
                                }
                            }
                            if let Some(td) = &self.text_drag {
                                if td.page == i && response.dragged() {
                                    if let Some(pp) = response.interact_pointer_pos() {
                                        let page_pt = (pp - rect.min) / zoom;
                                        text_drag_update = Some(Pos2::new(page_pt.x, page_pt.y));
                                    }
                                }
                                if td.page == i && response.drag_stopped() {
                                    text_drag_finish = true;
                                }
                            }
                        }

                        ui.add_space(PAGE_GAP * zoom);
                    }

                    if let (Some(ei), Some(page_rect)) = (self.text_editing, editing_page_rect) {
                        let registered = &self.registered_embedded;
                        let serif = self.serif.as_ref();
                        if let Some(edit) = self.edits.get_mut(ei) {
                            let font_size = edit.font_size.unwrap_or(18.0);
                            let zoom_px = font_size * zoom;
                            let family = overlay_preview_family(edit, registered, serif);
                            let color = edit.color.map(|c| {
                                Color32::from_rgb(c[0], c[1], c[2])
                            }).unwrap_or(Color32::from_rgb(17, 24, 39));
                            let font_id = FontId::new(zoom_px, family);

                            let mut text = edit.text.clone().unwrap_or_default();
                            let measured = ui.fonts(|f| {
                                if text.is_empty() {
                                    Vec2::new(zoom_px * 0.4, zoom_px * 1.2)
                                } else {
                                    let galley = f.layout_no_wrap(
                                        text.clone(),
                                        font_id.clone(),
                                        color,
                                    );
                                    galley.size()
                                }
                            });
                            let pad_px = 4.0_f32;
                            let displayed_size = Vec2::new(
                                (measured.x + pad_px * 2.0).max(zoom_px),
                                (measured.y + pad_px * 2.0).max(zoom_px * 1.4),
                            );
                            let wrap_buffer = Vec2::new(zoom_px * 4.0, zoom_px * 1.5);
                            let widget_size = displayed_size + wrap_buffer;
                            let widget_rect = Rect::from_min_size(
                                page_rect.min + Vec2::new(edit.x, edit.y) * zoom,
                                widget_size,
                            );

                            let id = egui::Id::new(("paper-text-edit", ei));
                            let resp = ui.put(
                                widget_rect,
                                egui::TextEdit::multiline(&mut text)
                                    .id(id)
                                    .frame(false)
                                    .desired_width(f32::INFINITY)
                                    .text_color(color)
                                    .font(font_id),
                            );
                            if self.text_editing_just_started {
                                resp.request_focus();
                                self.text_editing_just_started = false;
                            }
                            edit.text = if text.is_empty() { None } else { Some(text) };
                            edit.width = displayed_size.x / zoom;
                            edit.height = displayed_size.y / zoom;
                            edit.ref_width = edit.width;
                            edit.ref_height = edit.height;
                            if resp.lost_focus() {
                                self.set_text_editing(None);
                            }
                            ctx.request_repaint();
                        } else {
                            self.text_editing = None;
                        }
                    }
                });

                if let Some(set) = click_select {
                    self.selected = set;
                }
                if let Some(set) = click_select_object {
                    self.selected_object = set;
                    self.selected = None;
                    self.drag = None;
                    if set.is_some() {
                        self.selected_dupe = None;
                    }
                    match set {
                        Some((p, o)) => {
                            let have = matches!(&self.object_details, Some((dp, do_, _)) if *dp == p && *do_ == o);
                            if !have {
                                self.object_details = None;
                                self.engine.request_object_details(p, o, self.generation);
                            }
                        }
                        None => self.object_details = None,
                    }
                }
                if let Some(set) = click_select_dupe {
                    self.selected_dupe = set;
                    if let Some(gi) = set {
                        self.selected_object = None;
                        self.selected = None;
                        self.drag = None;
                        if let Some(d) = self.object_dupes.get(gi) {
                            let (p, o) = (d.page_index, d.object_index);
                            let have = matches!(&self.object_details, Some((dp, do_, _)) if *dp == p && *do_ == o);
                            if !have {
                                self.object_details = None;
                                self.engine.request_object_details(p, o, self.generation);
                            }
                        }
                    }
                }
                if let Some((p, o)) = ctx_duplicate {
                    let seq = self
                        .object_dupes
                        .iter()
                        .filter(|d| d.page_index == p && d.object_index == o)
                        .filter_map(|d| d.copy_seq)
                        .max()
                        .map_or(0, |m| m + 1);
                    let mut d = ObjectEdit::new(p, o);
                    d.copy_seq = Some(seq);
                    self.object_dupes.push(d);
                    self.selected_dupe = Some(self.object_dupes.len() - 1);
                    self.selected_object = None;
                    self.selected = None;
                    let have = matches!(&self.object_details, Some((dp, do_, _)) if *dp == p && *do_ == o);
                    if !have {
                        self.object_details = None;
                        self.engine.request_object_details(p, o, self.generation);
                    }
                }
                if let Some(d) = new_drag {
                    self.drag = Some(d);
                }
                if let Some(d) = new_object_drag {
                    self.object_drag = Some(d);
                }
                if let Some(td) = new_text_drag {
                    self.text_selection = None;
                    let page = td.page;
                    self.text_drag = Some(td);
                    if !self.page_text.contains_key(&page)
                        && !self.page_text_requested.contains(&page)
                    {
                        self.engine.load_page_text(page, self.generation);
                        self.page_text_requested.insert(page);
                    }
                }
                if let (Some(td), Some(pt)) = (self.text_drag.as_mut(), text_drag_update) {
                    td.current_page_pt = pt;
                }
                if text_drag_finish {
                    if let Some(td) = self.text_drag.take() {
                        let rect = Self::drag_rect(&td);
                        let (rw, rh) = (rect.2, rect.3);
                        if rw >= 1.0 && rh >= 1.0 {
                            let chars = self.chars_in_rect(td.page, rect);
                            self.text_selection = Some(TextSelection {
                                page: td.page,
                                rect,
                                chars,
                            });
                        } else {
                            self.text_selection = None;
                        }
                    }
                }
                if self.text_drag.is_some() {
                    self.recompute_text_selection();
                }
                if let (Some(d), Some(pp)) = (self.object_drag.as_ref(), object_drag_pos) {
                    let aspect_lock = self.link_object || ctx.input(|i| i.modifiers.shift);
                    if let Some(gi) = d.dupe_index {
                        if gi < self.object_dupes.len() {
                            let mut e = d.orig.clone();
                            apply_object_drag(&mut e, d, pp, aspect_lock);
                            self.object_dupes[gi] = e;
                        }
                    } else {
                        let key = (d.page, d.object_index);
                        let mut e = self.object_edits.get(&key).cloned().unwrap_or_else(|| ObjectEdit::new(key.0, key.1));
                        apply_object_drag(&mut e, d, pp, aspect_lock);
                        self.object_edits.insert(key, e);
                    }
                }
                if let Some((kind, page, x, y)) = new_edit {
                    self.set_text_editing(None);
                    let idx = self.add_edit(kind, page, x, y);
                    self.selected = Some(idx);
                    if kind == OverlayKind::Text {
                        self.set_text_editing(Some(idx));
                    }
                }
                if let Some(ei) = new_text_focus {
                    self.set_text_editing(Some(ei));
                }
                if let Some((page, x, y)) = new_image {
                    if let Some(idx) = self.add_image_edit(page, x, y) {
                        self.selected = Some(idx);
                    }
                }
                if stop_drag {
                    self.drag = None;
                }
                if stop_object_drag {
                    self.object_drag = None;
                }
                if let Some((page, rect, pp)) = eyedropper_sample {
                    self.apply_eyedropper_sample(page, rect, pp);
                }
                self.current_page = best_page.1;
            });

            let zoom_request: Option<(f32, Option<Pos2>)> = if let Some((units, anchor)) = wheel_zoom {
                let target = (self.zoom * (units * 0.20).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
                Some((target, anchor))
            } else {
                self.pending_zoom.take().map(|t| (t, None))
            };
            if let Some((z1, anchor)) = zoom_request {
                let z0 = self.zoom;
                if (z1 - z0).abs() > 1e-6 {
                    let viewport = scroll_output.inner_rect;
                    let a = anchor.unwrap_or_else(|| viewport.center());
                    let a = Pos2::new(
                        a.x.clamp(viewport.min.x, viewport.max.x),
                        a.y.clamp(viewport.min.y, viewport.max.y),
                    );
                    let rel = a - viewport.min;
                    let k = z1 / z0;

                    let max_page_w = doc.pages.iter().map(|p| p.width).fold(1.0_f32, f32::max);
                    let viewport_w = viewport.width();
                    let page_left_old =
                        ((viewport_w - max_page_w * z0) * 0.5).max(0.0);
                    let page_left_new =
                        ((viewport_w - max_page_w * z1) * 0.5).max(0.0);
                    let pad_top = PAGE_PAD_TOP;

                    let offset_old = scroll_output.state.offset;
                    let new_off_x =
                        (rel.x + offset_old.x - page_left_old) * k + page_left_new - rel.x;
                    let new_off_y =
                        (rel.y + offset_old.y - pad_top) * k + pad_top - rel.y;
                    let new_content_w = (max_page_w * z1).max(viewport_w);
                    let max_off_x = (new_content_w - viewport_w).max(0.0);
                    self.zoom = z1;
                    self.pending_scroll_offset = Some(Vec2::new(
                        new_off_x.clamp(0.0, max_off_x),
                        new_off_y.max(0.0),
                    ));
                    ctx.request_repaint();
                }
            }
        });
    }

    fn add_edit(&mut self, kind: OverlayKind, page: usize, x: f32, y: f32) -> usize {
        let edit = match kind {
            OverlayKind::Text => OverlayEdit {
                page_index: page,
                kind,
                x,
                y,
                width: 24.0,
                height: 24.0,
                ref_width: 24.0,
                ref_height: 24.0,
                rotation: 0.0,
                flip_horizontal: false,
                flip_vertical: false,
                text: None,
                font_size: Some(self.settings.default_text_font_size),
                color: Some([17, 24, 39]),
                font_family: Some("Helvetica".to_string()),
                font_embedded_id: None,
                image_data: None,
            },
            OverlayKind::Image => OverlayEdit {
                page_index: page,
                kind,
                x,
                y,
                width: 120.0,
                height: 120.0,
                ref_width: 120.0,
                ref_height: 120.0,
                rotation: 0.0,
                flip_horizontal: false,
                flip_vertical: false,
                text: None,
                font_size: None,
                color: None,
                font_family: None,
                font_embedded_id: None,
                image_data: None,
            },
        };
        self.edits.push(edit);
        self.edits.len() - 1
    }

    fn add_image_edit(&mut self, page: usize, x: f32, y: f32) -> Option<usize> {
        let path = rfd::FileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
            .pick_file()?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(format!("Could not read {}: {e}", path.display()));
                return None;
            }
        };
        let (iw, ih) = match image::load_from_memory(&bytes) {
            Ok(img) => (img.width().max(1) as f32, img.height().max(1) as f32),
            Err(e) => {
                self.error = Some(format!("Unsupported image {}: {e}", path.display()));
                return None;
            }
        };
        let scale = (260.0 / iw.max(ih)).min(1.0);
        let (w, h) = ((iw * scale).max(MIN_EDIT_SIZE), (ih * scale).max(MIN_EDIT_SIZE));
        self.edits.push(OverlayEdit {
            page_index: page,
            kind: OverlayKind::Image,
            x: (x - w / 2.0).max(0.0),
            y: (y - h / 2.0).max(0.0),
            width: w,
            height: h,
            ref_width: w,
            ref_height: h,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            text: None,
            font_size: None,
            color: None,
            font_family: None,
            font_embedded_id: None,
            image_data: Some(Arc::new(bytes)),
        });
        Some(self.edits.len() - 1)
    }

    fn edit_state(&self) -> EditState {
        EditState {
            overlays: self.edits.clone(),
            objects: self.object_edits.clone(),
            dupes: self.object_dupes.clone(),
        }
    }
    fn has_unexported_changes(&self) -> bool {
        self.doc.is_some() && self.edit_state() != self.saved_state
    }

    fn confirm_close_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_close {
            return;
        }
        if !self.has_unexported_changes() {
            self.confirm_close = false;
            return;
        }
        let mut export = false;
        let mut close_anyway = false;
        let mut cancel = false;
        egui::Window::new("Unexported changes")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("This document has changes that haven't been exported.");
                ui.label("Close without exporting them?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(format!("{}  Export…", ph::EXPORT)).clicked() {
                        export = true;
                    }
                    if ui
                        .button(format!("{}  Close without exporting", ph::X))
                        .clicked()
                    {
                        close_anyway = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if export {
            self.confirm_close = false;
            if self.export_dialog() {
                self.pending_after_export = Some(AfterExport::Close);
            }
        } else if close_anyway {
            self.confirm_close = false;
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if cancel {
            self.confirm_close = false;
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Default text size");
                        ui.add(
                            egui::DragValue::new(&mut self.settings.default_text_font_size)
                                .speed(0.2)
                                .range(4.0..=400.0)
                                .suffix(" pt"),
                        );
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.small("Applied when you add a new text box. Saved automatically.");
            });
        if !open {
            self.show_settings = false;
        }
    }

    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        if self.about_icon.is_none() {
            let png = include_bytes!("../assets/icon_256.png");
            if let Ok(img) = image::load_from_memory(png) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let color = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.about_icon =
                    Some(ctx.load_texture("about-icon", color, TextureOptions::LINEAR));
            }
        }
        let mut open = true;
        egui::Window::new("About Paper")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(tex) = &self.about_icon {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(96.0)));
                    }
                    ui.add_space(6.0);
                    ui.heading("Paper");
                    ui.label("A small tool to help retouch PDFs.");
                    ui.label("Modify and add text and images.");
                    ui.add_space(8.0);
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label(format!(
                        "Built {}",
                        option_env!("PAPER_BUILD_DATE").unwrap_or("unknown")
                    ));
                    ui.add_space(8.0);
                    ui.label("MIT License");
                    ui.add_space(4.0);
                });
            });
        self.show_about = open;
    }

    fn confirm_open_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_open.clone() else { return };
        if !self.has_unexported_changes() {
            self.pending_open = None;
            self.open_path(path);
            return;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("the selected file")
            .to_string();
        let mut export = false;
        let mut discard = false;
        let mut cancel = false;
        egui::Window::new("Unexported changes")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The current document has changes that haven't been exported.");
                ui.label(format!("Open “{name}” and discard them?"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(format!("{}  Export…", ph::EXPORT)).clicked() {
                        export = true;
                    }
                    if ui.button(format!("{}  Discard & open", ph::X)).clicked() {
                        discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if export {
            let p = self.pending_open.take();
            if self.export_dialog() {
                self.pending_after_export = p.map(AfterExport::Open);
            } else {
                self.pending_open = p;
            }
        } else if discard {
            self.pending_open = None;
            self.open_path(path);
        } else if cancel {
            self.pending_open = None;
        }
    }
    fn restore_edit_state(&mut self, s: EditState) {
        self.edits = s.overlays;
        self.object_edits = s.objects;
        self.object_dupes = s.dupes;
        self.selected = None;
        self.selected_object = None;
        self.selected_dupe = None;
        self.object_details = None;
        self.drag = None;
        self.object_drag = None;
    }
    fn do_undo(&mut self) {
        let cur = self.edit_state();
        if let Some(prev) = self.history.undo(cur) {
            self.restore_edit_state(prev);
        }
    }
    fn do_redo(&mut self) {
        let cur = self.edit_state();
        if let Some(next) = self.history.redo(cur) {
            self.restore_edit_state(next);
        }
    }

    fn object_inspector_panel(&mut self, ctx: &egui::Context) {
        if self.selected.is_some() {
            return;
        }
        let Some((page, oi)) = self.selected_object else { return };
        let key = (page, oi);
        let info = self
            .objects
            .get(&page)
            .and_then(|l| l.iter().find(|o| o.object_index == oi))
            .cloned();
        let is_text = info.as_ref().map_or(false, |i| i.kind == ObjectKind::Text);
        let is_image = info.as_ref().map_or(false, |i| i.kind == ObjectKind::Image);
        let details = match &self.object_details {
            Some((p, o, d)) if *p == page && *o == oi => Some(d.clone()),
            _ => None,
        };
        let cur = self.object_edits.get(&key).cloned().unwrap_or_else(|| ObjectEdit::new(page, oi));
        let mut next = cur.clone();
        let fallback_fill = details.as_ref().and_then(|d| d.fill_color).unwrap_or([0, 0, 0]);
        let fallback_stroke = details.as_ref().and_then(|d| d.stroke_color).unwrap_or([0, 0, 0]);
        let fragile_page = self.unsafe_object_pages.contains(&page);
        let mut select_dupe: Option<usize> = None;
        let mut remove_dupe: Option<usize> = None;

        egui::SidePanel::right("object-props").resizable(true).default_width(272.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("PDF object");
            ui.horizontal(|ui| {
                ui.label(format!("page {}  ·  #{}", page + 1, oi));
                if let Some(info) = &info {
                    ui.label(format!("·  {}", object_kind_name(info.kind)));
                }
            });
            if let Some(info) = &info {
                ui.small(format!("orig: {:.0}×{:.0} pt", info.width, info.height));
            }
            ui.separator();

            let _ = fragile_page;

            ui.checkbox(&mut next.delete, format!("{}  Delete this object on export", ph::TRASH));
            if let Some(info) = &info {
                egui::Grid::new("obj-orig-geom")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("X");
                        ui.add_enabled(false, egui::DragValue::new(&mut info.x.clone()).suffix(" pt"));
                        ui.end_row();
                        ui.label("Y");
                        ui.add_enabled(false, egui::DragValue::new(&mut info.y.clone()).suffix(" pt"));
                        ui.end_row();
                    });
            }
            ui.add_enabled_ui(!next.delete, |ui| {
                let orig_w = info.as_ref().map_or(1.0, |i| i.width.max(0.1));
                let orig_h = info.as_ref().map_or(1.0, |i| i.height.max(0.1));
                egui::Grid::new("obj-edit-move").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Move X");
                    ui.add(egui::DragValue::new(&mut next.dx).speed(0.5).suffix(" pt"));
                    ui.end_row();
                    ui.label("Move Y");
                    ui.add(egui::DragValue::new(&mut next.dy).speed(0.5).suffix(" pt"));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.label("Size");
                    ui.selectable_value(&mut self.unit_pct_object, false, "pt");
                    ui.selectable_value(&mut self.unit_pct_object, true, "%");
                    ui.add_space(6.0);
                    let icon = if self.link_object { ph::LINK } else { ph::LINK_BREAK };
                    ui.toggle_value(&mut self.link_object, icon)
                        .on_hover_text("Keep width / height proportional");
                });
                let linked = self.link_object;
                let mut w = next.scale_x * orig_w;
                let mut h = next.scale_y * orig_h;
                edit_size_row(
                    ui,
                    "obj-edit-size",
                    &mut w,
                    &mut h,
                    orig_w,
                    orig_h,
                    self.unit_pct_object,
                    linked,
                );
                next.scale_x = (w / orig_w).max(0.001);
                next.scale_y = (h / orig_h).max(0.001);

                egui::Grid::new("obj-edit-rot-flip").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Rotation");
                    ui.add(egui::DragValue::new(&mut next.rotation).speed(0.5).suffix("°"));
                    ui.end_row();
                    ui.label("Flip");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut next.flip_horizontal, format!("{} H", ph::FLIP_HORIZONTAL));
                        ui.checkbox(&mut next.flip_vertical, format!("{} V", ph::FLIP_VERTICAL));
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                let fill_target = EyedropperTarget::ObjectFill(page, oi);
                let fill_active = self.eyedropper == Some(fill_target);
                match optional_color_editor(
                    ui,
                    "Override fill colour",
                    &mut next.fill_color,
                    &mut next.fill_alpha,
                    fallback_fill,
                    &mut self.recent_colors,
                    fill_active,
                ) {
                    ColorFieldOutcome::ToggleEyedropper => {
                        self.eyedropper = if fill_active { None } else { Some(fill_target) };
                    }
                    _ => {}
                }
                let stroke_target = EyedropperTarget::ObjectStroke(page, oi);
                let stroke_active = self.eyedropper == Some(stroke_target);
                match optional_color_editor(
                    ui,
                    "Override stroke colour",
                    &mut next.stroke_color,
                    &mut next.stroke_alpha,
                    fallback_stroke,
                    &mut self.recent_colors,
                    stroke_active,
                ) {
                    ColorFieldOutcome::ToggleEyedropper => {
                        self.eyedropper = if stroke_active { None } else { Some(stroke_target) };
                    }
                    _ => {}
                }
                ui.horizontal(|ui| {
                    let mut on = next.stroke_width.is_some();
                    if ui.checkbox(&mut on, "Override stroke width").changed() {
                        next.stroke_width = if on { Some(details.as_ref().and_then(|d| d.stroke_width).unwrap_or(1.0)) } else { None };
                    }
                    if let Some(w) = &mut next.stroke_width {
                        ui.add(egui::DragValue::new(w).speed(0.1).range(0.0..=200.0).suffix(" pt"));
                    }
                });
                if is_text {
                    ui.horizontal(|ui| {
                        let mut on = next.font_size.is_some();
                        if ui.checkbox(&mut on, "Override font size").changed() {
                            next.font_size = if on { Some(details.as_ref().and_then(|d| d.font_size).unwrap_or(12.0)) } else { None };
                        }
                        if let Some(s) = &mut next.font_size {
                            ui.add(egui::DragValue::new(s).speed(0.2).range(1.0..=400.0).suffix(" pt"));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut on = next.char_spacing.is_some();
                        if ui.checkbox(&mut on, "Override character spacing").changed() {
                            next.char_spacing = if on {
                                Some(details.as_ref().and_then(|d| d.char_spacing).unwrap_or(0.0))
                            } else {
                                None
                            };
                        }
                        if let Some(v) = &mut next.char_spacing {
                            ui.add(
                                egui::DragValue::new(v)
                                    .speed(0.02)
                                    .range(-10.0..=10.0)
                                    .suffix(" pt"),
                            );
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut on = next.word_spacing.is_some();
                        if ui.checkbox(&mut on, "Override word spacing").changed() {
                            next.word_spacing = if on {
                                Some(details.as_ref().and_then(|d| d.word_spacing).unwrap_or(0.0))
                            } else {
                                None
                            };
                        }
                        if let Some(v) = &mut next.word_spacing {
                            ui.add(
                                egui::DragValue::new(v)
                                    .speed(0.1)
                                    .range(-20.0..=20.0)
                                    .suffix(" pt"),
                            );
                        }
                    });
                    let original = details.as_ref().and_then(|d| d.text.clone()).unwrap_or_default();
                    let mut text = next.text.clone().unwrap_or_else(|| original.clone());
                    ui.label("Text:");
                    let resp = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY));
                    if resp.changed() {
                        next.text = if text == original { None } else { Some(text) };
                    }
                    if details.is_none() {
                        ui.small("(loading current text…)");
                    }
                    if details.as_ref().map_or(false, |d| d.is_kerned_tj) {
                        ui.small(
                            egui::RichText::new(
                                "⚠ Typeset line. Editing the text may change the spacing of the whole line. Undo restores it.",
                            )
                            .color(egui::Color32::from_rgb(180, 130, 30)),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Z-order:");
                    if ui.button(ph::ARROW_LINE_DOWN).on_hover_text("Send to back").clicked() {
                        next.arrange = Some(ArrangeAction::SendToBack);
                    }
                    if ui.button(ph::ARROW_DOWN).on_hover_text("Send backward").clicked() {
                        next.arrange = Some(match next.arrange {
                            Some(ArrangeAction::Shift(n)) => ArrangeAction::Shift(n - 1),
                            _ => ArrangeAction::Shift(-1),
                        });
                    }
                    if ui.button(ph::ARROW_UP).on_hover_text("Bring forward").clicked() {
                        next.arrange = Some(match next.arrange {
                            Some(ArrangeAction::Shift(n)) => ArrangeAction::Shift(n + 1),
                            _ => ArrangeAction::Shift(1),
                        });
                    }
                    if ui.button(ph::ARROW_LINE_UP).on_hover_text("Bring to front").clicked() {
                        next.arrange = Some(ArrangeAction::BringToFront);
                    }
                    if next.arrange.is_some() && ui.button(ph::X).on_hover_text("Clear z-order change").clicked() {
                        next.arrange = None;
                    }
                });
                if let Some(a) = next.arrange {
                    let label = match a {
                        ArrangeAction::BringToFront => "brought to front".to_string(),
                        ArrangeAction::SendToBack => "sent to back".to_string(),
                        ArrangeAction::Shift(0) => "(no z-order change)".to_string(),
                        ArrangeAction::Shift(1) => "moved forward 1 step".to_string(),
                        ArrangeAction::Shift(-1) => "moved back 1 step".to_string(),
                        ArrangeAction::Shift(n) if n > 0 => format!("moved forward {n} steps"),
                        ArrangeAction::Shift(n) => format!("moved back {} steps", n.abs()),
                    };
                    ui.small(label);
                }
                if is_image {
                    ui.horizontal(|ui| {
                        if ui.button(format!("{}  Replace image…", ph::IMAGE)).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
                                .pick_file()
                            {
                                if let Ok(bytes) = std::fs::read(&path) {
                                    if image::load_from_memory(&bytes).is_ok() {
                                        next.image_data = Some(Arc::new(bytes));
                                    }
                                }
                            }
                        }
                        if next.image_data.is_some() {
                            ui.label(format!("{} replaced", ph::CHECK))
                                .on_hover_text("This image object's bitmap will be swapped (kept within its original bounds).");
                            if ui.button(ph::X).on_hover_text("Keep the original image").clicked() {
                                next.image_data = None;
                            }
                        }
                    });
                    if ui
                        .button(format!("{}  Export image…", ph::DOWNLOAD_SIMPLE))
                        .on_hover_text("Save this image object's bitmap to disk as PNG (alpha preserved).")
                        .clicked()
                    {
                        let stem = self
                            .doc
                            .as_ref()
                            .and_then(|d| d.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
                            .unwrap_or_else(|| "document".to_string());
                        let default_name = format!("{stem}-p{}-obj{oi}.png", page + 1);
                        if let Some(out) = rfd::FileDialog::new()
                            .add_filter("PNG image", &["png"])
                            .set_file_name(default_name)
                            .save_file()
                        {
                            let out = if out.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false) {
                                out
                            } else {
                                out.with_extension("png")
                            };
                            self.status = format!("Exporting image to {}…", out.display());
                            self.engine.export_object_image(page, oi, out);
                        }
                    }
                }
            });

            let can_duplicate = info.as_ref().map_or(false, |i| {
                matches!(
                    i.kind,
                    ObjectKind::Text | ObjectKind::Path | ObjectKind::Image | ObjectKind::Form
                )
            });
            if can_duplicate {
                ui.separator();
                ui.label(egui::RichText::new("Duplicates").strong());
                if ui
                    .button(format!("{}  Duplicate object", ph::COPY))
                    .on_hover_text("Add an independent copy, stacked exactly on the original")
                    .clicked()
                {
                    let seq = self
                        .object_dupes
                        .iter()
                        .filter(|d| d.page_index == page && d.object_index == oi)
                        .filter_map(|d| d.copy_seq)
                        .max()
                        .map_or(0, |m| m + 1);
                    let mut d = ObjectEdit::new(page, oi);
                    d.copy_seq = Some(seq);
                    self.object_dupes.push(d);
                    select_dupe = Some(self.object_dupes.len() - 1);
                }
                let dup_indices: Vec<usize> = self
                    .object_dupes
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.page_index == page && d.object_index == oi)
                    .map(|(i, _)| i)
                    .collect();
                if dup_indices.is_empty() {
                    ui.small("No copies yet.");
                } else {
                    ui.small(format!(
                        "{} cop{}. Click one below (or on the page) to edit it.",
                        dup_indices.len(),
                        if dup_indices.len() == 1 { "y" } else { "ies" }
                    ));
                    for (n, &gi) in dup_indices.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(false, format!("{}  copy {}", ph::COPY, n + 1))
                                .clicked()
                            {
                                select_dupe = Some(gi);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(ph::TRASH).on_hover_text("Remove this copy").clicked() {
                                        remove_dupe = Some(gi);
                                    }
                                },
                            );
                        });
                    }
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Reset object").clicked() {
                    next = ObjectEdit::new(page, oi);
                }
                if !cur.is_noop() {
                    ui.label(
                        egui::RichText::new(ph::DOT)
                            .size(20.0)
                            .color(egui::Color32::from_rgb(220, 130, 30)),
                    )
                    .on_hover_text("This object has unsaved-to-disk edits (visible in the preview).");
                }
            });
            ui.small("Drag the box to move, the corner squares to resize, the circle to rotate • arrow keys nudge • pages re-render after each change.");

            ui.separator();
            ui.collapsing("original properties", |ui| match details {
                Some(d) => {
                    if let Some(t) = &d.text {
                        ui.label("text:");
                        ui.label(egui::RichText::new(t).monospace());
                    }
                    if let Some(f) = &d.font_name {
                        ui.label(format!("font: {f}"));
                    }
                    if let Some(s) = d.font_size {
                        ui.label(format!("font size: {s:.2} pt"));
                    }
                    if let Some(c) = d.fill_color {
                        color_row(ui, "fill", c, d.fill_alpha);
                    }
                    if let Some(c) = d.stroke_color {
                        color_row(ui, "stroke", c, d.stroke_alpha);
                    }
                    if let Some(w) = d.stroke_width {
                        ui.label(format!("stroke width: {w:.2} pt"));
                    }
                    if let Some(v) = d.char_spacing {
                        ui.label(format!("char spacing: {v:.3} pt"));
                    }
                    if let Some(v) = d.word_spacing {
                        ui.label(format!("word spacing: {v:.3} pt"));
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("loading…");
                    });
                }
            });
            });
        });

        if next != cur {
            if next.is_noop() {
                self.object_edits.remove(&key);
            } else {
                self.object_edits.insert(key, next);
            }
        }
        if let Some(gi) = remove_dupe {
            if gi < self.object_dupes.len() {
                self.object_dupes.remove(gi);
            }
            if self.selected_dupe == Some(gi) {
                self.selected_dupe = None;
            }
        }
        if let Some(gi) = select_dupe {
            self.selected_dupe = Some(gi);
            self.selected_object = None;
        }
    }

    fn dupe_inspector_panel(&mut self, ctx: &egui::Context) {
        let Some(gi) = self.selected_dupe else { return };
        if gi >= self.object_dupes.len() {
            self.selected_dupe = None;
            return;
        }
        let (src_page, src_oi) = {
            let d = &self.object_dupes[gi];
            (d.page_index, d.object_index)
        };
        let details = match &self.object_details {
            Some((p, o, dt)) if *p == src_page && *o == src_oi => Some(dt.clone()),
            _ => None,
        };
        let src_info = self
            .objects
            .get(&src_page)
            .and_then(|l| l.iter().find(|o| o.object_index == src_oi))
            .cloned();
        let is_text = src_info.as_ref().map_or(false, |o| o.kind == ObjectKind::Text);
        let is_image = src_info.as_ref().map_or(false, |o| o.kind == ObjectKind::Image);
        let orig_w = src_info.as_ref().map_or(1.0, |i| i.width.max(0.1));
        let orig_h = src_info.as_ref().map_or(1.0, |i| i.height.max(0.1));

        let cur = self.object_dupes[gi].clone();
        let mut next = cur.clone();
        let mut remove = false;
        let mut select_source: Option<(usize, usize)> = None;
        let mut do_extract = false;
        let mut do_replace = false;
        egui::SidePanel::right("dupe-props")
            .resizable(true)
            .default_width(264.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Duplicate copy");
                    ui.label(format!("copy of object #{} · page {}", src_oi, src_page + 1));
                    if let Some(i) = &src_info {
                        ui.small(format!("orig: {:.0}×{:.0} pt", i.width, i.height));
                    }
                    ui.separator();
                    egui::Grid::new("dupe-move").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                        ui.label("Move X");
                        ui.add(egui::DragValue::new(&mut next.dx).speed(0.5).suffix(" pt"));
                        ui.end_row();
                        ui.label("Move Y");
                        ui.add(egui::DragValue::new(&mut next.dy).speed(0.5).suffix(" pt"));
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Size");
                        ui.selectable_value(&mut self.unit_pct_object, false, "pt");
                        ui.selectable_value(&mut self.unit_pct_object, true, "%");
                        ui.add_space(6.0);
                        let icon = if self.link_object { ph::LINK } else { ph::LINK_BREAK };
                        ui.toggle_value(&mut self.link_object, icon)
                            .on_hover_text("Keep width / height proportional");
                    });
                    let linked = self.link_object;
                    let mut w = next.scale_x * orig_w;
                    let mut h = next.scale_y * orig_h;
                    edit_size_row(ui, "dupe-size", &mut w, &mut h, orig_w, orig_h, self.unit_pct_object, linked);
                    next.scale_x = (w / orig_w).max(0.001);
                    next.scale_y = (h / orig_h).max(0.001);
                    egui::Grid::new("dupe-rot-flip").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                        ui.label("Rotation");
                        ui.add(egui::DragValue::new(&mut next.rotation).speed(0.5).suffix("°"));
                        ui.end_row();
                        ui.label("Flip");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut next.flip_horizontal, format!("{} H", ph::FLIP_HORIZONTAL));
                            ui.checkbox(&mut next.flip_vertical, format!("{} V", ph::FLIP_VERTICAL));
                        });
                        ui.end_row();
                        ui.label("Z-order");
                        let z_label = match next.dup_z {
                            DupZOrder::OnTop => "On top",
                            DupZOrder::AboveSource => "Above original",
                            DupZOrder::Behind => "Behind page",
                        };
                        egui::ComboBox::from_id_salt("dupe-z")
                            .selected_text(z_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut next.dup_z, DupZOrder::OnTop, "On top");
                                ui.selectable_value(&mut next.dup_z, DupZOrder::AboveSource, "Above original");
                                ui.selectable_value(&mut next.dup_z, DupZOrder::Behind, "Behind page");
                            });
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    dupe_color_row(ui, "Override fill colour", &mut next.fill_color, &mut next.fill_alpha);
                    dupe_color_row(ui, "Override stroke colour", &mut next.stroke_color, &mut next.stroke_alpha);
                    ui.horizontal(|ui| {
                        let mut on = next.stroke_width.is_some();
                        if ui.checkbox(&mut on, "Override stroke width").changed() {
                            next.stroke_width = if on {
                                Some(details.as_ref().and_then(|x| x.stroke_width).unwrap_or(1.0))
                            } else {
                                None
                            };
                        }
                        if let Some(w) = &mut next.stroke_width {
                            ui.add(egui::DragValue::new(w).speed(0.1).range(0.0..=200.0).suffix(" pt"));
                        }
                    });
                    if is_text {
                        ui.horizontal(|ui| {
                            let mut on = next.font_size.is_some();
                            if ui.checkbox(&mut on, "Override font size").changed() {
                                next.font_size = if on {
                                    Some(details.as_ref().and_then(|x| x.font_size).unwrap_or(12.0))
                                } else {
                                    None
                                };
                            }
                            if let Some(s) = &mut next.font_size {
                                ui.add(egui::DragValue::new(s).speed(0.2).range(1.0..=400.0).suffix(" pt"));
                            }
                        });
                        ui.horizontal(|ui| {
                            let mut on = next.char_spacing.is_some();
                            if ui.checkbox(&mut on, "Override character spacing").changed() {
                                next.char_spacing = if on {
                                    Some(details.as_ref().and_then(|x| x.char_spacing).unwrap_or(0.0))
                                } else {
                                    None
                                };
                            }
                            if let Some(v) = &mut next.char_spacing {
                                ui.add(egui::DragValue::new(v).speed(0.02).range(-10.0..=10.0).suffix(" pt"));
                            }
                        });
                        ui.horizontal(|ui| {
                            let mut on = next.word_spacing.is_some();
                            if ui.checkbox(&mut on, "Override word spacing").changed() {
                                next.word_spacing = if on {
                                    Some(details.as_ref().and_then(|x| x.word_spacing).unwrap_or(0.0))
                                } else {
                                    None
                                };
                            }
                            if let Some(v) = &mut next.word_spacing {
                                ui.add(egui::DragValue::new(v).speed(0.1).range(-20.0..=20.0).suffix(" pt"));
                            }
                        });
                        let original = details.as_ref().and_then(|x| x.text.clone()).unwrap_or_default();
                        let mut text = next.text.clone().unwrap_or_else(|| original.clone());
                        ui.label("Text:");
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY),
                        );
                        if resp.changed() {
                            next.text = if text == original { None } else { Some(text) };
                        }
                        if details.is_none() {
                            ui.small("(loading current text…)");
                        }
                        if details.as_ref().map_or(false, |x| x.is_kerned_tj) {
                            ui.small(
                                egui::RichText::new(
                                    "⚠ Typeset line. Editing the text may change the spacing of the whole line.",
                                )
                                .color(egui::Color32::from_rgb(180, 130, 30)),
                            );
                        }
                    }
                    if is_image {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button(format!("{}  Replace image…", ph::IMAGE)).clicked() {
                                do_replace = true;
                            }
                            if next.image_data.is_some()
                                && ui.button(ph::X).on_hover_text("Keep the source image").clicked()
                            {
                                next.image_data = None;
                            }
                        });
                        if next.image_data.is_some() {
                            ui.small("This copy uses a replacement image (the original is unchanged).");
                        }
                        if ui
                            .button(format!("{}  Export image…", ph::DOWNLOAD_SIMPLE))
                            .on_hover_text("Save the source object's bitmap to disk as PNG")
                            .clicked()
                        {
                            do_extract = true;
                        }
                    }
                    ui.separator();
                    ui.small("Drag the copy on the page to move it. It draws on top of the original.");
                    ui.horizontal(|ui| {
                        if ui.button(format!("{}  Remove copy", ph::TRASH)).clicked() {
                            remove = true;
                        }
                        if ui.button("Select original").clicked() {
                            select_source = Some((src_page, src_oi));
                        }
                    });
                });
            });
        if next != cur && gi < self.object_dupes.len() {
            self.object_dupes[gi] = next;
        }
        if do_replace {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
                .pick_file()
            {
                match std::fs::read(&path) {
                    Ok(bytes) if image::load_from_memory(&bytes).is_ok() => {
                        if let Some(d) = self.object_dupes.get_mut(gi) {
                            d.image_data = Some(Arc::new(bytes));
                        }
                    }
                    Ok(_) => self.error = Some(format!("Unsupported image {}", path.display())),
                    Err(e) => self.error = Some(format!("Could not read {}: {e}", path.display())),
                }
            }
        }
        if do_extract {
            let stem = self
                .doc
                .as_ref()
                .and_then(|d| d.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| "document".to_string());
            let default_name = format!("{stem}-p{}-obj{src_oi}.png", src_page + 1);
            if let Some(out) = rfd::FileDialog::new()
                .add_filter("PNG image", &["png"])
                .set_file_name(default_name)
                .save_file()
            {
                let out = if out.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false) {
                    out
                } else {
                    out.with_extension("png")
                };
                self.status = format!("Exporting image to {}…", out.display());
                self.engine.export_object_image(src_page, src_oi, out);
            }
        }
        if remove {
            if gi < self.object_dupes.len() {
                self.object_dupes.remove(gi);
            }
            self.selected_dupe = None;
        } else if let Some((p, o)) = select_source {
            self.selected_dupe = None;
            self.selected_object = Some((p, o));
            self.object_details = None;
            self.engine.request_object_details(p, o, self.generation);
        }
    }

    fn chars_in_rect(&self, page: usize, rect: (f32, f32, f32, f32)) -> Vec<usize> {
        let Some(chars) = self.page_text.get(&page) else { return Vec::new() };
        let (rx, ry, rw, rh) = rect;
        let x1 = rx + rw;
        let y1 = ry + rh;
        let mut out = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            let intersects = c.x < x1 && c.x + c.width > rx && c.y < y1 && c.y + c.height > ry;
            if intersects {
                out.push(i);
            }
        }
        out
    }

    fn drag_rect(td: &TextDrag) -> (f32, f32, f32, f32) {
        let x0 = td.start_page_pt.x.min(td.current_page_pt.x);
        let x1 = td.start_page_pt.x.max(td.current_page_pt.x);
        let y0 = td.start_page_pt.y.min(td.current_page_pt.y);
        let y1 = td.start_page_pt.y.max(td.current_page_pt.y);
        (x0, y0, x1 - x0, y1 - y0)
    }

    fn recompute_text_selection(&mut self) {
        if let Some(td) = self.text_drag.clone() {
            let rect = Self::drag_rect(&td);
            let chars = self.chars_in_rect(td.page, rect);
            self.text_selection = Some(TextSelection { page: td.page, rect, chars });
        } else if let Some((page, rect, was_empty)) = self
            .text_selection
            .as_ref()
            .map(|s| (s.page, s.rect, s.chars.is_empty()))
        {
            if was_empty {
                let chars = self.chars_in_rect(page, rect);
                if !chars.is_empty() {
                    if let Some(sel) = self.text_selection.as_mut() {
                        sel.chars = chars;
                    }
                }
            }
        }
    }

    fn selected_text_string(&self) -> Option<String> {
        let sel = self.text_selection.as_ref()?;
        let chars = self.page_text.get(&sel.page)?;
        let mut s = String::new();
        let mut prev: Option<&PageTextChar> = None;
        for &i in &sel.chars {
            let Some(c) = chars.get(i) else { continue };
            if let Some(p) = prev {
                let line_height = p.height.max(c.height).max(1.0);
                let prev_baseline = p.y + p.height;
                let cur_baseline = c.y + c.height;
                let dy = cur_baseline - prev_baseline;
                let crossed_line = dy.abs() > line_height * 0.5;
                if crossed_line {
                    if s.ends_with(' ') {
                        s.pop();
                    }
                    s.push('\n');
                    if c.ch == ' ' {
                        prev = Some(c);
                        continue;
                    }
                }
            }
            s.push(c.ch);
            prev = Some(c);
        }
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn set_text_editing(&mut self, new: Option<usize>) {
        let mut new = new;
        if let Some(old) = self.text_editing {
            if Some(old) != new {
                let should_delete = self.edits.get(old).map_or(false, |e| {
                    e.kind == OverlayKind::Text
                        && e.text.as_ref().map_or(true, |s| s.trim().is_empty())
                });
                if should_delete {
                    self.edits.remove(old);
                    self.selected = match self.selected {
                        Some(s) if s == old => None,
                        Some(s) if s > old => Some(s - 1),
                        s => s,
                    };
                    new = match new {
                        Some(n) if n > old => Some(n - 1),
                        Some(n) if n == old => None,
                        n => n,
                    };
                }
            }
        }
        if self.text_editing != new {
            self.text_editing = new;
            self.text_editing_just_started = new.is_some();
        }
    }

    fn apply_eyedropper_sample(&mut self, page: usize, page_rect: Rect, pp: Pos2) {
        let Some(target) = self.eyedropper else { return };
        let Some(slot) = self.pages.get(page) else { return };
        let Some(snapshot) = slot.rgba.as_ref() else {
            self.status = "Page still rendering. Try the eyedropper again in a moment.".into();
            return;
        };
        let u = ((pp.x - page_rect.min.x) / page_rect.width()).clamp(0.0, 1.0);
        let v = ((pp.y - page_rect.min.y) / page_rect.height()).clamp(0.0, 1.0);
        let px = ((u * snapshot.width as f32) as u32).min(snapshot.width.saturating_sub(1));
        let py = ((v * snapshot.height as f32) as u32).min(snapshot.height.saturating_sub(1));
        let i = ((py * snapshot.width + px) as usize) * 4;
        if i + 2 >= snapshot.data.len() {
            return;
        }
        let c = [snapshot.data[i], snapshot.data[i + 1], snapshot.data[i + 2]];
        match target {
            EyedropperTarget::OverlayText(idx) => {
                if let Some(e) = self.edits.get_mut(idx) {
                    e.color = Some(c);
                }
            }
            EyedropperTarget::ObjectFill(p, oi) => {
                let key = (p, oi);
                let mut e = self.object_edits.get(&key).cloned().unwrap_or_else(|| ObjectEdit::new(p, oi));
                e.fill_color = Some(c);
                if e.fill_alpha.is_none() {
                    e.fill_alpha = Some(255);
                }
                self.object_edits.insert(key, e);
                self.flush_object_edits();
            }
            EyedropperTarget::ObjectStroke(p, oi) => {
                let key = (p, oi);
                let mut e = self.object_edits.get(&key).cloned().unwrap_or_else(|| ObjectEdit::new(p, oi));
                e.stroke_color = Some(c);
                if e.stroke_alpha.is_none() {
                    e.stroke_alpha = Some(255);
                }
                self.object_edits.insert(key, e);
                self.flush_object_edits();
            }
        }
        push_recent_color(&mut self.recent_colors, c);
        self.eyedropper = None;
        self.status = format!("Picked #{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    }

    fn picks_at(&self, page: usize, page_rect: Rect, zoom: f32, screen: Pos2) -> Vec<Pick> {
        let mut picks: Vec<Pick> = Vec::new();
        let Some(list) = self.objects.get(&page) else { return picks };
        let mut dupe_hits: Vec<usize> = self
            .object_dupes
            .iter()
            .enumerate()
            .filter(|(_, d)| d.page_index == page)
            .filter_map(|(gi, d)| {
                let o = list.iter().find(|o| o.object_index == d.object_index)?;
                point_in_object(page_rect, zoom, o, Some(d), screen).then_some(gi)
            })
            .collect();
        dupe_hits.reverse();
        picks.extend(dupe_hits.into_iter().map(Pick::Dupe));
        picks.extend(
            objects_at(list, &self.object_edits, page, page_rect, zoom, screen)
                .into_iter()
                .map(Pick::Orig),
        );
        picks
    }

    fn all_object_edits(&self) -> Vec<ObjectEdit> {
        self.object_edits
            .values()
            .filter(|e| !e.is_noop())
            .cloned()
            .chain(self.object_dupes.iter().cloned())
            .collect()
    }

    fn flush_object_edits(&mut self) {
        self.object_edits.retain(|_, e| !e.is_noop());
        let mut new_pages: HashSet<usize> = self.object_edits.keys().map(|(p, _)| *p).collect();
        new_pages.extend(self.object_dupes.iter().map(|d| d.page_index));
        let affected: Vec<usize> = self.pages_with_object_edits.union(&new_pages).copied().collect();
        for p in affected {
            if let Some(slot) = self.pages.get_mut(p) {
                slot.texture = None;
                slot.tex_scale = 0.0;
                slot.rgba = None;
            }
            self.pending.remove(&p);
            if let Some(slot) = self.thumbs.get_mut(p) {
                *slot = None;
            }
            self.thumb_pending.remove(&p);
        }
        self.pages_with_object_edits = new_pages;
        self.engine.set_object_edits(self.all_object_edits());
    }
}

impl eframe::App for PaperApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.ensure_embedded_fonts_registered(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            if self.has_unexported_changes() && !self.allow_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = true;
            }
        }
        self.confirm_close_dialog(ctx);
        self.confirm_open_dialog(ctx);
        self.about_dialog(ctx);
        self.settings_dialog(ctx);

        if let Some(path) = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .find(|p| p.extension().map_or(false, |e| e.eq_ignore_ascii_case("pdf")))
        }) {
            self.request_open(path);
        }

        if let Some(path) = crate::file_open::take_pending().into_iter().last() {
            self.request_open(path);
        }

        if self.tool == Tool::Objects {
            self.selected = None;
            self.drag = None;
        } else {
            self.selected_object = None;
            self.selected_dupe = None;
            self.object_details = None;
        }
        if self.tool != Tool::Select {
            self.text_drag = None;
            self.text_selection = None;
        }

        let copy_pressed = ctx.input(|i| {
            let cmd = i.modifiers.command || i.modifiers.ctrl;
            (cmd && i.key_pressed(Key::C))
                || i.events.iter().any(|e| matches!(e, egui::Event::Copy))
        });
        if copy_pressed {
            if let Some(s) = self.selected_text_string() {
                let count = s.chars().count();
                ctx.output_mut(|o| o.copied_text = s);
                self.status = format!("Copied {} char(s)", count);
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy)));
            } else if let Some(sel) = &self.text_selection {
                if self.page_text.contains_key(&sel.page) {
                    self.status = "Nothing to copy. That area has no text on this page.".into();
                } else {
                    self.status = "Still loading page text. Try Ctrl+C again in a moment.".into();
                }
            }
        }

        let before: Option<EditState> =
            if self.history.pending.is_none() { Some(self.edit_state()) } else { None };
        let object_before = self.object_edits.clone();
        let dupes_before = self.object_dupes.clone();

        self.top_bar(ctx);
        self.status_bar(ctx);
        self.properties_panel(ctx);
        self.object_inspector_panel(ctx);
        self.dupe_inspector_panel(ctx);
        self.thumbnail_panel(ctx);
        self.central(ctx);
        self.preview_window(ctx);

        let busy = self.drag.is_some()
            || self.object_drag.is_some()
            || ctx.input(|i| i.pointer.primary_down())
            || ctx.memory(|m| m.focused().is_some());

        if self.history.navigated {
            self.history.navigated = false;
        } else {
            let now = self.edit_state();
            if let Some(b) = before {
                if b != now {
                    self.history.note_change(b);
                }
            }
            if !busy {
                self.history.commit(&now);
            }
        }

        if object_before != self.object_edits || dupes_before != self.object_dupes {
            self.object_edits_dirty = true;
        }
        if self.object_edits_dirty && !busy {
            self.flush_object_edits();
            self.object_edits_dirty = false;
        }

        if !self.image_cache.is_empty() {
            let live: HashSet<usize> = self
                .edits
                .iter()
                .filter_map(|e| e.image_data.as_ref().map(|a| Arc::as_ptr(a) as usize))
                .collect();
            self.image_cache.retain(|k, _| live.contains(k));
        }

        if !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }
}

fn decode_image_texture(ctx: &egui::Context, data: &[u8]) -> TextureHandle {
    let make = |ci: ColorImage| ctx.load_texture("img-overlay", ci, TextureOptions::LINEAR);
    match image::load_from_memory(data) {
        Ok(img) => {
            let img = if img.width().max(img.height()) > 2048 {
                img.resize(2048, 2048, image::imageops::FilterType::Triangle)
            } else {
                img
            };
            let rgba = img.to_rgba8();
            make(ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            ))
        }
        Err(_) => ctx.load_texture(
            "img-overlay-bad",
            ColorImage::new([1, 1], Color32::from_rgb(255, 0, 255)),
            TextureOptions::NEAREST,
        ),
    }
}

fn object_color(kind: ObjectKind) -> Color32 {
    match kind {
        ObjectKind::Text => Color32::from_rgb(40, 110, 220),
        ObjectKind::Image => Color32::from_rgb(30, 150, 70),
        ObjectKind::Path => Color32::from_rgb(210, 120, 30),
        ObjectKind::Shading => Color32::from_rgb(150, 60, 180),
        ObjectKind::Form => Color32::from_rgb(20, 150, 160),
        ObjectKind::Other => Color32::from_gray(120),
    }
}

fn object_kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Text => "text",
        ObjectKind::Image => "image",
        ObjectKind::Path => "path",
        ObjectKind::Shading => "shading",
        ObjectKind::Form => "form",
        ObjectKind::Other => "other",
    }
}

fn object_transform(obj: &ObjectInfo, edit: Option<&ObjectEdit>) -> (Pos2, Vec2, f32) {
    let (dx, dy, sx, sy, deg) =
        edit.map_or((0.0, 0.0, 1.0, 1.0, 0.0), |e| (e.dx, e.dy, e.scale_x.abs().max(0.01), e.scale_y.abs().max(0.01), e.rotation));
    let center = Pos2::new(obj.x + obj.width / 2.0 + dx, obj.y + obj.height / 2.0 + dy);
    let half = Vec2::new((obj.width * sx / 2.0).max(0.25), (obj.height * sy / 2.0).max(0.25));
    (center, half, deg.to_radians())
}

fn object_local_to_screen(page_rect: Rect, zoom: f32, center: Pos2, angle: f32, lp: Vec2) -> Pos2 {
    let p = rot(lp, angle);
    page_pt_to_screen(page_rect, zoom, Pos2::new(center.x + p.x, center.y + p.y))
}

fn object_quad_screen(page_rect: Rect, zoom: f32, obj: &ObjectInfo, edit: Option<&ObjectEdit>) -> [Pos2; 4] {
    let (c, h, a) = object_transform(obj, edit);
    [
        object_local_to_screen(page_rect, zoom, c, a, Vec2::new(-h.x, -h.y)),
        object_local_to_screen(page_rect, zoom, c, a, Vec2::new(h.x, -h.y)),
        object_local_to_screen(page_rect, zoom, c, a, Vec2::new(h.x, h.y)),
        object_local_to_screen(page_rect, zoom, c, a, Vec2::new(-h.x, h.y)),
    ]
}

fn object_handle_screen(page_rect: Rect, zoom: f32, obj: &ObjectInfo, edit: Option<&ObjectEdit>, h: Handle) -> Pos2 {
    let (c, half, a) = object_transform(obj, edit);
    let lp = match h {
        Handle::TopLeft => Vec2::new(-half.x, -half.y),
        Handle::TopRight => Vec2::new(half.x, -half.y),
        Handle::BottomLeft => Vec2::new(-half.x, half.y),
        Handle::BottomRight => Vec2::new(half.x, half.y),
        Handle::Rotate => Vec2::new(0.0, -half.y - ROTATE_OFFSET_PX / zoom),
        Handle::Move => Vec2::ZERO,
    };
    object_local_to_screen(page_rect, zoom, c, a, lp)
}

fn hit_object_handle(page_rect: Rect, zoom: f32, obj: &ObjectInfo, edit: Option<&ObjectEdit>, screen: Pos2) -> Option<Handle> {
    if object_handle_screen(page_rect, zoom, obj, edit, Handle::Rotate).distance(screen) <= HANDLE_HIT_PX + 2.0 {
        return Some(Handle::Rotate);
    }
    let (c, half, a) = object_transform(obj, edit);
    let pp = (screen - page_rect.min) / zoom;
    let lp = rot(Vec2::new(pp.x - c.x, pp.y - c.y), -a);
    let tol = HANDLE_HIT_PX / zoom;
    for (h, corner) in [
        (Handle::TopLeft, Vec2::new(-half.x, -half.y)),
        (Handle::TopRight, Vec2::new(half.x, -half.y)),
        (Handle::BottomLeft, Vec2::new(-half.x, half.y)),
        (Handle::BottomRight, Vec2::new(half.x, half.y)),
    ] {
        if (lp - corner).length() <= tol {
            return Some(h);
        }
    }
    (lp.x.abs() <= half.x && lp.y.abs() <= half.y).then_some(Handle::Move)
}

fn point_in_object(page_rect: Rect, zoom: f32, obj: &ObjectInfo, edit: Option<&ObjectEdit>, screen: Pos2) -> bool {
    let (c, half, a) = object_transform(obj, edit);
    let pp = (screen - page_rect.min) / zoom;
    let lp = rot(Vec2::new(pp.x - c.x, pp.y - c.y), -a);
    lp.x.abs() <= half.x && lp.y.abs() <= half.y
}

fn topmost_object_at(
    list: &[ObjectInfo],
    edits: &HashMap<(usize, usize), ObjectEdit>,
    page: usize,
    page_rect: Rect,
    zoom: f32,
    screen: Pos2,
) -> Option<usize> {
    list.iter()
        .filter(|o| {
            let edit = edits.get(&(page, o.object_index));
            !edit.map_or(false, |e| e.delete) && point_in_object(page_rect, zoom, o, edit, screen)
        })
        .map(|o| o.object_index)
        .max()
}

fn objects_at(
    list: &[ObjectInfo],
    edits: &HashMap<(usize, usize), ObjectEdit>,
    page: usize,
    page_rect: Rect,
    zoom: f32,
    screen: Pos2,
) -> Vec<usize> {
    let mut hits: Vec<usize> = list
        .iter()
        .filter(|o| {
            let edit = edits.get(&(page, o.object_index));
            !edit.map_or(false, |e| e.delete) && point_in_object(page_rect, zoom, o, edit, screen)
        })
        .map(|o| o.object_index)
        .collect();
    hits.sort_unstable_by(|a, b| b.cmp(a));
    hits
}

fn edit_size_row(
    ui: &mut egui::Ui,
    salt: &str,
    width: &mut f32,
    height: &mut f32,
    ref_w: f32,
    ref_h: f32,
    unit_pct: bool,
    linked: bool,
) {
    egui::Grid::new(salt).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        let aspect = if *height > 0.0 { *width / *height } else { 1.0 };
        ui.label("Width");
        let mut w_disp = if unit_pct { *width / ref_w * 100.0 } else { *width };
        let resp_w = ui.add(
            egui::DragValue::new(&mut w_disp)
                .speed(if unit_pct { 0.5 } else { 0.5 })
                .range(0.1..=f32::INFINITY)
                .suffix(if unit_pct { " %" } else { " pt" }),
        );
        if resp_w.changed() {
            let new_w = if unit_pct { (w_disp / 100.0) * ref_w } else { w_disp };
            *width = new_w.max(MIN_EDIT_SIZE);
            if linked && aspect.is_finite() && aspect > 0.0 {
                *height = (*width / aspect).max(MIN_EDIT_SIZE);
            }
        }
        ui.end_row();

        ui.label("Height");
        let mut h_disp = if unit_pct { *height / ref_h * 100.0 } else { *height };
        let resp_h = ui.add(
            egui::DragValue::new(&mut h_disp)
                .speed(if unit_pct { 0.5 } else { 0.5 })
                .range(0.1..=f32::INFINITY)
                .suffix(if unit_pct { " %" } else { " pt" }),
        );
        if resp_h.changed() {
            let new_h = if unit_pct { (h_disp / 100.0) * ref_h } else { h_disp };
            *height = new_h.max(MIN_EDIT_SIZE);
            if linked && aspect.is_finite() && aspect > 0.0 {
                *width = (*height * aspect).max(MIN_EDIT_SIZE);
            }
        }
        ui.end_row();
    });
}

fn apply_object_drag(edit: &mut ObjectEdit, d: &ObjectDrag, page_pt: Pos2, aspect_lock: bool) {
    let cx = d.obj_x + d.obj_w / 2.0 + d.orig.dx;
    let cy = d.obj_y + d.obj_h / 2.0 + d.orig.dy;
    match d.handle {
        Handle::Move => {
            edit.dx = d.orig.dx + (page_pt.x - d.start_page_pt.x);
            edit.dy = d.orig.dy + (page_pt.y - d.start_page_pt.y);
        }
        Handle::Rotate => {
            let v = Vec2::new(page_pt.x - cx, page_pt.y - cy);
            if v.length() > 1.0 {
                let deg = v.x.atan2(-v.y).to_degrees();
                if deg.is_finite() {
                    edit.rotation = deg;
                }
            }
        }
        _ => {
            let lp = rot(Vec2::new(page_pt.x - cx, page_pt.y - cy), -d.orig.rotation.to_radians());
            let half_x = lp.x.abs().max(MIN_EDIT_SIZE / 2.0);
            let half_y = lp.y.abs().max(MIN_EDIT_SIZE / 2.0);
            let mut sx = (2.0 * half_x / d.obj_w.max(0.5)).max(0.01);
            let mut sy = (2.0 * half_y / d.obj_h.max(0.5)).max(0.01);
            if aspect_lock {
                let s = sx.max(sy);
                sx = s;
                sy = s;
            }
            edit.scale_x = sx;
            edit.scale_y = sy;
        }
    }
}

fn dupe_color_row(
    ui: &mut egui::Ui,
    label: &str,
    color: &mut Option<[u8; 3]>,
    alpha: &mut Option<u8>,
) {
    ui.horizontal(|ui| {
        let mut on = color.is_some();
        if ui.checkbox(&mut on, label).changed() {
            if on {
                *color = Some([0, 0, 0]);
                if alpha.is_none() {
                    *alpha = Some(255);
                }
            } else {
                *color = None;
                *alpha = None;
            }
        }
        if let Some(c) = color.as_mut() {
            ui.color_edit_button_srgb(c);
            let mut a = alpha.unwrap_or(255);
            if ui
                .add(egui::DragValue::new(&mut a).speed(1.0).range(0..=255).prefix("α "))
                .changed()
            {
                *alpha = Some(a);
            }
        }
    });
}

fn optional_color_editor(
    ui: &mut egui::Ui,
    label: &str,
    color: &mut Option<[u8; 3]>,
    alpha: &mut Option<u8>,
    fallback: [u8; 3],
    recents: &mut VecDeque<[u8; 3]>,
    eyedropper_active: bool,
) -> ColorFieldOutcome {
    let mut outcome = ColorFieldOutcome::Unchanged;
    ui.horizontal(|ui| {
        let mut on = color.is_some();
        if ui.checkbox(&mut on, label).changed() {
            if on {
                *color = Some(fallback);
                if alpha.is_none() {
                    *alpha = Some(255);
                }
            } else {
                *color = None;
                *alpha = None;
            }
        }
        if let Some(c) = color.as_mut() {
            if ui.color_edit_button_srgb(c).changed() {
                push_recent_color(recents, *c);
                outcome = ColorFieldOutcome::Changed;
            }
            if eyedropper_button(ui, eyedropper_active).clicked() {
                outcome = ColorFieldOutcome::ToggleEyedropper;
            }
        }
    });
    if let Some(_) = *color {
        let mut a = alpha.unwrap_or(255);
        if ui.add(egui::Slider::new(&mut a, 0u8..=255).text("opacity")).changed() {
            *alpha = Some(a);
        }
        let chosen = recent_color_strip(ui, recents);
        if let Some(c) = chosen {
            *color = Some(c);
            push_recent_color(recents, c);
            outcome = ColorFieldOutcome::Changed;
        }
    }
    outcome
}

fn color_field(
    ui: &mut egui::Ui,
    color: &mut [u8; 3],
    recents: &mut VecDeque<[u8; 3]>,
    eyedropper_active: bool,
) -> ColorFieldOutcome {
    let mut outcome = ColorFieldOutcome::Unchanged;
    ui.horizontal(|ui| {
        if ui.color_edit_button_srgb(color).changed() {
            push_recent_color(recents, *color);
            outcome = ColorFieldOutcome::Changed;
        }
        if eyedropper_button(ui, eyedropper_active).clicked() {
            outcome = ColorFieldOutcome::ToggleEyedropper;
        }
    });
    let chosen = recent_color_strip(ui, recents);
    if let Some(c) = chosen {
        *color = c;
        push_recent_color(recents, c);
        outcome = ColorFieldOutcome::Changed;
    }
    outcome
}

fn eyedropper_button(ui: &mut egui::Ui, active: bool) -> egui::Response {
    let icon = if active { ph::EYEDROPPER_SAMPLE } else { ph::EYEDROPPER };
    let btn = egui::Button::new(icon).selected(active);
    let resp = ui.add(btn);
    if active {
        resp.on_hover_text("Eyedropper armed. Click a pixel on the page (or click here to cancel)")
    } else {
        resp.on_hover_text("Eyedropper: sample a colour from the page")
    }
}

fn recent_color_strip(
    ui: &mut egui::Ui,
    recents: &mut VecDeque<[u8; 3]>,
) -> Option<[u8; 3]> {
    if recents.is_empty() {
        return None;
    }
    let mut chosen: Option<[u8; 3]> = None;
    let mut clear = false;
    let mut remove_at: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        ui.small("Recent:");
        for (i, &c) in recents.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            painter.rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(120)));
            let resp = resp.on_hover_text(format!("#{:02x}{:02x}{:02x}, right-click for options", c[0], c[1], c[2]));
            if resp.clicked() {
                chosen = Some(c);
            }
            resp.context_menu(|ui| {
                ui.label(format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]));
                ui.separator();
                if ui.button(format!("{}  Remove", ph::X)).clicked() {
                    remove_at = Some(i);
                    ui.close_menu();
                }
                if ui.button(format!("{}  Clear all", ph::TRASH)).clicked() {
                    clear = true;
                    ui.close_menu();
                }
            });
        }
        if ui
            .small_button(ph::TRASH)
            .on_hover_text("Clear recent colours")
            .clicked()
        {
            clear = true;
        }
    });
    if clear {
        recents.clear();
    } else if let Some(i) = remove_at {
        if i < recents.len() {
            recents.remove(i);
        }
    }
    chosen
}

fn color_row(ui: &mut egui::Ui, label: &str, c: [u8; 3], alpha: Option<u8>) {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
        ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(120)));
        ui.monospace(format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]));
        if let Some(a) = alpha {
            if a != 255 {
                ui.label(format!("α {a}"));
            }
        }
    });
}

fn setup_fonts(ctx: &egui::Context) -> (egui::FontDefinitions, Option<egui::FontFamily>) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    const SERIF_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        "/usr/share/fonts/dejavu/DejaVuSerif.ttf",
        "/usr/share/fonts/TTF/DejaVuSerif.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/liberation/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/freefont/FreeSerif.ttf",
        "/usr/share/fonts/noto/NotoSerif-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-Regular.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
        "/Library/Fonts/Times New Roman.ttf",
        "/System/Library/Fonts/Supplemental/Georgia.ttf",
        "/System/Library/Fonts/NewYork.ttf",
        "C:\\Windows\\Fonts\\times.ttf",
        "C:\\Windows\\Fonts\\georgia.ttf",
    ];
    let serif = SERIF_CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()).map(|bytes| {
        fonts.font_data.insert("paper-serif".to_owned(), egui::FontData::from_owned(bytes));
        fonts
            .families
            .entry(egui::FontFamily::Name("serif".into()))
            .or_default()
            .push("paper-serif".to_owned());
        egui::FontFamily::Name("serif".into())
    });

    ctx.set_fonts(fonts.clone());
    (fonts, serif)
}

fn font_choice_label(f: &EmbeddedFontInfo) -> String {
    let mut label = f.base_font.clone();
    if f.is_subset {
        label.push_str("  (subset)");
    }
    if !f.is_simple {
        label.push_str(&format!("  [{}]", f.subtype));
    }
    label
}

fn preview_font_family(name: Option<&str>, serif: Option<&egui::FontFamily>) -> egui::FontFamily {
    match name {
        Some("Courier") => egui::FontFamily::Monospace,
        Some("Times-Roman") | Some("Times Roman") => serif.cloned().unwrap_or(egui::FontFamily::Proportional),
        _ => egui::FontFamily::Proportional,
    }
}

fn overlay_preview_family(
    edit: &OverlayEdit,
    registered: &HashMap<(u32, u16), egui::FontFamily>,
    serif: Option<&egui::FontFamily>,
) -> egui::FontFamily {
    if let Some(id) = edit.font_embedded_id {
        if let Some(family) = registered.get(&id) {
            return family.clone();
        }
    }
    preview_font_family(edit.font_family.as_deref(), serif)
}


fn rot(v: Vec2, angle_rad: f32) -> Vec2 {
    let (s, c) = angle_rad.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

fn new_text_click_pos(x: f32, y: f32) -> (f32, f32) {
    const BASELINE_OFFSET: f32 = 18.0;
    (x, (y - BASELINE_OFFSET).max(0.0))
}

fn local_to_page(edit: &OverlayEdit, lp: Vec2) -> Pos2 {
    let cx = edit.width / 2.0;
    let cy = edit.height / 2.0;
    let mut centered = lp - Vec2::new(cx, cy);
    if edit.flip_horizontal {
        centered.x = -centered.x;
    }
    if edit.flip_vertical {
        centered.y = -centered.y;
    }
    let pivot = Vec2::new(edit.x + cx, edit.y + cy);
    let p = pivot + rot(centered, edit.rotation.to_radians());
    Pos2::new(p.x, p.y)
}

fn page_to_local(edit: &OverlayEdit, page_pt: Pos2) -> Vec2 {
    let cx = edit.width / 2.0;
    let cy = edit.height / 2.0;
    let pivot = Vec2::new(edit.x + cx, edit.y + cy);
    let rel = Vec2::new(page_pt.x, page_pt.y) - pivot;
    let mut un = rot(rel, -edit.rotation.to_radians());
    if edit.flip_horizontal {
        un.x = -un.x;
    }
    if edit.flip_vertical {
        un.y = -un.y;
    }
    un + Vec2::new(cx, cy)
}

fn page_pt_to_screen(page_rect: Rect, zoom: f32, page_pt: Pos2) -> Pos2 {
    page_rect.min + Vec2::new(page_pt.x, page_pt.y) * zoom
}

fn local_corners(edit: &OverlayEdit) -> [Vec2; 4] {
    [
        Vec2::new(0.0, 0.0),
        Vec2::new(edit.width, 0.0),
        Vec2::new(0.0, edit.height),
        Vec2::new(edit.width, edit.height),
    ]
}

fn hit_handle(edit: &OverlayEdit, page_rect: Rect, zoom: f32, screen: Pos2) -> Option<Handle> {
    let hit_pts = HANDLE_HIT_PX / zoom;
    let rotate_local = Vec2::new(edit.width / 2.0, -ROTATE_OFFSET_PX / zoom);
    let rotate_screen = page_pt_to_screen(page_rect, zoom, local_to_page(edit, rotate_local));
    if rotate_screen.distance(screen) <= HANDLE_HIT_PX + 2.0 {
        return Some(Handle::Rotate);
    }
    let lp = page_to_local(edit, Pos2::new((screen.x - page_rect.min.x) / zoom, (screen.y - page_rect.min.y) / zoom));
    for (h, c) in [
        (Handle::TopLeft, Vec2::new(0.0, 0.0)),
        (Handle::TopRight, Vec2::new(edit.width, 0.0)),
        (Handle::BottomLeft, Vec2::new(0.0, edit.height)),
        (Handle::BottomRight, Vec2::new(edit.width, edit.height)),
    ] {
        if (lp - c).length() <= hit_pts {
            return Some(h);
        }
    }
    None
}

fn point_in_edit(edit: &OverlayEdit, page_rect: Rect, zoom: f32, screen: Pos2) -> bool {
    let lp = page_to_local(edit, Pos2::new((screen.x - page_rect.min.x) / zoom, (screen.y - page_rect.min.y) / zoom));
    lp.x >= 0.0 && lp.y >= 0.0 && lp.x <= edit.width && lp.y <= edit.height
}

fn topmost_edit_at(edits: &[OverlayEdit], page: usize, page_rect: Rect, zoom: f32, screen: Pos2) -> Option<usize> {
    edits
        .iter()
        .enumerate()
        .filter(|(_, e)| e.page_index == page && point_in_edit(e, page_rect, zoom, screen))
        .map(|(i, _)| i)
        .last()
}

fn apply_drag(edit: &mut OverlayEdit, d: &Drag, page_pt: Pos2, aspect_lock: bool) {
    match d.handle {
        Handle::Move => {
            let delta = page_pt - d.start_page_pt;
            edit.x = d.orig.x + delta.x;
            edit.y = d.orig.y + delta.y;
        }
        Handle::Rotate => {
            let pivot = Vec2::new(d.orig.x + d.orig.width / 2.0, d.orig.y + d.orig.height / 2.0);
            let v = Vec2::new(page_pt.x, page_pt.y) - pivot;
            if v.length() > 1.0 {
                let mut deg = v.x.atan2(-v.y).to_degrees();
                if deg.is_nan() {
                    deg = 0.0;
                }
                edit.rotation = deg;
            }
        }
        corner => {
            let opp = match corner {
                Handle::TopLeft => Vec2::new(d.orig.width, d.orig.height),
                Handle::TopRight => Vec2::new(0.0, d.orig.height),
                Handle::BottomLeft => Vec2::new(d.orig.width, 0.0),
                Handle::BottomRight => Vec2::new(0.0, 0.0),
                _ => unreachable!(),
            };
            let opp_page = local_to_page(&d.orig, opp);
            let lp = page_to_local(&d.orig, page_pt);
            let mut new_w = (lp.x - opp.x).abs().max(MIN_EDIT_SIZE);
            let mut new_h = (lp.y - opp.y).abs().max(MIN_EDIT_SIZE);
            if aspect_lock && d.orig.width > 0.0 && d.orig.height > 0.0 {
                let fx = new_w / d.orig.width;
                let fy = new_h / d.orig.height;
                let factor = fx.max(fy);
                new_w = (factor * d.orig.width).max(MIN_EDIT_SIZE);
                new_h = (factor * d.orig.height).max(MIN_EDIT_SIZE);
            }
            edit.width = new_w;
            edit.height = new_h;
            let opp_new_local = match corner {
                Handle::TopLeft => Vec2::new(new_w, new_h),
                Handle::TopRight => Vec2::new(0.0, new_h),
                Handle::BottomLeft => Vec2::new(new_w, 0.0),
                Handle::BottomRight => Vec2::new(0.0, 0.0),
                _ => unreachable!(),
            };
            let mut probe = edit.clone();
            probe.x = d.orig.x;
            probe.y = d.orig.y;
            let cur = local_to_page(&probe, opp_new_local);
            edit.x = d.orig.x + (opp_page.x - cur.x);
            edit.y = d.orig.y + (opp_page.y - cur.y);
        }
    }
}

fn cursor_for(h: Handle) -> CursorIcon {
    match h {
        Handle::Move => CursorIcon::Grab,
        Handle::Rotate => CursorIcon::Alias,
        Handle::TopLeft | Handle::BottomRight => CursorIcon::ResizeNwSe,
        Handle::TopRight | Handle::BottomLeft => CursorIcon::ResizeNeSw,
    }
}


fn draw_edit(
    ui: &egui::Ui,
    page_rect: Rect,
    zoom: f32,
    edit: &OverlayEdit,
    selected: bool,
    image_cache: &HashMap<usize, TextureHandle>,
    serif: Option<&egui::FontFamily>,
    registered_embedded: &HashMap<(u32, u16), egui::FontFamily>,
) {
    const ACCENT: Color32 = Color32::from_rgb(40, 110, 220);
    let content = ui.painter_at(page_rect);
    let chrome = ui.painter_at(page_rect.expand(ROTATE_OFFSET_PX + HANDLE_PX + 6.0));

    let screen_corner = |c: Vec2| page_pt_to_screen(page_rect, zoom, local_to_page(edit, c));
    let [tl, tr, bl, br] = local_corners(edit);
    let poly = vec![screen_corner(tl), screen_corner(tr), screen_corner(br), screen_corner(bl)];

    match edit.kind {
        OverlayKind::Text => {
            let c = edit.color.unwrap_or([17, 24, 39]);
            let color = Color32::from_rgb(c[0], c[1], c[2]);
            let size = (edit.font_size.unwrap_or(18.0) * zoom).max(1.0);
            let font_id = egui::FontId::new(size, overlay_preview_family(edit, registered_embedded, serif));
            let galley = ui.fonts(|f| {
                f.layout(edit.text.clone().unwrap_or_default(), font_id, color, edit.width * zoom)
            });
            let mut unflipped = edit.clone();
            unflipped.flip_horizontal = false;
            unflipped.flip_vertical = false;
            let tl_unflipped =
                page_pt_to_screen(page_rect, zoom, local_to_page(&unflipped, Vec2::ZERO));
            let mut ts = epaint::TextShape::new(tl_unflipped, galley, color);
            ts.angle = edit.rotation.to_radians();
            if !edit.flip_horizontal && !edit.flip_vertical {
                content.add(ts);
            } else {
                let pivot = page_pt_to_screen(
                    page_rect,
                    zoom,
                    local_to_page(&unflipped, Vec2::new(edit.width / 2.0, edit.height / 2.0)),
                );
                let (font_tex_size, prepared_discs) = ui.ctx().fonts(|f| {
                    let atlas = f.texture_atlas();
                    let atlas = atlas.lock();
                    (atlas.size(), atlas.prepared_discs())
                });
                let mut tessellator = epaint::Tessellator::new(
                    ui.ctx().pixels_per_point(),
                    epaint::TessellationOptions::default(),
                    font_tex_size,
                    prepared_discs,
                );
                let mut mesh = epaint::Mesh::default();
                tessellator.tessellate_text(&ts, &mut mesh);
                for v in &mut mesh.vertices {
                    if edit.flip_horizontal {
                        v.pos.x = 2.0 * pivot.x - v.pos.x;
                    }
                    if edit.flip_vertical {
                        v.pos.y = 2.0 * pivot.y - v.pos.y;
                    }
                }
                if edit.flip_horizontal ^ edit.flip_vertical {
                    for chunk in mesh.indices.chunks_exact_mut(3) {
                        chunk.swap(1, 2);
                    }
                }
                content.add(epaint::Shape::Mesh(mesh));
            }
        }
        OverlayKind::Image => {
            let tex = edit
                .image_data
                .as_ref()
                .and_then(|d| image_cache.get(&(Arc::as_ptr(d) as usize)));
            if let Some(tex) = tex {
                let mut mesh = egui::Mesh::with_texture(tex.id());
                let uvs = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
                for (p, uv) in poly.iter().zip(uvs) {
                    mesh.vertices.push(epaint::Vertex { pos: *p, uv, color: Color32::WHITE });
                }
                mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                content.add(Shape::mesh(mesh));
            } else {
                content.add(Shape::convex_polygon(poly.clone(), Color32::from_gray(225), Stroke::NONE));
                content.text(poly[0], Align2::LEFT_TOP, "image", FontId::proportional(12.0), Color32::from_gray(110));
            }
        }
    }

    let outline = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(40, 110, 220, 120))
    };
    chrome.add(Shape::closed_line(poly, outline));

    if selected {
        for c in [tl, tr, bl, br] {
            let p = screen_corner(c);
            let r = Rect::from_center_size(p, Vec2::splat(HANDLE_PX * 2.0));
            chrome.rect_filled(r, 1.0, Color32::WHITE);
            chrome.rect_stroke(r, 1.0, Stroke::new(1.2, ACCENT));
        }
        let top_mid = screen_corner(Vec2::new(edit.width / 2.0, 0.0));
        let rot_h = screen_corner(Vec2::new(edit.width / 2.0, -ROTATE_OFFSET_PX / zoom));
        chrome.line_segment([top_mid, rot_h], Stroke::new(1.2, ACCENT));
        chrome.circle_filled(rot_h, HANDLE_PX, Color32::WHITE);
        chrome.circle_stroke(rot_h, HANDLE_PX, Stroke::new(1.2, ACCENT));
    }
}


fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
