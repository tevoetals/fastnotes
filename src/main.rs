//! Fast Notes — notas rápidas em Rust puro falando Wayland diretamente.
//! Renderização por software (wl_shm, CPU só quando algo muda), texto via
//! cosmic-text, acentos/dead keys via xkbcommon compose, auto-save após pausa,
//! Markdown renderizado ao vivo, várias abas.

mod canvas;
mod img;
mod md;
mod store;
mod undo;

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use cosmic_text::{
    PhysicalGlyph, Renderer, render_decoration,
    Action, Attrs, AttrsList, Buffer, Change, Cursor, Edit, Editor, Ellipsize, Family, FontSystem, Metrics,
    Motion, Selection, Shaping, Style, SwashCache, UnderlineStyle, Weight, Wrap,
};
use smithay_client_toolkit::{
    activation::{ActivationHandler, ActivationState, RequestData},
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
    },
    delegate_registry,
    output::{OutputHandler, OutputState},
    primary_selection::{
        PrimarySelectionManagerState,
        device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler},
        selection::{PrimarySelectionSource, PrimarySelectionSourceHandler},
    },
    reexports::calloop::{
        EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken, channel,
        generic::Generic,
        timer::{TimeoutAction, Timer},
    },
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource,
        wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface,
    },
};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use xkbcommon::xkb;

use canvas::{Canvas, Color, Rect, rgb, rgba};
use img::ImageCache;
use md::Block;
use store::{NoteMeta, State, Store};
use undo::Undo;

static START: OnceLock<Instant> = OnceLock::new();

const APP_ID: &str = "io.github.tevoetals.fastnotes";
const AUTOSAVE_MS: u64 = 1500;
const BLINK_MS: u64 = 500;
const BLINK_FOR_SECS: u64 = 5;
const ZOOM_DEFAULT: i32 = 16;
const ZOOM_MIN: i32 = 10;
const ZOOM_MAX: i32 = 40;
const MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
// =====================================================================
// Sistema de tamanhos (ver README, "Sistema de espaçamento e tipografia").
// Grade de 4 px; espaços em múltiplos de 8; escala tipográfica 1.25 (terça
// maior); corpo com entrelinha 1.5 e títulos 1.25; medida máxima ~38 em
// (≈ 70 caracteres); alvos de clique ≥ 24 px (WCAG 2.5.8), 32 px no cabeçalho.
// =====================================================================
const SP1: f32 = 4.0;
const SP2: f32 = 8.0;
const SP3: f32 = 12.0;
const SP4: f32 = 16.0;
const SP6: f32 = 24.0;
const SP8: f32 = 32.0;
/// Escala tipográfica: 1.25^k arredondado (H1 2.0, H2 1.5625, H3 1.25).
const HEADING_SCALE: [f32; 6] = [2.0, 1.5625, 1.25, 1.0, 1.0, 1.0];
const BODY_LEADING: f32 = 1.5;
/// Quebra curta (Shift+Enter, `\` no fim da linha): a linha perde 0.25 em de
/// altura (entrelinha 1.5 → 1.25). Como o cosmic-text centra o texto na caixa
/// da linha, os glifos dessa linha são desenhados 0.125 em mais para baixo,
/// para que só a distância até a linha seguinte diminua.
const TIGHT_CUT: f32 = 0.45;
/// Célula de tabela: 12×6 px em 16 px (0.75 em × 0.375 em).
const CELL_PAD_X: f32 = 0.75;
const CELL_PAD_Y: f32 = 0.375;
const CELL_MIN_W: f32 = 3.0;
const HEADING_LEADING: f32 = 1.25;
const UI_LEADING: f32 = 1.35;
/// Medida (largura da coluna de texto) em em: ~70 caracteres na Inter.
const MEASURE_EM: f32 = 38.0;
const HEADER_H: f32 = 48.0;
const FOOTER_H: f32 = 32.0;
const BUTTON: f32 = 32.0;
const ICON_HALF: f32 = 8.0;
const STROKE: f32 = 2.0;
const TAB_H: f32 = 32.0;
const TAB_MIN_W: f32 = 48.0;
const TAB_MAX_W: f32 = 240.0;
const TAB_PAD: f32 = 12.0;
const TAB_CLOSE: f32 = 20.0;
const RADIUS_SM: f32 = 6.0;
const RADIUS: f32 = 8.0;
const RADIUS_LG: f32 = 12.0;
const TEXT_MARGIN_X: f32 = SP8;
const TEXT_MARGIN_TOP: f32 = 24.0;
const PANEL_W: f32 = 384.0;
const SEARCH_H: f32 = 40.0;
const ROW_H: f32 = 48.0;
const MENU_W: f32 = 256.0;
const MENU_ROW_H: f32 = 32.0;
const WHEEL_R: f32 = 64.0;
const UI_FONT: f32 = 13.0;
const UI_FONT_SM: f32 = 12.0;
const UI_FONT_XS: f32 = 11.0;

fn body_lh(font_px: f32) -> f32 {
    (font_px * BODY_LEADING).round()
}

fn heading_lh(font_px: f32) -> f32 {
    (font_px * HEADING_LEADING).round()
}

/// Deslocamento (px) para baixo dos glifos de cada linha com quebra curta;
/// a altura da linha é a normal menos o dobro disso.
fn compute_shifts(lines: &[md::LineInfo], font_px: f32) -> Vec<i32> {
    let cut = (font_px * TIGHT_CUT).round();
    lines
        .iter()
        .map(|l| {
            if !l.hard_break
                || matches!(l.block, Block::Table | Block::TableSep | Block::Space | Block::Image | Block::ColStart | Block::ColSep | Block::ColEnd | Block::Fence | Block::Code)
            {
                return 0;
            }
            let (fs, lh) = match l.block {
                Block::Heading(n) => {
                    let fs = (font_px * HEADING_SCALE[(n as usize - 1).min(5)]).round();
                    (fs, heading_lh(fs))
                }
                _ => (font_px, body_lh(font_px)),
            };
            let c = cut.min(lh - fs.round()).max(0.0);
            (c / 2.0).round() as i32
        })
        .collect()
}

/// Inter ativa? (senão, Noto Sans e sem tracking)
static INTER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tracking recomendado pelo autor da Inter (métricas dinâmicas):
/// `a + b·e^(c·tamanho)`, em em (unidade do `letter_spacing` do cosmic-text).
/// Zero com outras fontes.
fn tracking(size_px: f32) -> f32 {
    if !INTER.load(std::sync::atomic::Ordering::Relaxed) {
        return 0.0;
    }
    let em = -0.0223 + 0.185 * (-0.1745 * size_px).exp();
    (em * 10000.0).round() / 10000.0
}

/// Menu `/`: (rótulo, chave, palavras para busca).
const SLASH_ITEMS: [(&str, &str, &str); 16] = [
    ("Título 1", "h1", "titulo heading h1"),
    ("Título 2", "h2", "titulo heading h2"),
    ("Título 3", "h3", "titulo heading h3"),
    ("Texto normal", "p", "texto paragrafo normal"),
    ("Lista", "ul", "lista pontos bullet"),
    ("Lista numerada", "ol", "lista numerada numeros"),
    ("Checkbox", "todo", "checkbox tarefa todo caixa"),
    ("Toggle", "toggle", "toggle dobra esconder recolher"),
    ("Citação", "quote", "citacao quote"),
    ("Divisor", "hr", "divisor linha separador"),
    ("Tabela", "table", "tabela table"),
    ("2 colunas", "col2", "colunas 2 duas"),
    ("3 colunas", "col3", "colunas 3 tres"),
    ("4 colunas", "col4", "colunas 4 quatro"),
    ("Imagem", "image", "imagem foto figura image"),
    ("Bloco de código", "code", "codigo code bloco"),
];

struct Slash {
    line: usize,
    start: usize,
    sel: usize,
}

/// Roda de cores (Ctrl+Shift+C): matiz/saturação na roda, brilho na barra.
struct Picker {
    h: f32,
    s: f32,
    v: f32,
    panel: Rect,
    center: (i32, i32),
    radius: i32,
    bar: Rect,
}

impl Picker {
    fn hex(&self) -> u32 {
        let (r, g, b) = hsv_to_rgb(self.h, self.s, self.v);
        (r as u32) << 16 | (g as u32) << 8 | b as u32
    }
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = (h / 60.0).rem_euclid(6.0);
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let f = |t: f32| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (f(r), f(g), f(b))
}

fn rgb_to_hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

const IMAGE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

#[derive(Clone, Copy)]
enum PasteKind {
    Text,
    Image,
    Uris,
}

const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

// ---------- tema: só preto e branco (branco com alpha para tons) ----------
const BLACK: Color = rgb(0, 0, 0);
const WHITE: Color = rgb(0xff, 0xff, 0xff);
const fn white(a: u8) -> Color {
    rgba(0xff, 0xff, 0xff, a)
}
const LINE: Color = white(0x1f);
const BORDER: Color = white(0x38);
const GRID: Color = white(0x3c);
/// Réguas de tabela: suave entre linhas, mais forte sob o cabeçalho.
const RULE_SOFT: Color = white(0x22);
const RULE_STRONG: Color = white(0x58);
const DIM: Color = white(0x8c);
const DIM2: Color = white(0x59);
const ICON: Color = white(0x99);
const MARKER: Color = white(0x55);
const SELECTION: Color = white(0x3a);
const ROW_SEL: Color = white(0x1f);
const HOVER: Color = white(0x14);
const TAB_ACTIVE: Color = white(0x1c);
const TRANSPARENT: Color = rgba(0, 0, 0, 0);
const ELLIPSIZE: Ellipsize = Ellipsize::End(cosmic_text::EllipsizeHeightLimit::Lines(1));

static SNAPSHOT: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();

/// `FASTNOTES_PROFILE=1`: tempo médio de cada fase (ms), impresso a cada 30 quadros.
struct Prof {
    t: [f64; PROF_NAMES.len()],
    frames: u32,
}
const PROF_NAMES: [&str; 12] = ["cabeçalho", "shaping", "glifos", "decos+colunas", "rodapé", "painéis", "quadro", "snapshot", "entrada", "restyle", "derivados", "settle"];
const P_HEADER: usize = 0;
const P_SHAPE: usize = 1;
const P_GLYPHS: usize = 2;
const P_DECOS: usize = 3;
const P_FOOTER: usize = 4;
const P_PANELS: usize = 5;
const P_FRAME: usize = 6;
const P_SNAPSHOT: usize = 7;
const P_INPUT: usize = 8;
const P_RESTYLE: usize = 9;
const P_DERIVED: usize = 10;
const P_SETTLE: usize = 11;

impl Prof {
    fn new() -> Option<Prof> {
        std::env::var_os("FASTNOTES_PROFILE").map(|_| Prof { t: [0.0; PROF_NAMES.len()], frames: 0 })
    }
    fn add(&mut self, phase: usize, since: Instant) {
        self.t[phase] += since.elapsed().as_secs_f64() * 1000.0;
    }
    fn frame_done(&mut self) {
        self.frames += 1;
        if self.frames % 30 == 0 {
            let n = self.frames as f64;
            let parts: Vec<String> = PROF_NAMES.iter().zip(self.t.iter()).map(|(k, v)| format!("{k} {:.2}", v / n)).collect();
            eprintln!("[perfil] média por quadro (ms) após {} quadros: {}", self.frames, parts.join(" | "));
        }
    }
}

/// Renderizador de texto: cada glifo é um blit da máscara já rasterizada
/// (cache do swash), em vez de um callback por pixel.
const LABEL_CACHE: usize = 192;
/// Meio espaço (`<br>`): metade de uma linha em branco.
const SPACE_LEADING: f32 = 0.75;
const GLYPH_COLS: usize = 10;
const GLYPH_ROWS: usize = 6;
const GLYPH_CELL: f32 = 40.0;

/// Item dos seletores de emoji e de símbolos.
struct GlyphEntry {
    text: &'static str,
    group: u8,
    name: &'static str,
    /// Nome + palavras-chave (pt/en), em minúsculas e sem acentos, para busca.
    keys: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GlyphKind {
    Emoji,
    Symbol,
}

static EMOJI_TSV: &str = include_str!("emoji.tsv");
static SYMBOLS_TSV: &str = include_str!("symbols.tsv");
static EMOJI: OnceLock<Vec<GlyphEntry>> = OnceLock::new();
static SYMBOLS: OnceLock<Vec<GlyphEntry>> = OnceLock::new();
const EMOJI_GROUPS: [&str; 10] = ["Sorrisos e emoção", "Pessoas e corpo", "Componente", "Animais e natureza", "Comida e bebida", "Viagem e lugares", "Atividades", "Objetos", "Símbolos", "Bandeiras"];
const SYMBOL_GROUPS: [&str; 8] = ["Setas", "Marcas", "Formas", "Matemática", "Moedas", "Pontuação", "Caixas", "Diversos"];

fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c.to_lowercase().next().unwrap_or(c) {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

fn parse_glyphs(tsv: &'static str, groups: &[&str]) -> Vec<GlyphEntry> {
    tsv.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let text = it.next()?;
            let group: u8 = it.next()?.parse().ok()?;
            let name = it.next()?;
            let kw = it.next().unwrap_or("");
            let gname = groups.get(group as usize).copied().unwrap_or("");
            Some(GlyphEntry { text, group, name, keys: fold(&format!("{name} | {kw} | {gname}")) })
        })
        .collect()
}

fn glyphs(kind: GlyphKind) -> &'static [GlyphEntry] {
    match kind {
        GlyphKind::Emoji => EMOJI.get_or_init(|| parse_glyphs(EMOJI_TSV, &EMOJI_GROUPS)),
        GlyphKind::Symbol => SYMBOLS.get_or_init(|| parse_glyphs(SYMBOLS_TSV, &SYMBOL_GROUPS)),
    }
}

/// Seletor de emoji (Ctrl+.) ou símbolos (Ctrl+,): grade com busca.
struct Glyphs {
    kind: GlyphKind,
    query: String,
    sel: usize,
    top: usize,
    filtered: Vec<usize>,
    panel: Rect,
    cells: Vec<(Rect, usize)>,
}

impl Glyphs {
    fn new(kind: GlyphKind) -> Glyphs {
        let mut g = Glyphs { kind, query: String::new(), sel: 0, top: 0, filtered: Vec::new(), panel: Rect::default(), cells: Vec::new() };
        g.refilter();
        g
    }

    fn refilter(&mut self) {
        let q = fold(self.query.trim());
        let all = glyphs(self.kind);
        let words: Vec<&str> = q.split_whitespace().collect();
        self.filtered = all
            .iter()
            .enumerate()
            .filter(|(_, e)| words.iter().all(|w| e.keys.contains(w)))
            .map(|(i, _)| i)
            .collect();
        self.sel = 0;
        self.top = 0;
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let next = self.sel as i32 + delta;
        self.sel = next.clamp(0, n as i32 - 1) as usize;
        let row = self.sel / GLYPH_COLS;
        if row < self.top {
            self.top = row;
        } else if row >= self.top + GLYPH_ROWS {
            self.top = row + 1 - GLYPH_ROWS;
        }
    }

    fn scroll(&mut self, rows: i32) {
        let total_rows = self.filtered.len().div_ceil(GLYPH_COLS);
        let max_top = total_rows.saturating_sub(GLYPH_ROWS);
        self.top = (self.top as i32 + rows).clamp(0, max_top as i32) as usize;
    }
}

#[derive(PartialEq, Eq)]
struct LabelKey {
    text: String,
    size: u32,
    weight: u16,
    max_w: Option<u32>,
    scale: i32,
}

struct FastRenderer<'a, 'c> {
    canvas: &'a mut Canvas<'c>,
    fs: &'a mut FontSystem,
    cache: &'a mut SwashCache,
    ox: i32,
    oy: i32,
}

impl Renderer for FastRenderer<'_, '_> {
    fn rectangle(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color) {
        self.canvas.rect(self.ox + x, self.oy + y, w as i32, h as i32, color);
    }

    fn glyph(&mut self, g: PhysicalGlyph, color: Color) {
        let Some(img) = self.cache.get_image(self.fs, g.cache_key) else { return };
        let x = self.ox + g.x + img.placement.left;
        let y = self.oy + g.y - img.placement.top;
        let (w, h) = (img.placement.width, img.placement.height);
        match img.content {
            cosmic_text::SwashContent::Mask => self.canvas.blit_mask(x, y, w, h, &img.data, color),
            cosmic_text::SwashContent::Color => self.canvas.blit_rgba(x, y, w, h, &img.data),
            cosmic_text::SwashContent::SubpixelMask => {}
        }
    }
}

/// Desenha um `Buffer` já moldado (`shape_until_scroll`) com o renderizador rápido.
fn render_buffer(b: &Buffer, r: &mut FastRenderer<'_, '_>, color: Color) {
    render_buffer_shifted(b, r, color, |_| 0);
}

/// Idem, com deslocamento vertical por linha (quebras curtas).
fn render_buffer_shifted(b: &Buffer, r: &mut FastRenderer<'_, '_>, color: Color, shift: impl Fn(usize) -> i32) {
    for run in b.layout_runs() {
        let dy = shift(run.line_i);
        r.oy += dy;
        for glyph in run.glyphs {
            let pg = glyph.physical((0.0, run.line_y), 1.0);
            r.glyph(pg, glyph.color_opt.unwrap_or(color));
        }
        render_decoration(r, &run, color);
        r.oy -= dy;
    }
}

/// Desenha o editor (seleção, decorações e glifos) com deslocamento por linha.
/// Réplica de `Editor::render` do cosmic-text, sem cursor (desenhado à parte).
fn render_editor(ed: &Editor<'static>, shift: &[i32], r: &mut FastRenderer<'_, '_>, text_color: Color, selection_color: Color) {
    let sel = ed.selection_bounds();
    ed.with_buffer(|buffer| {
        let buf_w = buffer.size().0.unwrap_or(0.0) as i32;
        for run in buffer.layout_runs() {
            let dy = shift.get(run.line_i).copied().unwrap_or(0);
            r.oy += dy;
            let line_i = run.line_i;
            let line_top = run.line_top as i32;
            let line_h = run.line_height as u32;
            if let Some((start, end)) = sel {
                if line_i >= start.line && line_i <= end.line {
                    let hl: Vec<(f32, f32)> = run.highlight(start, end).collect();
                    if hl.is_empty() && run.glyphs.is_empty() && end.line > line_i {
                        r.rectangle(0, line_top, buf_w.max(0) as u32, line_h, selection_color);
                    } else {
                        let len = hl.len();
                        for (idx, (x, w)) in hl.into_iter().enumerate() {
                            let mut min = x as i32;
                            let mut max = (x + w) as i32;
                            if idx + 1 == len && end.line > line_i {
                                if run.rtl {
                                    min = 0;
                                } else {
                                    max = buf_w;
                                }
                            }
                            r.rectangle(min, line_top, (max - min).max(0) as u32, line_h, selection_color);
                        }
                    }
                }
            }
            render_decoration(r, &run, text_color);
            for glyph in run.glyphs {
                let pg = glyph.physical((0.0, run.line_y), 1.0);
                r.glyph(pg, glyph.color_opt.unwrap_or(text_color));
            }
            r.oy -= dy;
        }
    });
}

fn prof_add(prof: &mut Option<Prof>, phase: usize, since: Instant) {
    if let Some(p) = prof {
        p.add(phase, since);
    }
}

fn trace(label: &str) {
    if std::env::var_os("FASTNOTES_TRACE").is_some() {
        let ms = START.get().map(|t| t.elapsed().as_micros() as f64 / 1000.0).unwrap_or(0.0);
        eprintln!("[{ms:>7.1} ms] {label}");
    }
}

// =====================================================================
// Instância única: socket Unix em $XDG_RUNTIME_DIR.
// =====================================================================

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("fastnotes-{}.sock", unsafe { libc::getuid() }))
}

/// Se já existe uma instância, pede para ela se ativar e devolve `true`.
fn activate_existing() -> bool {
    let Ok(mut s) = UnixStream::connect(socket_path()) else { return false };
    // Lançadores (KRunner, atalhos) passam um token no ambiente; num terminal
    // não há token, então pedimos um ao compositor (custa ~10 ms).
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(fetch_activation_token)
        .unwrap_or_default();
    let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
    writeln!(s, "activate {token}").is_ok()
}

/// Cliente Wayland mínimo só para obter um token xdg-activation.
struct TokenFetcher {
    registry_state: RegistryState,
    token: Option<String>,
}

impl ActivationHandler for TokenFetcher {
    type RequestUdata = ();
    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        self.token = Some(token);
    }
}

delegate_registry!(TokenFetcher);
impl ProvidesRegistryState for TokenFetcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![];
}
smithay_client_toolkit::delegate_dispatch2!(TokenFetcher);

fn fetch_activation_token() -> Option<String> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<TokenFetcher>(&conn).ok()?;
    let qh = queue.handle();
    let activation = ActivationState::bind(&globals, &qh).ok()?;
    let mut st = TokenFetcher { registry_state: RegistryState::new(&globals), token: None };
    activation.request_token(
        &qh,
        RequestData { seat_and_serial: None, surface: None, app_id: Some(APP_ID.to_string()), udata: () },
    );
    for _ in 0..20 {
        queue.blocking_dispatch(&mut st).ok()?;
        if st.token.is_some() {
            break;
        }
    }
    st.token
}

fn bind_socket() -> Option<UnixListener> {
    let p = socket_path();
    let _ = std::fs::remove_file(&p);
    let l = UnixListener::bind(&p).ok()?;
    l.set_nonblocking(true).ok()?;
    Some(l)
}

// =====================================================================
// Fontes: um conjunto curado carrega em ~2 ms; o banco completo do sistema
// carrega em segundo plano e substitui o curado (para qualquer script).
// =====================================================================

fn locale() -> String {
    std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(|s| s.replace('_', "-")))
        .filter(|s| !s.is_empty() && s != "C" && s != "POSIX")
        .unwrap_or_else(|| "en-US".to_string())
}

fn file_mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// Observa a pasta de notas (inotify): nomes de arquivo `.md` gravados,
/// renomeados ou apagados ali chegam em `App::on_disk_change`. Sem custo em
/// repouso: o kernel acorda o laço de eventos só quando algo muda.
fn watch_notes(dir: &Path) -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let cdir = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: chamadas simples de libc; o fd passa a pertencer ao File.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_DELETE;
        if libc::inotify_add_watch(fd, cdir.as_ptr(), mask) < 0 {
            libc::close(fd);
            return None;
        }
        Some(std::fs::File::from_raw_fd(fd))
    }
}

/// Lê os eventos pendentes do inotify e devolve os nomes `.md` afetados.
fn read_inotify(f: &mut std::fs::File) -> Vec<String> {
    let mut names = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        let mut i = 0;
        // struct inotify_event { int wd; uint32 mask, cookie, len; char name[len]; }
        while i + 16 <= n {
            let len = u32::from_ne_bytes(buf[i + 12..i + 16].try_into().unwrap()) as usize;
            let raw = &buf[i + 16..(i + 16 + len).min(n)];
            let name = String::from_utf8_lossy(raw.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
            if name.ends_with(".md") && !names.contains(&name) {
                names.push(name);
            }
            i += 16 + len;
        }
    }
    names
}

/// Diretório com `InterVariable.ttf`: instalado pelo install.sh, pacote do
/// sistema ou a pasta `fonts/` do repositório (execução via cargo).
fn inter_dir() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(d) = data {
        cands.push(d.join("fonts/fastnotes"));
    }
    cands.push(PathBuf::from("/app/share/fonts/fastnotes")); // Flatpak
    cands.push(PathBuf::from("/usr/share/fonts/inter"));
    cands.push(PathBuf::from("/usr/share/fonts/TTF"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo) = exe.ancestors().nth(3) {
            cands.push(repo.join("fonts"));
        }
    }
    cands.into_iter().find(|d| d.join("InterVariable.ttf").is_file())
}

fn load_inter(db: &mut fontdb::Database) -> bool {
    let Some(dir) = inter_dir() else { return false };
    let ok = db.load_font_file(dir.join("InterVariable.ttf")).is_ok();
    let _ = db.load_font_file(dir.join("InterVariable-Italic.ttf"));
    ok
}

fn curated_db() -> Option<fontdb::Database> {
    // No Flatpak as fontes do sistema ficam em /run/host/fonts.
    let dir = ["/usr/share/fonts/noto", "/run/host/fonts/noto"]
        .into_iter()
        .map(Path::new)
        .find(|d| d.join("NotoSans-Regular.ttf").is_file())?;
    let files = [
        "NotoSans-Regular.ttf",
        "NotoSans-Bold.ttf",
        "NotoSans-Italic.ttf",
        "NotoSans-BoldItalic.ttf",
        "NotoSansMono-Regular.ttf",
        "NotoSansMono-Bold.ttf",
        "NotoColorEmoji.ttf",
        "NotoSansSymbols-Regular.ttf",
        "NotoSansSymbols2-Regular.ttf",
        "NotoSansMath-Regular.ttf",
    ];
    let mut db = fontdb::Database::new();
    load_inter(&mut db);
    let mut loaded = 0;
    for f in files {
        if db.load_font_file(dir.join(f)).is_ok() {
            loaded += 1;
        }
    }
    (loaded >= 2).then_some(db)
}

/// Controles e caracteres de formato (ZWJ, seletores de variação…) não têm glifo.
fn is_format_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c as u32,
            0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFE00..=0xFE0F | 0xFEFF | 0xE0020..=0xE007F
        )
}

fn full_db() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    load_inter(&mut db);
    db
}

fn make_font_system(mut db: fontdb::Database) -> FontSystem {
    let inter = db
        .faces()
        .find_map(|f| f.families.iter().find(|(n, _)| n.starts_with("Inter")).map(|(n, _)| n.clone()));
    INTER.store(inter.is_some(), std::sync::atomic::Ordering::Relaxed);
    match inter {
        Some(name) => db.set_sans_serif_family(name),
        None => db.set_sans_serif_family("Noto Sans"),
    }
    db.set_monospace_family("Noto Sans Mono");
    db.set_serif_family("Noto Serif");
    FontSystem::new_with_locale_and_db(locale(), db)
}

// =====================================================================
// Aba: um editor + estado de uma nota.
// =====================================================================

struct Tab {
    editor: Editor<'static>,
    undo: Undo,
    path: Option<PathBuf>,
    saved_text: String,
    /// mtime do arquivo na última leitura/gravação feita pelo app: outra
    /// mtime no disco = alguém de fora (o bot do Telegram) mudou a nota.
    disk_mtime: Option<std::time::SystemTime>,
    /// Houve conflito com uma gravação de fora: o próximo "salvo" avisa.
    conflict: bool,
    status: String,
    title: String,
    words: usize,
    chars: usize,
    editor_size: (f32, f32),
    /// Largura disponível para tabelas: da coluna de texto até a margem direita.
    wide_w: f32,
    metrics_key: (i32, i32),
    lines: Vec<md::LineInfo>,
    last_line: usize,
    /// Prévia de cor da roda: (início, fim, cor).
    preview: Option<(Cursor, Cursor, u32)>,
    /// Blocos de colunas (`:::` … `:::`), com um sub-buffer por coluna.
    columns: Vec<ColBlock>,
    /// Tabelas renderizadas como células proporcionais (um sub-buffer por célula).
    tables: Vec<TableBlock>,
    /// Deslocamento vertical (px) dos glifos de cada linha (quebra curta).
    line_shift: Vec<i32>,
    /// Imagens em linha do buffer principal, já com tamanho na tela.
    inline_imgs: Vec<InlineImg>,
}

/// Imagem em linha pronta para desenhar: posição do glifo reservado e tamanho.
struct InlineImg {
    line: usize,
    idx: usize,
    start: usize,
    end: usize,
    path: String,
    w: u32,
    h: u32,
}

/// Imagem desenhada na tela (para redimensionar pelos cantos).
struct ImgHit {
    rect: Rect,
    line: usize,
    start: usize,
    end: usize,
}

/// Arrasto de canto de imagem em andamento.
struct ImgDrag {
    line: usize,
    start: usize,
    end: usize,
    w0: i32,
    x0: i32,
    /// Canto direito (a largura cresce com o mouse indo para a direita).
    right: bool,
}

const IMG_MIN_W: u32 = 24;
const IMG_CORNER: f32 = 10.0;
const IMG_PAD: f32 = 0.25;

/// Tabela desenhada no espaço das suas linhas `|`: colunas com largura pelo
/// conteúdo, números à direita, réguas horizontais suaves.
struct TableBlock {
    start: usize,
    end: usize,
    w: f32,
    col_x: Vec<f32>,
    col_w: Vec<f32>,
    /// 0 esquerda, 1 centro, 2 direita.
    align: Vec<u8>,
    row_h: f32,
    font: f32,
    hpad: f32,
    vpad: f32,
    rows: Vec<TableRow>,
}

struct TableRow {
    line: usize,
    cells: Vec<TableCell>,
}

struct TableCell {
    /// Intervalo (bytes) do conteúdo da célula na linha crua, sem espaços das pontas.
    start: usize,
    end: usize,
    w: f32,
    buffer: Buffer,
}

/// Célula clicável: retângulo na tela e origem do texto.
struct CellHit {
    rect: Rect,
    line: usize,
    bi: usize,
    ri: usize,
    ci: usize,
    tx: i32,
}

/// Índice da célula que contém a posição `idx` da linha crua.
fn cell_index(pipes: &[usize], idx: usize, ncells: usize) -> usize {
    pipes.iter().filter(|&&p| p < idx).count().saturating_sub(1).min(ncells.saturating_sub(1))
}

/// Largura mínima de uma coluna, em px, ao arrastar o divisor.
const COL_MIN_W: f32 = 48.0;
/// Meia largura da área de arraste do divisor de colunas.
const COL_GRIP: i32 = 5;

/// Larguras em px a partir das porcentagens de `::: 30 70`, relativas à
/// largura de leitura `base` (faltando ou inválidas → iguais, somando 100 %).
/// A soma pode passar de 100 %: o bloco se estende até `max` (margem direita).
fn col_widths(pct: Vec<u16>, n: usize, base: f32, max: f32) -> Vec<f32> {
    let pct: Vec<f32> = if pct.len() == n { pct.into_iter().map(f32::from).collect() } else { vec![100.0 / n as f32; n] };
    let sum: f32 = pct.iter().sum::<f32>().max(1.0);
    let avail = (base * sum / 100.0).min(max.max(base));
    let min = COL_MIN_W.min(avail / n as f32);
    let mut w: Vec<f32> = pct.iter().map(|p| (avail * p / sum).max(min)).collect();
    // Se o mínimo empurrou a soma para cima, tira o excesso das mais largas.
    let excess = w.iter().sum::<f32>() - avail;
    if excess > 0.5 {
        let room: f32 = w.iter().map(|x| x - min).sum::<f32>().max(1.0);
        for x in &mut w {
            *x -= excess * (*x - min) / room;
        }
    }
    w
}

/// Arraste do divisor entre as colunas `ci - 1` e `ci` do bloco `bi`
/// (`ci == n`: borda direita, que alarga o bloco inteiro).
struct ColDrag {
    bi: usize,
    ci: usize,
    /// Linha `:::` de abertura, onde ficam as porcentagens.
    line: usize,
    x0: i32,
    /// Larguras (px) no início do arraste; 100 % e máximo do bloco.
    w0: Vec<f32>,
    base: f32,
    max: f32,
}

/// Bloco de colunas renderizado no espaço da linha `:::` inicial.
struct ColBlock {
    start: usize,
    /// Última linha do bloco (o `:::` de fecho, ou a última linha se não houver).
    end: usize,
    height: f32,
    /// Espaço entre colunas; largura das colunas somadas a 100 % (medida de
    /// leitura) e o máximo até a margem direita (px).
    gap: f32,
    base: f32,
    max: f32,
    cols: Vec<ColText>,
}

struct ColText {
    first: usize,
    last: usize,
    /// Linha do sub-buffer → linha real (linhas dobradas ficam de fora).
    map: Vec<usize>,
    x: f32,
    w: f32,
    buffer: Buffer,
}

impl ColText {
    fn sub_line(&self, main: usize) -> Option<usize> {
        self.map.iter().position(|&m| m == main)
    }
}

impl Tab {
    /// Coluna que contém a linha `line`, se houver: (bloco, coluna).
    fn column_of(&self, line: usize) -> Option<(usize, usize)> {
        for (bi, b) in self.columns.iter().enumerate() {
            for (ci, c) in b.cols.iter().enumerate() {
                if line >= c.first && line <= c.last {
                    return Some((bi, ci));
                }
            }
        }
        None
    }

    /// Bloco de colunas que contém a linha (de conteúdo ou estrutural).
    fn col_block_of(&self, line: usize) -> Option<usize> {
        self.columns.iter().position(|b| line >= b.start && line <= b.end)
    }

    /// Linha estrutural (`:::` de abertura ou o `|||` que antecede a coluna `ci`).
    fn col_sep_line(&self, bi: usize, ci: usize) -> Option<usize> {
        let b = self.columns.get(bi)?;
        if ci == 0 {
            return Some(b.start);
        }
        (b.start + 1..=b.end).filter(|&k| self.lines[k].block == Block::ColSep).nth(ci - 1)
    }

    /// Bloco de colunas só com linhas vazias (uma por coluna, no máximo).
    fn col_block_empty(&self, bi: usize) -> bool {
        let Some(b) = self.columns.get(bi) else { return false };
        (b.start + 1..b.end).all(|k| matches!(self.lines[k].block, Block::ColSep) || self.line_text(k).trim().is_empty())
            && b.cols.iter().all(|c| c.map.len() <= 1)
    }

    fn new(font_system: &mut FontSystem, font_px: f32) -> Tab {
        let mut b = Buffer::new(font_system, Metrics::new(font_px, body_lh(font_px)));
        b.set_wrap(Wrap::WordOrGlyph);
        b.set_tab_width(4);
        Tab {
            editor: Editor::new(b),
            undo: Undo::default(),
            path: None,
            saved_text: String::new(),
            disk_mtime: None,
            conflict: false,
            status: "nova nota".to_string(),
            title: "Nova nota".to_string(),
            words: 0,
            chars: 0,
            editor_size: (0.0, 0.0),
            wide_w: 0.0,
            metrics_key: (0, 0),
            lines: Vec::new(),
            last_line: 0,
            preview: None,
            columns: Vec::new(),
            tables: Vec::new(),
            line_shift: Vec::new(),
            inline_imgs: Vec::new(),
        }
    }

    fn text(&self) -> String {
        self.editor.with_buffer(|b| {
            let mut s = String::new();
            for (i, l) in b.lines.iter().enumerate() {
                if i > 0 {
                    s.push('\n');
                }
                s.push_str(l.text());
            }
            s
        })
    }

    fn line_texts(&self) -> Vec<String> {
        self.editor.with_buffer(|b| b.lines.iter().map(|l| l.text().to_string()).collect())
    }

    fn line_text(&self, i: usize) -> String {
        self.editor.with_buffer(|b| b.lines.get(i).map(|l| l.text().to_string()).unwrap_or_default())
    }

    fn end_cursor(&self) -> Cursor {
        self.editor.with_buffer(|b| {
            let last = b.lines.len().saturating_sub(1);
            Cursor::new(last, b.lines.get(last).map(|l| l.text().len()).unwrap_or(0))
        })
    }

    fn update_derived(&mut self, text: &str) {
        let t = store::title_of(text);
        self.title = if t.is_empty() { "Nova nota".to_string() } else { t };
        self.words = text.split_whitespace().count();
        self.chars = text.chars().count();
    }

    fn on_text_changed(&mut self) {
        let text = self.text();
        self.update_derived(&text);
        self.status = "editando…".to_string();
    }

    fn is_empty_new(&self) -> bool {
        self.path.is_none() && self.text().trim().is_empty()
    }

    fn load(&mut self, text: &str, fs: &mut FontSystem, images: &mut ImageCache, font_px: f32) {
        let text = &md::format_tables_in_text(text);
        let attrs = Attrs::new().family(Family::SansSerif).color(WHITE);
        self.editor.with_buffer_mut(|b| b.set_text(text, &attrs, Shaping::Advanced, None));
        let end = self.end_cursor();
        self.editor.set_selection(Selection::None);
        self.editor.set_cursor(end);
        self.editor.set_redraw(true);
        self.undo.clear();
        self.saved_text = text.to_string();
        self.last_line = end.line;
        self.update_derived(text);
        self.restyle(fs, images, font_px);
    }

    /// Reaplica os estilos Markdown em todas as linhas.
    fn restyle(&mut self, fs: &mut FontSystem, images: &mut ImageCache, font_px: f32) {
        let texts = self.line_texts();
        self.lines = md::analyze(texts.iter().map(String::as_str));
        self.line_shift = compute_shifts(&self.lines, font_px);
        let cursor_line = self.editor.cursor().line;
        self.build_columns(fs, font_px, cursor_line);
        let cursor = self.editor.cursor();
        self.build_tables(fs, font_px, cursor);
        let lines = &self.lines;
        let text_w = self.editor_size.0;
        let wide_w = self.wide_w.max(text_w);
        let mut inline_imgs: Vec<InlineImg> = Vec::new();
        self.editor.with_buffer_mut(|b| {
            for (i, info) in lines.iter().enumerate() {
                let Some(bl) = b.lines.get_mut(i) else { break };
                let mut base = Attrs::new().family(Family::SansSerif).color(WHITE).letter_spacing(tracking(font_px));
                let mut line_h = body_lh(font_px);
                match info.block {
                    Block::Heading(l) => {
                        let k = HEADING_SCALE[(l as usize - 1).min(5)];
                        let fs = (font_px * k).round();
                        line_h = heading_lh(fs);
                        base = base.metrics(Metrics::new(fs, line_h)).weight(Weight::BOLD).letter_spacing(tracking(fs));
                    }
                    Block::Space => {
                        // Meio espaço: metade de uma linha em branco; o `<br>`
                        // aparece pequeno e apagado só na linha do cursor.
                        line_h = (font_px * SPACE_LEADING).round();
                        base = base.metrics(Metrics::new((font_px * 0.6).round().max(1.0), line_h));
                    }
                    _ => {}
                }
                let shift = self.line_shift.get(i).copied().unwrap_or(0);
                if shift > 0 {
                    let fsize = match info.block {
                        Block::Heading(l) => (font_px * HEADING_SCALE[(l as usize - 1).min(5)]).round(),
                        _ => font_px,
                    };
                    line_h -= 2.0 * shift as f32;
                    base = base.metrics(Metrics::new(fsize, line_h));
                }
                if info.folded || info.col.is_some() || matches!(info.block, Block::ColSep | Block::ColEnd | Block::TableSep) {
                    let tiny = Attrs::new().metrics(Metrics::new(1.0, 1.0)).color(TRANSPARENT);
                    bl.set_attrs_list(AttrsList::new(&tiny));
                    continue;
                }
                if info.block == Block::Table {
                    // A linha crua fica invisível; as células são desenhadas por cima.
                    let h = self.tables.iter().find(|t| i >= t.start && i <= t.end).map(|t| t.row_h).unwrap_or(line_h);
                    let tiny = Attrs::new().metrics(Metrics::new(1.0, h)).color(TRANSPARENT);
                    bl.set_attrs_list(AttrsList::new(&tiny));
                    continue;
                }
                if info.block == Block::ColStart {
                    // A linha `:::` ocupa a altura do bloco de colunas (menos as linhas de 1 px).
                    let h = self.columns.iter().find(|b| b.start == i).map(|b| b.height).unwrap_or(1.0);
                    let inner = lines[i + 1..].iter().take_while(|l| l.col.is_some() || matches!(l.block, Block::ColSep)).count() as f32 + 1.0;
                    let tiny = Attrs::new().metrics(Metrics::new(1.0, (h - inner).max(1.0))).color(TRANSPARENT);
                    bl.set_attrs_list(AttrsList::new(&tiny));
                    continue;
                }
                let mut list = AttrsList::new(&base);
                let preview_spans;
                let spans: &[(std::ops::Range<usize>, md::Flags)] = match self.preview {
                    Some((a, b, c)) if i >= a.line && i <= b.line => {
                        let len = texts[i].len();
                        let start = if i == a.line { a.index } else { 0 };
                        let end = if i == b.line { b.index } else { len };
                        preview_spans = md::recolor(&info.spans, len, start..end, c);
                        &preview_spans
                    }
                    _ => &info.spans,
                };
                let img_missing = info.images.iter().any(|im| images.get(&im.path).is_none());
                for (r, f) in spans {
                    list.add_span(r.clone(), &attrs_for(base.clone(), *f, i == cursor_line || img_missing, line_h));
                }
                // Imagens em linha: o `!` vira um glifo invisível de 1 px cuja
                // altura de linha é a da imagem e cujo avanço é a largura dela.
                let max_w = if wide_w > 0.0 { wide_w as u32 } else { 800 };
                for im in &info.images {
                    let Some(bm) = images.get(&im.path) else { continue };
                    let w = im.width.unwrap_or(bm.w.min(max_w)).clamp(IMG_MIN_W, max_w.max(IMG_MIN_W));
                    let h = (bm.h as u64 * w as u64 / bm.w.max(1) as u64).max(1) as u32;
                    let pad = (font_px * IMG_PAD).round();
                    let slot = Attrs::new().metrics(Metrics::new(1.0, h as f32 + pad)).letter_spacing(w as f32).color(TRANSPARENT);
                    list.add_span(im.start..im.start + 1, &slot);
                    inline_imgs.push(InlineImg { line: i, idx: im.start, start: im.start, end: im.end, path: im.path.clone(), w, h });
                }
                bl.set_attrs_list(list);
            }
        });
        self.inline_imgs = inline_imgs;
    }

    /// Atributos base de uma linha (títulos maiores, cabeçalho de tabela em negrito).
    fn line_base(info: &md::LineInfo, font_px: f32) -> (Attrs<'static>, f32) {
        let mut base = Attrs::new().family(Family::SansSerif).color(WHITE).letter_spacing(tracking(font_px));
        let mut line_h = body_lh(font_px);
        match info.block {
            Block::Heading(l) => {
                let k = HEADING_SCALE[(l as usize - 1).min(5)];
                let fs = (font_px * k).round();
                line_h = heading_lh(fs);
                base = base.metrics(Metrics::new(fs, line_h)).weight(Weight::BOLD).letter_spacing(tracking(fs));
            }
            Block::Table if info.header => base = base.weight(Weight::BOLD),
            Block::TableSep => {
                line_h = (font_px * 0.5).round().max(4.0);
                base = base.metrics(Metrics::new(1.0, line_h));
            }
            _ => {}
        }
        (base, line_h)
    }

    /// Monta um sub-buffer por coluna de cada bloco `:::`.
    fn build_columns(&mut self, fs: &mut FontSystem, font_px: f32, cursor_line: usize) {
        self.columns.clear();
        let texts = self.line_texts();
        let text_w = if self.editor_size.0 > 0.0 { self.editor_size.0 } else { 600.0 };
        let gap = (font_px * 1.4).round();
        let pad = (font_px * 0.5).round();
        let n = self.lines.len();
        let mut i = 0;
        while i < n {
            if self.lines[i].block != Block::ColStart {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i + 1;
            while j < n && !matches!(self.lines[j].block, Block::ColEnd | Block::ColStart) {
                j += 1;
            }
            let end = if j < n && self.lines[j].block == Block::ColEnd { j } else { j.saturating_sub(1).max(start) };
            let seps = (i + 1..j).filter(|&k| self.lines[k].block == Block::ColSep).count();
            let ncols = (seps + 1).min(md::MAX_COLS as usize);
            let gaps = gap * (ncols as f32 - 1.0);
            let base = (text_w - gaps).max(40.0 * ncols as f32);
            let max = (self.wide_w - gaps).max(base);
            let widths = col_widths(md::col_fence(&texts[start]).unwrap_or_default(), ncols, base, max);
            let mut cols = Vec::new();
            let mut cx = 0.0;
            let mut max_h: f32 = 0.0;
            for c in 0..ncols {
                let members: Vec<usize> = (i + 1..j).filter(|&k| self.lines[k].col == Some((start, c as u8)) && !self.lines[k].folded).collect();
                // Coluna sem linhas: intervalo vazio, nenhuma linha "pertence" a ela.
                let (first, last) = match (members.first(), members.last()) {
                    (Some(&f), Some(&l)) => (f, l),
                    _ => (usize::MAX, usize::MAX),
                };
                let mut pieces: Vec<(String, Attrs<'static>)> = Vec::new();
                let default = Attrs::new().family(Family::SansSerif).color(WHITE);
                for (mi, &k) in members.iter().enumerate() {
                    if mi > 0 {
                        pieces.push(("\n".to_string(), default.clone()));
                    }
                    let info = &self.lines[k];
                    let (mut base, mut line_h) = Self::line_base(info, font_px);
                    let shift = self.line_shift.get(k).copied().unwrap_or(0);
                    if shift > 0 {
                        let fsize = match info.block {
                            Block::Heading(l) => (font_px * HEADING_SCALE[(l as usize - 1).min(5)]).round(),
                            _ => font_px,
                        };
                        line_h -= 2.0 * shift as f32;
                        base = base.metrics(Metrics::new(fsize, line_h));
                    }
                    let text = &texts[k];
                    let mut pos = 0;
                    for (r, f) in &info.spans {
                        if r.start > pos {
                            pieces.push((text[pos..r.start].to_string(), base.clone()));
                        }
                        pieces.push((text[r.start..r.end.min(text.len())].to_string(), attrs_for(base.clone(), *f, k == cursor_line, line_h)));
                        pos = r.end.min(text.len());
                    }
                    if pos < text.len() {
                        pieces.push((text[pos..].to_string(), base.clone()));
                    }
                    if text.is_empty() {
                        pieces.push((String::new(), base.clone()));
                    }
                }
                let colw = widths[c];
                let mut buffer = Buffer::new(fs, Metrics::new(font_px, body_lh(font_px)));
                buffer.set_wrap(Wrap::WordOrGlyph);
                buffer.set_tab_width(4);
                buffer.set_size(Some(colw), None);
                buffer.set_rich_text(pieces.iter().map(|(t, a)| (t.as_str(), a.clone())), &default, Shaping::Advanced, None);
                buffer.shape_until_scroll(fs, false);
                let h = buffer.layout_runs().last().map(|r| r.line_top + r.line_height).unwrap_or(body_lh(font_px));
                max_h = max_h.max(h);
                cols.push(ColText { first, last, map: members, x: cx, w: colw, buffer });
                cx += colw + gap;
            }
            self.columns.push(ColBlock { start, end, height: max_h + pad * 2.0, gap, base, max, cols });
            i = j + 1;
        }
    }

    /// Tabelas fora de colunas: mede cada célula na fonte do corpo (Inter,
    /// algarismos tabulares), largura da coluna = célula mais larga + padding,
    /// números à direita salvo alinhamento declarado em `:---:`; se não cabe
    /// na medida, a fonte da tabela encolhe (até 55 %).
    fn build_tables(&mut self, fs: &mut FontSystem, font_px: f32, cursor: Cursor) {
        self.tables.clear();
        let texts = self.line_texts();
        // Tabelas podem passar da medida do texto até a margem direita da janela.
        let text_w = if self.editor_size.0 > 0.0 { self.wide_w.max(self.editor_size.0) } else { 600.0 };
        let is_tbl = |l: &md::LineInfo| matches!(l.block, Block::Table | Block::TableSep) && l.col.is_none() && !l.folded;
        let n = self.lines.len();
        let mut i = 0;
        while i < n {
            if !is_tbl(&self.lines[i]) {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i;
            while j < n && is_tbl(&self.lines[j]) {
                j += 1;
            }
            let end = j - 1;
            i = j;
            // Células (intervalos crus) de cada linha e alinhamento declarado.
            let mut raw_rows: Vec<(usize, Vec<(usize, usize)>)> = Vec::new();
            let mut declared: Vec<Option<u8>> = Vec::new();
            for k in start..=end {
                let info = &self.lines[k];
                let text: &str = &texts[k];
                if info.block == Block::TableSep {
                    if declared.is_empty() {
                        declared = md::sep_align(text);
                    }
                    continue;
                }
                let p = &info.pipes;
                let mut cells = Vec::new();
                for c in 0..p.len() {
                    let cs = p[c] + 1;
                    let ce = if c + 1 < p.len() { p[c + 1] } else { text.len() };
                    if c + 1 == p.len() && text[cs.min(text.len())..].trim().is_empty() {
                        break;
                    }
                    let seg = &text[cs..ce];
                    let lead = seg.len() - seg.trim_start().len();
                    let trail = seg.len() - seg.trim_end().len();
                    let (a, b) = if lead + trail >= seg.len() { (cs + lead, cs + lead) } else { (cs + lead, ce - trail) };
                    cells.push((a, b));
                }
                raw_rows.push((k, cells));
            }
            let ncols = raw_rows.iter().map(|(_, c)| c.len()).max().unwrap_or(0);
            if ncols == 0 {
                continue;
            }
            let mut align = vec![0u8; ncols];
            for (c, a) in align.iter_mut().enumerate() {
                *a = match declared.get(c).copied().flatten() {
                    Some(v) => v,
                    None => {
                        let mut seen = false;
                        let numeric = raw_rows.iter().filter(|(k, _)| !self.lines[*k].header).all(|(k, cells)| {
                            cells.get(c).is_none_or(|&(a, b)| {
                                let t = &texts[*k][a..b];
                                if t.is_empty() {
                                    true
                                } else {
                                    seen = true;
                                    md::is_numeric_cell(t)
                                }
                            })
                        });
                        if numeric && seen { 2 } else { 0 }
                    }
                };
            }
            // Mede na fonte cheia; se não cabe, reconstrói menor.
            let mut font = font_px;
            for _attempt in 0..2 {
                let hpad = (font * CELL_PAD_X).round();
                let vpad = (font * CELL_PAD_Y).round();
                let mut rows: Vec<TableRow> = Vec::new();
                let mut col_w = vec![(font * CELL_MIN_W).round(); ncols];
                for (k, cells) in &raw_rows {
                    let info = &self.lines[*k];
                    let text: &str = &texts[*k];
                    let cursor_cell = (cursor.line == *k).then(|| cell_index(&info.pipes, cursor.index, cells.len().max(1)));
                    let mut out = Vec::new();
                    for (ci, &(a, b)) in cells.iter().enumerate() {
                        let editing = cursor_cell == Some(ci);
                        let lh = body_lh(font);
                        let mut base = Attrs::new().family(Family::SansSerif).color(WHITE).letter_spacing(tracking(font)).metrics(Metrics::new(font, lh));
                        if info.header {
                            base = base.weight(Weight::SEMIBOLD);
                        }
                        let mut list = AttrsList::new(&base);
                        for (r, f) in &info.spans {
                            let rs = r.start.max(a);
                            let re = r.end.min(b);
                            if rs < re {
                                // `mono` da linha crua não vale na célula (só `code`).
                                let mut f = *f;
                                f.mono = f.code;
                                list.add_span(rs - a..re - a, &attrs_for(base.clone(), f, editing, lh));
                            }
                        }
                        let mut buffer = Buffer::new(fs, Metrics::new(font, lh));
                        buffer.set_wrap(Wrap::None);
                        buffer.set_size(None, None);
                        buffer.set_text(&text[a..b], &base, Shaping::Advanced, None);
                        if let Some(l) = buffer.lines.get_mut(0) {
                            l.set_attrs_list(list);
                        }
                        buffer.shape_until_scroll(fs, false);
                        let w = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max).ceil();
                        col_w[ci] = col_w[ci].max(w + 2.0 * hpad);
                        out.push(TableCell { start: a, end: b, w, buffer });
                    }
                    rows.push(TableRow { line: *k, cells: out });
                }
                let total: f32 = col_w.iter().sum();
                if total > text_w && font == font_px {
                    font = (font_px * (text_w / total).clamp(0.55, 1.0) * 2.0).round() / 2.0;
                    continue;
                }
                let mut col_x = Vec::with_capacity(ncols);
                let mut x = 0.0;
                for w in &col_w {
                    col_x.push(x);
                    x += w;
                }
                self.tables.push(TableBlock {
                    start,
                    end,
                    w: total,
                    col_x,
                    col_w,
                    align: align.clone(),
                    row_h: body_lh(font) + 2.0 * vpad,
                    font,
                    hpad,
                    vpad,
                    rows,
                });
                break;
            }
        }
    }

    /// Salva se mudou. Nota esvaziada vai para a lixeira. Devolve se gravou.
    fn save(&mut self, store: &Store) -> bool {
        let text = self.text();
        if text == self.saved_text {
            return false;
        }
        if text.trim().is_empty() {
            if let Some(p) = self.path.take() {
                let _ = store.trash(&p);
            }
            self.saved_text = text;
            self.status = "nova nota".to_string();
            return true;
        }
        let path = match &self.path {
            Some(p) => p.clone(),
            None => {
                let p = store.new_path();
                self.path = Some(p.clone());
                p
            }
        };
        match store.write(&path, &text) {
            Ok(()) => {
                self.disk_mtime = file_mtime(&path);
                self.saved_text = text;
                self.status = if std::mem::take(&mut self.conflict) {
                    format!("salvo · {} · a versão de fora está na lixeira", store::now_hm())
                } else {
                    format!("salvo · {}", store::now_hm())
                };
            }
            Err(e) => self.status = format!("erro ao salvar: {e}"),
        }
        true
    }
}

/// Estilo de um trecho. Fora da linha em edição, marcadores (`**`, `#`, URL
/// de link…) ficam invisíveis e com largura ~0, como no Notion.
fn attrs_for(base: Attrs<'static>, f: md::Flags, editing: bool, line_h: f32) -> Attrs<'static> {
    let mut a = base;
    if f.cmarker || ((f.marker || f.url) && !editing) {
        // 1 px (menos que isso quebra a linha) com avanço anulado pelo letter-spacing.
        return a.metrics(Metrics::new(1.0, line_h)).letter_spacing(-0.6).color(TRANSPARENT);
    }
    if f.bold {
        a = a.weight(Weight::BOLD);
    }
    if f.italic {
        a = a.style(Style::Italic);
    }
    if f.code || f.mono {
        a = a.family(Family::Monospace);
    }
    if f.strike {
        a = a.strikethrough();
    }
    if f.underline {
        a = a.underline(UnderlineStyle::Single);
    }
    let color = if f.hidden {
        TRANSPARENT
    } else if f.marker {
        MARKER
    } else if let Some(c) = f.color {
        rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
    } else if f.dim || f.url {
        DIM
    } else if f.code {
        white(0xd9)
    } else {
        WHITE
    };
    a.color(color)
}

// =====================================================================
// Estado da interface (tudo que o desenho precisa, sem objetos Wayland).
// =====================================================================

#[derive(Default)]
struct Hits {
    list_btn: Rect,
    new_btn: Rect,
    trash_btn: Rect,
    close_btn: Rect,
    header: Rect,
    editor: Rect,
    /// Origem (x, y) da coluna de texto na tela.
    text_origin: (i32, i32),
    panel: Rect,
    rows: Vec<(Rect, usize)>,
    tabs: Vec<(Rect, usize)>,
    tab_closes: Vec<(Rect, usize)>,
    /// (retângulo, linha, índice do `[`)
    checkboxes: Vec<(Rect, usize, usize)>,
    /// (retângulo, linha, índice do `▸`/`▾`)
    toggles: Vec<(Rect, usize, usize)>,
    /// (retângulo na tela, bloco, coluna)
    cols: Vec<(Rect, usize, usize)>,
    /// Divisores arrastáveis: (área de arraste, bloco, coluna à direita).
    col_divs: Vec<(Rect, usize, usize)>,
    cells: Vec<CellHit>,
    images: Vec<ImgHit>,
}

enum Deco {
    Tri { rect: Rect, open: bool, line: usize, idx: usize },
    Rect { x: i32, y: i32, w: i32, h: i32, r: i32, c: Color },
    Line { x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: Color },
    Check { rect: Rect, checked: bool, line: usize, idx: usize },
}

struct Ui {
    font_system: FontSystem,
    swash: SwashCache,
    fonts_full: bool,
    /// Banco completo do sistema sendo carregado em 2º plano.
    fonts_loading: bool,
    /// Caracteres já confirmados nas fontes curadas.
    font_probe: HashSet<char>,
    tabs: Vec<Tab>,
    active: usize,
    width: u32,
    height: u32,
    scale: i32,
    zoom: i32,
    csd: bool,
    focused: bool,
    cursor_visible: bool,
    hover: (i32, i32),
    hits: Hits,
    /// Divisor de colunas sob o mouse (bloco, coluna à direita).
    col_hover: Option<(usize, usize)>,
    /// Arraste de um divisor de colunas em andamento.
    col_drag: Option<ColDrag>,
    traced: bool,
    prof: Option<Prof>,
    labels: Vec<(LabelKey, Buffer)>,
    glyphs: Option<Glyphs>,
    images: ImageCache,

    store: Store,

    list_open: bool,
    notes: Vec<NoteMeta>,
    filtered: Vec<usize>,
    query: String,
    list_sel: usize,
    list_top: usize,
    picker: Option<Picker>,
    slash: Option<Slash>,
}

impl Ui {
    fn font_px(&self) -> f32 {
        self.zoom as f32 * self.scale as f32
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let i = self.active;
        &mut self.tabs[i]
    }

    /// Editor da aba ativa junto com o sistema de fontes (campos distintos).
    fn ed(&mut self) -> (&mut FontSystem, &mut Tab) {
        let Ui { font_system, tabs, active, .. } = self;
        (font_system, &mut tabs[*active])
    }

    /// Reaplica estilos na aba ativa (fontes, imagens e zoom atuais).
    fn restyle_active(&mut self) {
        let fp = self.font_px();
        let Ui { font_system, images, tabs, active, .. } = self;
        tabs[*active].restyle(font_system, images, fp);
    }

    /// Divisor de colunas a realçar: o que está sendo arrastado ou sob o mouse.
    fn col_div_active(&self) -> Option<(usize, usize)> {
        self.col_drag.as_ref().map(|d| (d.bi, d.ci)).or(self.col_hover)
    }

    fn new_tab(&mut self) -> usize {
        let fp = self.font_px();
        let t = Tab::new(&mut self.font_system, fp);
        self.tabs.push(t);
        self.tabs.len() - 1
    }

    /// Abre `path` (ou nota nova com `None`) numa aba: reutiliza uma aba
    /// vazia, ou a que já tem a nota, senão cria.
    fn open_in_tab(&mut self, path: Option<PathBuf>) {
        if let Some(p) = &path {
            if let Some(i) = self.tabs.iter().position(|t| t.path.as_ref() == Some(p)) {
                self.active = i;
                return;
            }
        }
        let i = if !self.tabs.is_empty() && self.tab().is_empty_new() { self.active } else { self.new_tab() };
        self.active = i;
        let text = path.as_deref().and_then(|p| self.store.read(p).ok()).unwrap_or_default();
        let mtime = path.as_deref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
        let fp = self.font_px();
        let Ui { font_system, images, tabs, .. } = self;
        let tab = &mut tabs[i];
        tab.load(&text, font_system, images, fp);
        tab.disk_mtime = mtime;
        tab.path = path;
        tab.status = match mtime {
            Some(t) => format!("salvo · {}", store::fmt_date(t)),
            None => "nova nota".to_string(),
        };
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs[i].save(&self.store);
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab();
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if i < self.active {
            self.active -= 1;
        }
        self.persist_state();
    }

    fn save_all(&mut self) {
        let mut any = false;
        for t in &mut self.tabs {
            any |= t.save(&self.store);
        }
        if any {
            self.persist_state();
        }
    }

    fn persist_state(&self) {
        let name = |p: &Option<PathBuf>| p.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_string);
        let tabs: Vec<String> = self.tabs.iter().filter_map(|t| name(&t.path)).collect();
        let last = name(&self.tab().path);
        self.store.save_state(&State {
            last,
            tabs,
            active: self.active,
            width: self.width,
            height: self.height,
            zoom: self.zoom,
        });
    }

    fn set_zoom(&mut self, z: i32) {
        self.zoom = z.clamp(ZOOM_MIN, ZOOM_MAX);
        self.persist_state();
    }

    /// Alinha as colunas de todos os blocos de tabela da aba ativa, exceto a
    /// linha `except` (onde o usuário está digitando). Devolve se alterou.
    fn format_tables(&mut self, except: Option<usize>) -> bool {
        let (_, tab) = self.ed();
        let texts = tab.line_texts();
        let is_table = |i: usize| matches!(tab.lines.get(i).map(|l| l.block), Some(Block::Table | Block::TableSep));
        let mut any = false;
        let mut i = 0;
        while i < texts.len() {
            if !is_table(i) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < texts.len() && is_table(j) {
                j += 1;
            }
            let block: Vec<&str> = texts[i..j].iter().map(String::as_str).collect();
            let formatted = md::format_table(&block);
            tab.editor.start_change();
            for (k, new) in formatted.iter().enumerate() {
                let li = i + k;
                if Some(li) == except || *new == texts[li] {
                    continue;
                }
                tab.editor.delete_range(Cursor::new(li, 0), Cursor::new(li, texts[li].len()));
                tab.editor.insert_at(Cursor::new(li, 0), new, None);
                any = true;
            }
            if let Some(ch) = tab.editor.finish_change() {
                if !ch.items.is_empty() {
                    tab.undo.record(ch, false);
                }
            }
            i = j;
        }
        any
    }

    /// As fontes curadas (Noto) cobrem latim, grego, cirílico, símbolos,
    /// matemática e emoji. O banco completo do sistema (centenas de arquivos,
    /// ~45 ms de CPU) só é carregado se aparecer um caractere que nenhuma
    /// fonte curada tem (CJK, árabe, hebraico, índico…).
    fn text_needs_full_fonts(&mut self, text: &str) -> bool {
        if self.fonts_full || self.fonts_loading {
            return false;
        }
        let mut ids: Option<Vec<fontdb::ID>> = None;
        for c in text.chars() {
            if c.is_ascii() || c.is_whitespace() || is_format_char(c) || self.font_probe.contains(&c) {
                continue;
            }
            let ids = ids.get_or_insert_with(|| self.font_system.db().faces().map(|f| f.id).collect());
            let covered = ids.iter().any(|&id| {
                self.font_system
                    .get_font(id, fontdb::Weight::NORMAL)
                    .map(|f| f.as_swash().charmap().map(c) != 0)
                    .unwrap_or(false)
            });
            if !covered {
                return true;
            }
            self.font_probe.insert(c);
        }
        false
    }

    // ---------- lista ----------

    fn open_list(&mut self) {
        self.notes = self.store.list();
        self.query.clear();
        self.list_open = true;
        self.refilter();
        if let Some(cur) = &self.tab().path {
            if let Some(pos) = self.filtered.iter().position(|&i| &self.notes[i].path == cur) {
                self.list_sel = pos;
            }
        }
    }

    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.filtered = (0..self.notes.len())
            .filter(|&i| words.iter().all(|w| self.notes[i].haystack.contains(w)))
            .collect();
        self.list_sel = 0;
        self.list_top = 0;
    }

    fn list_move(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as i32;
        self.list_sel = (self.list_sel as i32 + delta).rem_euclid(n) as usize;
    }

    fn selected_note_path(&self) -> Option<PathBuf> {
        self.filtered.get(self.list_sel).map(|&i| self.notes[i].path.clone())
    }

    // ---------- desenho ----------

    fn px(&self, v: f32) -> i32 {
        (v * self.scale as f32).round() as i32
    }

    fn make_text(&mut self, text: &str, size: f32, weight: Weight, max_w: Option<f32>) -> Buffer {
        let s = self.scale as f32;
        let mut b = Buffer::new(&mut self.font_system, Metrics::new(size * s, (size * s * UI_LEADING).round()));
        b.set_size(max_w, None);
        b.set_wrap(Wrap::None);
        b.set_ellipsize(ELLIPSIZE);
        let attrs = Attrs::new().family(Family::SansSerif).weight(weight).letter_spacing(tracking(size * s));
        b.set_text(text, &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.font_system, false);
        b
    }

    fn text_width(b: &Buffer) -> f32 {
        b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    /// Texto numa linha; `align`: 0 = esquerda, 1 = centro, 2 = direita. Devolve a largura.
    /// Os rótulos moldados ficam num cache pequeno: títulos de abas, status e
    /// dicas mudam raramente e não precisam ser re-moldados a cada quadro.
    #[allow(clippy::too_many_arguments)]
    fn label(&mut self, canvas: &mut Canvas, text: &str, x: i32, cy: i32, size: f32, weight: Weight, color: Color, align: u8, max_w: Option<f32>) -> i32 {
        let key = LabelKey { text: text.to_string(), size: size.to_bits(), weight: weight.0, max_w: max_w.map(f32::to_bits), scale: self.scale };
        let idx = match self.labels.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                let b = self.make_text(text, size, weight, max_w);
                if self.labels.len() >= LABEL_CACHE {
                    self.labels.remove(0);
                }
                self.labels.push((key, b));
                self.labels.len() - 1
            }
        };
        let lh = (size * self.scale as f32 * UI_LEADING).round() as i32;
        let Ui { labels, font_system, swash, .. } = self;
        let b = &labels[idx].1;
        let w = Self::text_width(b).ceil() as i32;
        let x0 = match align {
            1 => x - w / 2,
            2 => x - w,
            _ => x,
        };
        let mut r = FastRenderer { canvas, fs: font_system, cache: swash, ox: x0, oy: cy - lh / 2 };
        render_buffer(b, &mut r, color);
        w
    }

    fn icon_button(&mut self, canvas: &mut Canvas, r: Rect, kind: u8) {
        let hovered = r.contains(self.hover.0, self.hover.1);
        if hovered {
            canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), HOVER);
        }
        let c = if hovered { WHITE } else { ICON };
        let t = self.px(STROKE).max(1);
        let cx = r.x + r.w / 2;
        let cy = r.y + r.h / 2;
        let half = self.px(ICON_HALF);
        match kind {
            0 => {
                for i in -1..=1 {
                    let y = cy + i * self.px(6.0) - t / 2;
                    canvas.rect(cx - half, y, half * 2, t, c);
                }
            }
            1 => {
                canvas.rect(cx - half, cy - t / 2, half * 2, t, c);
                canvas.rect(cx - t / 2, cy - half, t, half * 2, c);
            }
            2 => {
                let top = cy - half;
                canvas.rect(cx - half, top + self.px(2.0), half * 2, t, c);
                canvas.rect(cx - self.px(3.0), top, self.px(6.0), t, c);
                let by = top + self.px(5.0);
                let bh = half * 2 - self.px(5.0);
                let bw = half * 2 - self.px(4.0);
                canvas.rect(cx - bw / 2, by, t, bh, c);
                canvas.rect(cx + bw / 2 - t, by, t, bh, c);
                canvas.rect(cx - bw / 2, by + bh - t, bw, t, c);
            }
            _ => {
                let d = self.px(5.0);
                canvas.line(cx - d, cy - d, cx + d, cy + d, t, c);
                canvas.line(cx + d, cy - d, cx - d, cy + d, t, c);
            }
        }
    }

    fn paint(&mut self, canvas: &mut Canvas) {
        let t = |ui: &Ui, l: &str| if !ui.traced { trace(l) };
        t(self, "paint: início");
        let mut pt = Instant::now();
        let w = canvas.w;
        let h = canvas.h;
        let header_h = self.px(HEADER_H);
        let footer_h = self.px(FOOTER_H);
        canvas.fill(BLACK);

        // ---- cabeçalho: botões + abas ----
        canvas.rect(0, header_h - 1, w, 1, LINE);
        let bsz = self.px(BUTTON);
        let by = (header_h - bsz) / 2;
        let gap = self.px(SP2);
        let mut x = self.px(SP2);
        let list_btn = Rect::new(x, by, bsz, bsz);
        x += bsz + gap;
        let new_btn = Rect::new(x, by, bsz, bsz);
        x += bsz + gap * 2;
        let mut rx = w - self.px(SP2) - bsz;
        let close_btn = if self.csd {
            let r = Rect::new(rx, by, bsz, bsz);
            rx -= bsz + gap;
            r
        } else {
            Rect::default()
        };
        let trash_btn = Rect::new(rx, by, bsz, bsz);
        self.icon_button(canvas, list_btn, 0);
        self.icon_button(canvas, new_btn, 1);
        self.icon_button(canvas, trash_btn, 2);
        if self.csd {
            self.icon_button(canvas, close_btn, 3);
        }
        let (tabs, tab_closes) = self.paint_tabs(canvas, x, rx - gap * 2, header_h);
        t(self, "paint: cabeçalho+abas");
        prof_add(&mut self.prof, P_HEADER, pt);
        pt = Instant::now();

        // ---- editor ----
        let editor = Rect::new(0, header_h, w, h - header_h - footer_h);
        let ml = self.px(TEXT_MARGIN_X);
        let mt = self.px(TEXT_MARGIN_TOP);
        let font_px = self.font_px();
        // Medida limitada (~70 caracteres) e coluna centralizada em janelas largas.
        let measure = (MEASURE_EM * font_px).round() as i32;
        let text_w = (editor.w - 2 * ml).min(measure).max(self.px(40.0)) as f32;
        let text_h = (editor.h - mt).max(self.px(20.0)) as f32;
        let line_height = body_lh(font_px) as i32;
        let (zoom, scale) = (self.zoom, self.scale);
        let focused = self.focused && self.cursor_visible && !self.list_open;
        let cursor_w = self.px(2.0).max(1);
        let ox = editor.x + ml;
        let oy = editor.y + mt;
        let mut checkboxes = Vec::new();
        let mut toggles = Vec::new();
        let mut cols = Vec::new();
        let mut col_divs = Vec::new();
        let mut cells = Vec::new();
        let mut img_hits = Vec::new();
        let div_active = self.col_div_active();
        {
            let Ui { font_system, swash, tabs, active, traced, images, prof, .. } = self;
            let traced_flag = traced;
            let tab = &mut tabs[*active];
            if tab.metrics_key != (zoom, scale) {
                tab.metrics_key = (zoom, scale);
                tab.editor.with_buffer_mut(|b| b.set_metrics(Metrics::new(font_px, body_lh(font_px))));
                tab.restyle(font_system, images, font_px);
            }
            let wide_w = (editor.right() - ml - ox).max(text_w as i32) as f32;
            if tab.editor_size != (text_w, text_h) || tab.wide_w != wide_w {
                tab.editor_size = (text_w, text_h);
                tab.wide_w = wide_w;
                tab.editor.with_buffer_mut(|b| b.set_size(Some(text_w), Some(text_h)));
                tab.restyle(font_system, images, font_px);
            }
            tab.editor.shape_as_needed(font_system, true);
            if !*traced_flag { trace("paint: shaping"); }
            prof_add(prof, P_SHAPE, pt);
            pt = Instant::now();
            canvas.set_clip(editor);
            {
                let mut r = FastRenderer { canvas, fs: font_system, cache: swash, ox, oy };
                render_editor(&tab.editor, &tab.line_shift, &mut r, WHITE, SELECTION);
            }
            if !*traced_flag { trace("paint: glifos desenhados"); }
            prof_add(prof, P_GLYPHS, pt);
            pt = Instant::now();
            let all: Vec<&md::LineInfo> = tab.lines.iter().collect();
            let decos = tab.editor.with_buffer(|b| collect_decos(b, &all, font_px, text_w, scale, &[], &tab.line_shift));
            draw_decos(canvas, decos, ox, oy, &mut checkboxes, &mut toggles);
            // ---- colunas: sub-buffers no espaço da linha `:::` ----
            let sel = tab.editor.selection_bounds();
            let cursor = tab.editor.cursor();
            let block_tops: Vec<(usize, i32)> = tab.editor.with_buffer(|b| {
                b.layout_runs().filter(|r| tab.lines.get(r.line_i).is_some_and(|l| l.block == Block::ColStart)).map(|r| (r.line_i, r.line_top as i32)).collect()
            });
            let pad = (font_px * 0.5).round() as i32;
            for (bi, block) in tab.columns.iter_mut().enumerate() {
                let Some(&(_, top)) = block_tops.iter().find(|(l, _)| *l == block.start) else { continue };
                let y0 = oy + top + pad;
                let inner_h = block.height as i32 - 2 * pad;
                let ncols = block.cols.len();
                let block_gap = block.gap as i32;
                for (ci, col) in block.cols.iter_mut().enumerate() {
                    let x0 = ox + col.x as i32;
                    let rect = Rect::new(x0, y0, col.w as i32, inner_h);
                    canvas.set_clip(rect.intersect(&editor));
                    if ci > 0 {
                        let gx = x0 - block_gap / 2;
                        let hot = div_active == Some((bi, ci));
                        canvas.set_clip(editor);
                        if hot {
                            canvas.rect(gx - 1, y0, 3, inner_h, DIM);
                        } else {
                            canvas.rect(gx, y0, 1, inner_h, LINE);
                        }
                        canvas.set_clip(rect.intersect(&editor));
                        col_divs.push((Rect::new(gx - COL_GRIP, y0, 2 * COL_GRIP + 1, inner_h), bi, ci));
                    }
                    if let Some((s, e)) = sel {
                        if s.line <= col.last && e.line >= col.first && !col.map.is_empty() {
                            let n_sub = col.map.len();
                            let cs = match col.map.iter().position(|&m| m >= s.line) {
                                Some(k) if col.map[k] == s.line => Cursor::new(k, s.index),
                                Some(k) => Cursor::new(k, 0),
                                None => Cursor::new(n_sub - 1, usize::MAX / 2),
                            };
                            let ce = match col.map.iter().rposition(|&m| m <= e.line) {
                                Some(k) if col.map[k] == e.line => Cursor::new(k, e.index),
                                Some(k) => Cursor::new(k, usize::MAX / 2),
                                None => Cursor::new(0, 0),
                            };
                            for run in col.buffer.layout_runs() {
                                for (hx, hw) in run.highlight(cs, ce) {
                                    canvas.rect(x0 + hx as i32, y0 + run.line_top as i32, hw.max(1.0) as i32, run.line_height as i32, SELECTION);
                                }
                            }
                        }
                    }
                    {
                        let mut r = FastRenderer { canvas, fs: font_system, cache: swash, ox: x0, oy: y0 };
                        let (shift, map) = (&tab.line_shift, &col.map);
                        render_buffer_shifted(&col.buffer, &mut r, WHITE, |sub| map.get(sub).and_then(|&m| shift.get(m)).copied().unwrap_or(0));
                    }
                    if !col.map.is_empty() {
                        let infos: Vec<&md::LineInfo> = col.map.iter().map(|&m| &tab.lines[m]).collect();
                        let decos = collect_decos(&col.buffer, &infos, font_px, col.w, scale, &col.map, &tab.line_shift);
                        draw_decos(canvas, decos, x0, y0, &mut checkboxes, &mut toggles);
                        if let (true, Some(sl)) = (focused, col.sub_line(cursor.line)) {
                            let sub = Cursor::new(sl, cursor.index);
                            if let Some((cx, cy)) = col.buffer.cursor_position(&sub) {
                                let lh = col.buffer.layout_runs().find(|r| r.line_i == sub.line).map(|r| r.line_height as i32).unwrap_or(line_height);
                                let dy = tab.line_shift.get(cursor.line).copied().unwrap_or(0);
                                canvas.rect(x0 + cx as i32, y0 + cy as i32 + dy, cursor_w, lh, WHITE);
                            }
                        }
                    }
                    cols.push((rect, bi, ci));
                    if ci + 1 == ncols {
                        // Borda direita: alça invisível até o hover.
                        let gx = x0 + col.w as i32 + block_gap / 2;
                        if div_active == Some((bi, ci + 1)) {
                            canvas.set_clip(editor);
                            canvas.rect(gx - 1, y0, 3, inner_h, DIM);
                        }
                        col_divs.push((Rect::new(gx - COL_GRIP, y0, 2 * COL_GRIP + 1, inner_h), bi, ci + 1));
                    }
                }
                canvas.set_clip(editor);
            }
            // ---- tabelas: células proporcionais no espaço das linhas `|` ----
            let row_tops: Vec<(usize, i32)> = tab.editor.with_buffer(|b| {
                b.layout_runs()
                    .filter(|r| tab.lines.get(r.line_i).is_some_and(|l| l.block == Block::Table && l.col.is_none()))
                    .map(|r| (r.line_i, r.line_top as i32))
                    .collect()
            });
            for (bi, tb) in tab.tables.iter_mut().enumerate() {
                let tw = tb.w as i32;
                let (hpad, vpad, rh) = (tb.hpad as i32, tb.vpad as i32, tb.row_h as i32);
                let cur_lh = body_lh(tb.font) as i32;
                for (ri, row) in tb.rows.iter_mut().enumerate() {
                    let Some(&(_, top)) = row_tops.iter().find(|(l, _)| *l == row.line) else { continue };
                    let y = oy + top;
                    let header = tab.lines[row.line].header;
                    // Cobre o realce de seleção da linha crua (1 px) sob a tabela.
                    canvas.rect(ox, y, tw, rh, BLACK);
                    if ri == 0 {
                        canvas.rect(ox, y, tw, 1, RULE_SOFT);
                    }
                    canvas.rect(ox, y + rh - 1, tw, 1, if header { RULE_STRONG } else { RULE_SOFT });
                    let cursor_cell = (cursor.line == row.line).then(|| cell_index(&tab.lines[row.line].pipes, cursor.index, row.cells.len().max(1)));
                    for (ci, cell) in row.cells.iter_mut().enumerate() {
                        let cw = tb.col_w[ci] as i32;
                        let cx = ox + tb.col_x[ci] as i32;
                        let tx = match tb.align[ci] {
                            2 => cx + cw - hpad - cell.w as i32,
                            1 => cx + (cw - cell.w as i32) / 2,
                            _ => cx + hpad,
                        };
                        let ty = y + vpad;
                        let rect = Rect::new(cx, y, cw, rh);
                        canvas.set_clip(rect.intersect(&editor));
                        if let Some((s, e)) = sel {
                            let l = row.line;
                            if s.line <= l && e.line >= l {
                                let len = cell.end - cell.start;
                                let ls = if s.line < l { 0 } else { s.index.saturating_sub(cell.start).min(len) };
                                let le = if e.line > l { len } else { e.index.saturating_sub(cell.start).min(len) };
                                if le > ls {
                                    for run in cell.buffer.layout_runs() {
                                        for (hx, hw) in run.highlight(Cursor::new(0, ls), Cursor::new(0, le)) {
                                            canvas.rect(tx + hx as i32, ty + run.line_top as i32, hw.max(1.0) as i32, run.line_height as i32, SELECTION);
                                        }
                                    }
                                }
                            }
                        }
                        {
                            let mut r = FastRenderer { canvas, fs: font_system, cache: swash, ox: tx, oy: ty };
                            render_buffer(&cell.buffer, &mut r, WHITE);
                        }
                        if focused && cursor_cell == Some(ci) {
                            let sub = cursor.index.saturating_sub(cell.start).min(cell.end - cell.start);
                            if let Some((px, py)) = cell.buffer.cursor_position(&Cursor::new(0, sub)) {
                                canvas.rect(tx + px as i32, ty + py as i32, cursor_w, cur_lh, WHITE);
                            }
                        }
                        cells.push(CellHit { rect, line: row.line, bi, ri, ci, tx });
                    }
                    canvas.set_clip(editor);
                }
            }
            let in_table = tab.tables.iter().any(|t| cursor.line >= t.start && cursor.line <= t.end);
            let in_column = tab.column_of(cursor.line).is_some();
            // Imagens em linha: no lugar do glifo reservado (`!` do marcador).
            let placements: Vec<(usize, i32, i32, i32)> = tab.editor.with_buffer(|b| {
                let mut v = Vec::new();
                for run in b.layout_runs() {
                    for (k, im) in tab.inline_imgs.iter().enumerate() {
                        if im.line != run.line_i {
                            continue;
                        }
                        if let Some(g) = run.glyphs.iter().find(|g| g.start == im.idx) {
                            let dy = tab.line_shift.get(run.line_i).copied().unwrap_or(0);
                            let x = g.physical((0.0, run.line_y), 1.0).x;
                            let y = run.line_top as i32 + ((run.line_height as i32 - im.h as i32) / 2).max(0) + dy;
                            v.push((k, x, y, run.line_i as i32));
                        }
                    }
                }
                v
            });
            for (k, x, y, _) in placements {
                let im = &tab.inline_imgs[k];
                if let Some(bm) = images.scaled(&im.path, im.w) {
                    canvas.blit(ox + x, oy + y, bm.w, bm.h, &bm.rgba);
                    img_hits.push(ImgHit { rect: Rect::new(ox + x, oy + y, bm.w as i32, bm.h as i32), line: im.line, start: im.start, end: im.end });
                }
            }
            if focused && !in_column && !in_table {
                if let Some((cx, cy)) = tab.editor.cursor_position() {
                    let lh = tab.editor.with_buffer(|b| {
                        b.layout_runs().find(|r| r.line_i == tab.editor.cursor().line).map(|r| r.line_height as i32)
                    });
                    let dy = tab.line_shift.get(cursor.line).copied().unwrap_or(0);
                    canvas.rect(ox + cx, oy + cy + dy, cursor_w, lh.unwrap_or(line_height), WHITE);
                }
            }
            canvas.reset_clip();
            prof_add(prof, P_DECOS, pt);
            pt = Instant::now();
        }

        // ---- rodapé ----
        let fy = h - footer_h;
        canvas.rect(0, fy, w, 1, LINE);
        let fcy = fy + footer_h / 2;
        let (status, words, chars) = {
            let t = self.tab();
            (t.status.clone(), t.words, t.chars)
        };
        let counter = format!(
            "{} {}  ·  {} {}",
            words,
            if words == 1 { "palavra" } else { "palavras" },
            chars,
            if chars == 1 { "caractere" } else { "caracteres" }
        );
        let cw = self.label(canvas, &counter, w - self.px(SP4), fcy, UI_FONT_SM, Weight::NORMAL, DIM2, 2, None);
        let sw = self.label(canvas, &status, self.px(SP4), fcy, UI_FONT_SM, Weight::NORMAL, DIM, 0, Some((w / 3) as f32));
        let hint = "Ctrl+L notas · Ctrl+N nova · Ctrl+. emoji · Ctrl+, símbolos · Shift+Enter quebra curta · Esc sair";
        let hx = self.px(SP4) + sw + self.px(SP6);
        let hint_max = (w - self.px(SP4) - cw - self.px(SP6) - hx).max(0) as f32;
        if hint_max > self.px(60.0) as f32 {
            self.label(canvas, hint, hx, fcy, UI_FONT_SM, Weight::NORMAL, DIM2, 0, Some(hint_max));
        }

        t(self, "paint: rodapé");
        self.traced = true;
        prof_add(&mut self.prof, P_FOOTER, pt);
        pt = Instant::now();

        // ---- painel de notas ----
        let mut panel = Rect::default();
        let mut rows = Vec::new();
        if self.list_open {
            self.paint_list(canvas, header_h, &mut panel, &mut rows);
        }
        if self.picker.is_some() {
            self.paint_picker(canvas, fy);
        }
        if self.glyphs.is_some() {
            self.paint_glyphs(canvas, header_h);
        }
        if self.slash.is_some() {
            self.paint_slash(canvas, ox, oy, editor);
        }
        prof_add(&mut self.prof, P_PANELS, pt);

        self.hits = Hits {
            list_btn,
            new_btn,
            trash_btn,
            close_btn,
            header: Rect::new(0, 0, w, header_h),
            editor,
            text_origin: (ox, oy),
            panel,
            rows,
            tabs,
            tab_closes,
            checkboxes,
            toggles,
            cols,
            col_divs,
            cells,
            images: img_hits,
        };
    }

    fn paint_tabs(&mut self, canvas: &mut Canvas, x0: i32, x1: i32, header_h: i32) -> (Vec<(Rect, usize)>, Vec<(Rect, usize)>) {
        let n = self.tabs.len().max(1) as i32;
        let gap = self.px(SP1);
        let avail = (x1 - x0).max(self.px(40.0));
        let tab_w = ((avail + gap) / n).clamp(self.px(TAB_MIN_W), self.px(TAB_MAX_W));
        let th = self.px(TAB_H);
        let ty = (header_h - th) / 2;
        let mut tabs = Vec::new();
        let mut closes = Vec::new();
        for i in 0..self.tabs.len() {
            let r = Rect::new(x0 + i as i32 * tab_w, ty, tab_w - gap, th);
            if r.right() > x1 + gap {
                break;
            }
            let active = i == self.active;
            let hovered = r.contains(self.hover.0, self.hover.1);
            if active {
                canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), TAB_ACTIVE);
            } else if hovered {
                canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), HOVER);
            }
            let show_close = (active || hovered) && r.w >= self.px(72.0);
            let title = self.tabs[i].title.clone();
            let pad = self.px(TAB_PAD);
            let max_w = (r.w - pad * 2 - if show_close { self.px(TAB_CLOSE) } else { 0 }).max(self.px(10.0)) as f32;
            let color = if active { WHITE } else { DIM };
            let weight = if active { Weight::SEMIBOLD } else { Weight::NORMAL };
            self.label(canvas, &title, r.x + pad, r.y + r.h / 2, UI_FONT, weight, color, 0, Some(max_w));
            if show_close {
                let cs = self.px(TAB_CLOSE);
                let cr = Rect::new(r.right() - cs - self.px(SP1), r.y + (r.h - cs) / 2, cs, cs);
                let ch = cr.contains(self.hover.0, self.hover.1);
                if ch {
                    canvas.rounded_rect(cr.x, cr.y, cr.w, cr.h, self.px(RADIUS_SM), HOVER);
                }
                let d = self.px(4.0);
                let (cx, cy) = (cr.x + cr.w / 2, cr.y + cr.h / 2);
                let c = if ch { WHITE } else { DIM };
                let t = self.px(1.5).max(1);
                canvas.line(cx - d, cy - d, cx + d, cy + d, t, c);
                canvas.line(cx + d, cy - d, cx - d, cy + d, t, c);
                closes.push((cr, i));
            }
            tabs.push((r, i));
        }
        (tabs, closes)
    }

    /// Itens do menu `/` que casam com o que foi digitado depois da barra.
    fn slash_matches(&self) -> Vec<usize> {
        let Some(sl) = &self.slash else { return Vec::new() };
        let tab = self.tab();
        let text = tab.line_text(sl.line);
        let cur = tab.editor.cursor().index.min(text.len());
        let q = text.get(sl.start + 1..cur).unwrap_or("").to_lowercase();
        SLASH_ITEMS
            .iter()
            .enumerate()
            .filter(|(_, (label, key, words))| q.is_empty() || label.to_lowercase().contains(&q) || key.contains(&q) || words.contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    fn paint_slash(&mut self, canvas: &mut Canvas, ox: i32, oy: i32, editor: Rect) {
        let matches = self.slash_matches();
        let sel = self.slash.as_ref().map(|s| s.sel).unwrap_or(0).min(matches.len().saturating_sub(1));
        if let Some(sl) = self.slash.as_mut() {
            sl.sel = sel;
        }
        let (cx, cy, lh) = {
            let tab = self.tab();
            let pos = tab.editor.cursor_position().unwrap_or((0, 0));
            let lh = tab.editor.with_buffer(|b| b.layout_runs().find(|r| r.line_i == tab.editor.cursor().line).map(|r| r.line_height as i32)).unwrap_or(22);
            (ox + pos.0, oy + pos.1, lh)
        };
        let row_h = self.px(MENU_ROW_H);
        let pad = self.px(SP2);
        let n = matches.len().max(1);
        let pw = self.px(MENU_W);
        let ph = pad * 2 + row_h * n as i32;
        let mut px0 = cx.min(editor.right() - pw - self.px(SP2)).max(editor.x + self.px(SP2));
        let mut py0 = cy + lh + self.px(SP1);
        if py0 + ph > editor.bottom() {
            py0 = (cy - ph - self.px(SP1)).max(editor.y);
        }
        px0 = px0.max(0);
        for i in 1..=3 {
            let o = self.px(2.0) * i;
            canvas.rounded_rect(px0 - o / 2, py0 + o / 2, pw + o, ph + o, self.px(RADIUS_LG) + o / 2, white(6));
        }
        canvas.rounded_outline(px0, py0, pw, ph, self.px(RADIUS_LG), BORDER, BLACK);
        if matches.is_empty() {
            self.label(canvas, "Nada encontrado", px0 + pad + self.px(SP3), py0 + pad + row_h / 2, UI_FONT, Weight::NORMAL, DIM, 0, Some((pw - 2 * pad) as f32));
        }
        for (vi, &mi) in matches.iter().enumerate() {
            let r = Rect::new(px0 + pad, py0 + pad + row_h * vi as i32, pw - 2 * pad, row_h);
            if vi == sel {
                canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS_SM), ROW_SEL);
            }
            let (label, key, _) = SLASH_ITEMS[mi];
            self.label(canvas, label, r.x + self.px(SP3), r.y + r.h / 2, UI_FONT, if vi == sel { Weight::SEMIBOLD } else { Weight::NORMAL }, WHITE, 0, Some((r.w - self.px(72.0)) as f32));
            self.label(canvas, key, r.right() - self.px(SP3), r.y + r.h / 2, UI_FONT_XS, Weight::NORMAL, DIM2, 2, None);
        }
    }

    /// Seletor de emoji/símbolos: busca em cima, grade no meio, nome embaixo.
    fn paint_glyphs(&mut self, canvas: &mut Canvas, header_h: i32) {
        let Some(g) = self.glyphs.as_ref() else { return };
        let (kind, query, sel, top) = (g.kind, g.query.clone(), g.sel, g.top);
        let filtered = g.filtered.clone();
        let cell = self.px(GLYPH_CELL);
        let pad = self.px(SP2);
        let search_h = self.px(SEARCH_H);
        let name_h = self.px(SP8);
        let pw = pad * 2 + cell * GLYPH_COLS as i32;
        let ph = pad * 4 + search_h + cell * GLYPH_ROWS as i32 + name_h;
        let panel = Rect::new((canvas.w - pw) / 2, header_h + self.px(SP2), pw, ph);
        for i in 1..=4 {
            let o = self.px(2.0) * i;
            canvas.rounded_rect(panel.x - o / 2, panel.y + o / 2, panel.w + o, panel.h + o, self.px(RADIUS_LG) + o / 2, white(6));
        }
        canvas.rounded_outline(panel.x, panel.y, panel.w, panel.h, self.px(RADIUS_LG), BORDER, BLACK);
        let sr = Rect::new(panel.x + pad, panel.y + pad, panel.w - 2 * pad, search_h);
        canvas.rounded_outline(sr.x, sr.y, sr.w, sr.h, self.px(RADIUS), white(0x66), BLACK);
        let tx = sr.x + self.px(SP3);
        let tcy = sr.y + sr.h / 2;
        let caret_h = self.px(SP4);
        let placeholder = match kind {
            GlyphKind::Emoji => "Buscar emoji… (pt/en)",
            GlyphKind::Symbol => "Buscar símbolo… (seta, check, grau…)",
        };
        if query.is_empty() {
            self.label(canvas, placeholder, tx, tcy, UI_FONT, Weight::NORMAL, DIM, 0, Some((sr.w - self.px(SP6)) as f32));
            canvas.rect(tx, tcy - caret_h / 2, self.px(2.0).max(1), caret_h, WHITE);
        } else {
            let qw = self.label(canvas, &query, tx, tcy, UI_FONT, Weight::NORMAL, WHITE, 0, Some((sr.w - self.px(SP6)) as f32));
            canvas.rect(tx + qw + 1, tcy - caret_h / 2, self.px(2.0).max(1), caret_h, WHITE);
        }
        let gx = panel.x + pad;
        let gy = sr.bottom() + pad;
        let mut cells = Vec::new();
        let all = glyphs(kind);
        let glyph_size = match kind {
            GlyphKind::Emoji => 22.0,
            GlyphKind::Symbol => 20.0,
        };
        if filtered.is_empty() {
            self.label(canvas, "Nada encontrado.", gx + self.px(SP3), gy + cell / 2, UI_FONT, Weight::NORMAL, DIM, 0, None);
        }
        for row in 0..GLYPH_ROWS {
            for col in 0..GLYPH_COLS {
                let fi = (top + row) * GLYPH_COLS + col;
                let Some(&idx) = filtered.get(fi) else { break };
                let r = Rect::new(gx + col as i32 * cell, gy + row as i32 * cell, cell, cell);
                let hovered = r.contains(self.hover.0, self.hover.1);
                if fi == sel {
                    canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), ROW_SEL);
                } else if hovered {
                    canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), HOVER);
                }
                let e = &all[idx];
                self.label(canvas, e.text, r.x + r.w / 2, r.y + r.h / 2, glyph_size, Weight::NORMAL, WHITE, 1, None);
                cells.push((r, fi));
            }
        }
        let total_rows = filtered.len().div_ceil(GLYPH_COLS);
        if total_rows > GLYPH_ROWS {
            let track_h = cell * GLYPH_ROWS as i32;
            let thumb_h = (track_h * GLYPH_ROWS as i32 / total_rows as i32).max(self.px(SP4));
            let thumb_y = gy + (track_h - thumb_h) * top as i32 / (total_rows - GLYPH_ROWS).max(1) as i32;
            canvas.rounded_rect(panel.right() - pad / 2 - 2, thumb_y, 3, thumb_h, 1, white(0x50));
        }
        let ny = gy + cell * GLYPH_ROWS as i32 + pad + name_h / 2;
        let name = filtered.get(sel).map(|&i| {
            let e = &all[i];
            let groups: &[&str] = match kind {
                GlyphKind::Emoji => &EMOJI_GROUPS,
                GlyphKind::Symbol => &SYMBOL_GROUPS,
            };
            format!("{}  ·  {}", e.name, groups.get(e.group as usize).copied().unwrap_or(""))
        });
        let count = format!("{}  ·  Enter insere · Esc fecha", filtered.len());
        let cw = self.label(canvas, &count, panel.right() - pad - self.px(SP3), ny, UI_FONT_SM, Weight::NORMAL, DIM2, 2, None);
        if let Some(n) = name {
            let max_w = (pw - 2 * pad - 3 * self.px(SP3) - cw).max(self.px(60.0)) as f32;
            self.label(canvas, &n, gx + self.px(SP3), ny, UI_FONT, Weight::SEMIBOLD, WHITE, 0, Some(max_w));
        }
        if let Some(g) = self.glyphs.as_mut() {
            g.panel = panel;
            g.cells = cells;
        }
    }

    /// Roda de cores: matiz no ângulo, saturação no raio, brilho na barra.
    fn paint_picker(&mut self, canvas: &mut Canvas, footer_y: i32) {
        let Some(pk) = self.picker.as_ref() else { return };
        let (h, sat, v) = (pk.h, pk.s, pk.v);
        let radius = self.px(WHEEL_R);
        let pad = self.px(SP4);
        let bar_w = self.px(SP4);
        let info_w = self.px(152.0);
        let pw = pad * 4 + radius * 2 + bar_w + info_w;
        let ph = pad * 2 + radius * 2;
        let panel = Rect::new((canvas.w - pw) / 2, footer_y - ph - self.px(SP3), pw, ph);
        for i in 1..=4 {
            let o = self.px(2.0) * i;
            canvas.rounded_rect(panel.x - o / 2, panel.y + o / 2, panel.w + o, panel.h + o, self.px(RADIUS_LG) + o / 2, white(6));
        }
        canvas.rounded_outline(panel.x, panel.y, panel.w, panel.h, self.px(RADIUS_LG), BORDER, BLACK);

        let (cx, cy) = (panel.x + pad + radius, panel.y + pad + radius);
        let rf = radius as f32;
        for y in (cy - radius - 1)..=(cy + radius + 1) {
            for x in (cx - radius - 1)..=(cx + radius + 1) {
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (rf + 0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                let (r, g, b) = hsv_to_rgb(hue, (d / rf).min(1.0), v);
                canvas.rect(x, y, 1, 1, rgba(r, g, b, (cov * 255.0) as u8));
            }
        }
        // marcador na roda
        let mx = cx + (h.to_radians().cos() * sat * rf) as i32;
        let my = cy + (h.to_radians().sin() * sat * rf) as i32;
        let mr = self.px(6.0);
        canvas.rounded_outline(mx - mr, my - mr, mr * 2, mr * 2, mr, BLACK, TRANSPARENT);
        canvas.rounded_outline(mx - mr + 1, my - mr + 1, mr * 2 - 2, mr * 2 - 2, mr - 1, WHITE, TRANSPARENT);

        // barra de brilho
        let bar = Rect::new(cx + radius + pad, cy - radius, bar_w, radius * 2);
        for y in bar.y..bar.bottom() {
            let vv = 1.0 - (y - bar.y) as f32 / bar.h as f32;
            let (r, g, b) = hsv_to_rgb(h, sat, vv);
            canvas.rect(bar.x, y, bar.w, 1, rgb(r, g, b));
        }
        let by = bar.y + ((1.0 - v) * bar.h as f32) as i32;
        canvas.rect(bar.x - 2, by - 1, bar.w + 4, 3, BLACK);
        canvas.rect(bar.x - 2, by, bar.w + 4, 1, WHITE);

        // amostra + hex + dicas
        let hex = pk.hex();
        let ix = bar.right() + pad;
        let sw = self.px(48.0);
        canvas.rounded_rect(ix, cy - radius, sw, sw, self.px(RADIUS), rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8));
        let hex_text = format!("#{hex:06x}");
        self.label(canvas, &hex_text, ix + sw + self.px(SP3), cy - radius + sw / 2, 14.0, Weight::SEMIBOLD, WHITE, 0, None);
        let hints = ["mova o mouse: prévia", "clique ou Enter: aplicar", "0: remover cor", "Esc: cancelar"];
        for (i, t) in hints.iter().enumerate() {
            self.label(canvas, t, ix, cy - radius + sw + self.px(SP4) + i as i32 * self.px(18.0), UI_FONT_SM, Weight::NORMAL, DIM, 0, Some(info_w as f32));
        }
        if let Some(pk) = self.picker.as_mut() {
            pk.panel = panel;
            pk.center = (cx, cy);
            pk.radius = radius;
            pk.bar = bar;
        }
    }

    fn paint_list(&mut self, canvas: &mut Canvas, header_h: i32, panel_out: &mut Rect, rows_out: &mut Vec<(Rect, usize)>) {
        let pad = self.px(SP2);
        let pw = (self.px(PANEL_W)).min(canvas.w - 2 * self.px(SP2));
        let search_h = self.px(SEARCH_H);
        let row_h = self.px(ROW_H);
        let max_rows = 8usize;
        let n = self.filtered.len();
        let visible = n.clamp(1, max_rows);
        if self.list_sel < self.list_top {
            self.list_top = self.list_sel;
        } else if self.list_sel >= self.list_top + visible {
            self.list_top = self.list_sel + 1 - visible;
        }
        let ph = pad * 3 + search_h + row_h * visible as i32;
        let panel = Rect::new(self.px(SP2), header_h + self.px(SP2), pw, ph);
        for i in 1..=4 {
            let o = self.px(2.0) * i;
            canvas.rounded_rect(panel.x - o / 2, panel.y + o / 2, panel.w + o, panel.h + o, self.px(RADIUS_LG) + o / 2, white(6));
        }
        canvas.rounded_outline(panel.x, panel.y, panel.w, panel.h, self.px(RADIUS_LG), BORDER, BLACK);

        let sr = Rect::new(panel.x + pad, panel.y + pad, panel.w - 2 * pad, search_h);
        canvas.rounded_outline(sr.x, sr.y, sr.w, sr.h, self.px(RADIUS), white(0x66), BLACK);
        let tx = sr.x + self.px(SP3);
        let tcy = sr.y + sr.h / 2;
        let caret_h = self.px(SP4);
        if self.query.is_empty() {
            self.label(canvas, "Buscar notas…", tx, tcy, UI_FONT, Weight::NORMAL, DIM, 0, Some((sr.w - self.px(SP6)) as f32));
            canvas.rect(tx, tcy - caret_h / 2, self.px(2.0).max(1), caret_h, WHITE);
        } else {
            let q = self.query.clone();
            let qw = self.label(canvas, &q, tx, tcy, UI_FONT, Weight::NORMAL, WHITE, 0, Some((sr.w - self.px(SP6)) as f32));
            canvas.rect(tx + qw + 1, tcy - caret_h / 2, self.px(2.0).max(1), caret_h, WHITE);
        }

        let ry0 = sr.bottom() + pad;
        if n == 0 {
            let msg = if self.notes.is_empty() { "Nenhuma nota ainda — é só começar a escrever." } else { "Nada encontrado." };
            self.label(canvas, msg, panel.x + pad + self.px(SP3), ry0 + row_h / 2, UI_FONT, Weight::NORMAL, DIM, 0, Some((panel.w - 2 * pad) as f32));
        }
        for (vi, fi) in (self.list_top..(self.list_top + visible).min(n)).enumerate() {
            let r = Rect::new(panel.x + pad, ry0 + row_h * vi as i32, panel.w - 2 * pad, row_h);
            let hovered = r.contains(self.hover.0, self.hover.1);
            if fi == self.list_sel {
                canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), ROW_SEL);
            } else if hovered {
                canvas.rounded_rect(r.x, r.y, r.w, r.h, self.px(RADIUS), HOVER);
            }
            let ni = self.filtered[fi];
            let title = self.notes[ni].title.clone();
            let sub = if self.notes[ni].preview.is_empty() {
                store::fmt_date(self.notes[ni].modified)
            } else {
                format!("{}  ·  {}", store::fmt_date(self.notes[ni].modified), self.notes[ni].preview)
            };
            let tx = r.x + self.px(SP3);
            let maxw = Some((r.w - self.px(SP6)) as f32);
            self.label(canvas, &title, tx, r.y + self.px(16.0), UI_FONT, Weight::SEMIBOLD, WHITE, 0, maxw);
            self.label(canvas, &sub, tx, r.y + self.px(33.0), UI_FONT_SM, Weight::NORMAL, DIM, 0, maxw);
            rows_out.push((r, fi));
        }
        *panel_out = panel;
    }
}

/// Desenha decorações com deslocamento (ox, oy) e registra áreas clicáveis.
fn draw_decos(canvas: &mut Canvas, decos: Vec<Deco>, ox: i32, oy: i32, checkboxes: &mut Vec<(Rect, usize, usize)>, toggles: &mut Vec<(Rect, usize, usize)>) {
    for d in decos {
        match d {
            Deco::Tri { rect, open, line, idx } => {
                let r = Rect::new(ox + rect.x, oy + rect.y, rect.w, rect.h);
                let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                let d = r.w * 3 / 10;
                let pts = if open {
                    [(cx - d, cy - d / 2), (cx + d, cy - d / 2), (cx, cy + d / 2 + 1)]
                } else {
                    [(cx - d / 2, cy - d), (cx - d / 2, cy + d), (cx + d / 2 + 1, cy)]
                };
                canvas.triangle(pts, ICON);
                toggles.push((r, line, idx));
            }
            Deco::Rect { x, y, w, h, r, c } => canvas.rounded_rect(ox + x, oy + y, w, h, r, c),
            Deco::Line { x0, y0, x1, y1, t, c } => {
                if y0 == y1 {
                    canvas.rect(ox + x0.min(x1), oy + y0 - t / 2, (x1 - x0).abs(), t, c);
                } else if x0 == x1 {
                    canvas.rect(ox + x0 - t / 2, oy + y0.min(y1), t, (y1 - y0).abs(), c);
                } else {
                    canvas.line(ox + x0, oy + y0, ox + x1, oy + y1, t, c);
                }
            }
            Deco::Check { rect, checked, line, idx } => {
                let r = Rect::new(ox + rect.x, oy + rect.y, rect.w, rect.h);
                let radius = (r.w / 4).max(2);
                if checked {
                    canvas.rounded_rect(r.x, r.y, r.w, r.h, radius, WHITE);
                    let t = (r.w / 7).max(2);
                    let (x0, y0) = (r.x + r.w * 22 / 100, r.y + r.h * 52 / 100);
                    let (x1, y1) = (r.x + r.w * 42 / 100, r.y + r.h * 72 / 100);
                    let (x2, y2) = (r.x + r.w * 78 / 100, r.y + r.h * 30 / 100);
                    canvas.line(x0, y0, x1, y1, t, BLACK);
                    canvas.line(x1, y1, x2, y2, t, BLACK);
                } else {
                    canvas.rounded_outline(r.x, r.y, r.w, r.h, radius, white(0xaa), BLACK);
                }
                checkboxes.push((r, line, idx));
            }
        }
    }
}

/// Decorações Markdown a partir das posições reais dos glifos.
fn collect_decos(b: &Buffer, lines: &[&md::LineInfo], font_px: f32, text_w: f32, scale: i32, map: &[usize], shift: &[i32]) -> Vec<Deco> {
    let mut out = Vec::new();
    let s = scale as f32;
    let thin = (1.0 * s).round().max(1.0) as i32;
    let mut quote_xs: (usize, Vec<i32>) = (usize::MAX, Vec::new());
    let mut last_xs: Vec<i32> = Vec::new();
    {
        for run in b.layout_runs() {
            let Some(info) = lines.get(run.line_i).copied() else { continue };
            let abs_line = map.get(run.line_i).copied().unwrap_or(run.line_i);
            let in_main = map.is_empty();
            if info.folded || (in_main && (info.col.is_some() || matches!(info.block, Block::ColStart | Block::ColSep | Block::ColEnd))) {
                continue;
            }
            let dy = shift.get(abs_line).copied().unwrap_or(0);
            let top = run.line_top as i32 + dy;
            let line_y = run.line_y + dy as f32;
            let h = run.line_height as i32;
            let glyph_at = |idx: usize| run.glyphs.iter().find(|g| g.start == idx);
            match info.block {
                Block::Rule => {
                    out.push(Deco::Line { x0: 0, y0: top + h / 2, x1: text_w as i32, y1: top + h / 2, t: thin, c: white(0x40) });
                }
                Block::Code | Block::Fence => {
                    let x = -(12.0 * s) as i32;
                    out.push(Deco::Line { x0: x, y0: top, x1: x, y1: top + h, t: (2.0 * s) as i32, c: white(0x30) });
                }
                _ => {}
            }
            if !info.quotes.is_empty() {
                if quote_xs.0 != run.line_i {
                    let xs: Vec<i32> = info.quotes.iter().filter_map(|&q| glyph_at(q).map(|g| (g.x + g.w * 0.3) as i32)).collect();
                    quote_xs = (run.line_i, xs);
                }
                for &x in &quote_xs.1 {
                    out.push(Deco::Line { x0: x, y0: top, x1: x, y1: top + h, t: (3.0 * s) as i32, c: white(0x55) });
                }
            }
            if let (Block::Toggle(open), Some(idx)) = (info.block, info.toggle_idx) {
                if let Some(g) = glyph_at(idx) {
                    let side = (font_px * 0.9).round() as i32;
                    let cx = (g.x + g.w / 2.0) as i32;
                    let cy = (line_y - font_px * 0.32) as i32;
                    out.push(Deco::Tri { rect: Rect::new(cx - side / 2, cy - side / 2, side, side), open, line: abs_line, idx });
                }
            }
            if let Some(idx) = info.bullet {
                if info.checkbox.is_none() {
                    if let Some(g) = glyph_at(idx) {
                        let d = (font_px * 0.4).round().max(4.0) as i32;
                        let cx = (g.x + g.w / 2.0) as i32;
                        let cy = (line_y - font_px * 0.33) as i32;
                        out.push(Deco::Rect { x: cx - d / 2, y: cy - d / 2, w: d, h: d, r: d / 2, c: WHITE });
                    }
                }
            }
            if let Some((idx, checked)) = info.checkbox {
                if let (Some(g0), Some(g1)) = (glyph_at(idx), glyph_at(idx + 2)) {
                    let side = (font_px * 0.95).round() as i32;
                    let cx = ((g0.x + g1.x + g1.w) / 2.0) as i32;
                    let cy = (line_y - font_px * 0.32) as i32;
                    out.push(Deco::Check { rect: Rect::new(cx - side / 2, cy - side / 2, side, side), checked, line: abs_line, idx });
                }
            }
            if map.is_empty() && matches!(info.block, Block::Table | Block::TableSep) {
                // No buffer principal a tabela é desenhada por células (paint).
            } else if info.block == Block::TableSep {
                if let (Some(&first), Some(&last)) = (last_xs.first(), last_xs.last()) {
                    out.push(Deco::Line { x0: first, y0: top + h / 2, x1: last, y1: top + h / 2, t: thin, c: white(0x66) });
                }
            } else if !info.pipes.is_empty() {
                let xs: Vec<i32> = info.pipes.iter().filter_map(|&p| glyph_at(p).map(|g| (g.x + g.w / 2.0) as i32)).collect();
                if let (Some(&first), Some(&last)) = (xs.first(), xs.last()) {
                    for &x in &xs {
                        out.push(Deco::Line { x0: x, y0: top, x1: x, y1: top + h, t: thin, c: GRID });
                    }
                    let is_table = |i: Option<&md::LineInfo>| matches!(i.map(|l| l.block), Some(Block::Table | Block::TableSep));
                    let prev_is_table = run.line_i > 0 && is_table(lines.get(run.line_i - 1).copied());
                    if !prev_is_table {
                        out.push(Deco::Line { x0: first, y0: top, x1: last, y1: top, t: thin, c: GRID });
                    }
                    let next_is_sep = matches!(lines.get(run.line_i + 1).map(|l| l.block), Some(Block::TableSep));
                    if !next_is_sep {
                        out.push(Deco::Line { x0: first, y0: top + h - thin, x1: last, y1: top + h - thin, t: thin, c: GRID });
                    }
                }
                last_xs = xs;
            }
        }
    }
    out
}

// =====================================================================
// App: objetos Wayland + eventos.
// =====================================================================

struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    activation: Option<ActivationState>,
    ddm: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    psm: Option<PrimarySelectionManagerState>,
    primary_device: Option<PrimarySelectionDevice>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    cursor_icon: CursorIcon,
    copy_source: Option<CopyPasteSource>,
    primary_source: Option<PrimarySelectionSource>,
    clipboard_text: Arc<String>,
    primary_text: Arc<String>,

    /// Dois quadros (troca alternada) e um terceiro só se o compositor
    /// segurar os dois; o pool tem exatamente o tamanho dos quadros em uso.
    buffers: [Option<smithay_client_toolkit::shm::slot::Buffer>; 3],
    pool_frame: usize,
    configured: bool,
    exit: bool,
    frame_pending: bool,
    needs_redraw: bool,
    pending_token: Option<String>,
    modifiers: Modifiers,
    last_serial: u32,
    pointer_pos: (f64, f64),
    pointer_down: bool,
    img_drag: Option<ImgDrag>,
    last_click: (Instant, (f64, f64), u32),
    compose: Option<xkb::compose::State>,
    blink_token: Option<RegistrationToken>,
    blink_until: Instant,
    autosave_token: Option<RegistrationToken>,
    first_draw: bool,
    test_input: String,

    ui: Ui,
}

fn main() {
    START.get_or_init(Instant::now);
    trace("main");

    if activate_existing() {
        trace("instância existente ativada");
        return;
    }
    let listener = bind_socket();

    // Fontes curadas carregam numa thread enquanto conectamos ao Wayland.
    let font_thread = std::thread::spawn(curated_db);

    let conn = Connection::connect_to_env().expect("não foi possível conectar ao Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh: QueueHandle<App> = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue).insert(loop_handle.clone()).expect("wayland source");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let activation = ActivationState::bind(&globals, &qh).ok();
    let ddm = DataDeviceManagerState::bind(&globals, &qh).ok();
    let psm = PrimarySelectionManagerState::bind(&globals, &qh).ok();

    let store = Store::open().expect("não foi possível criar o diretório de notas");
    let state = store.load_state();

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("Fast Notes");
    window.set_app_id(APP_ID);
    window.set_min_size(Some((360, 240)));
    window.commit();
    trace("wayland conectado");

    let curated = font_thread.join().ok().flatten();
    let fonts_full = curated.is_none();
    let font_system = make_font_system(curated.unwrap_or_else(full_db));
    trace(if fonts_full { "fontes do sistema carregadas" } else { "fontes curadas carregadas" });

    let compose = {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_CTYPE"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_else(|_| "C".to_string());
        xkb::compose::Table::new_from_locale(&ctx, std::ffi::OsStr::new(&locale), xkb::compose::COMPILE_NO_FLAGS)
            .ok()
            .map(|t| xkb::compose::State::new(&t, xkb::compose::STATE_NO_FLAGS))
    };

    let pool_frame = (state.width * state.height * 4) as usize;
    let pool = SlotPool::new(pool_frame * 2, &shm).expect("shm pool");

    let mut ui = Ui {
        font_system,
        swash: SwashCache::new(),
        fonts_full,
        fonts_loading: false,
        font_probe: HashSet::new(),
        tabs: Vec::new(),
        active: 0,
        width: state.width,
        height: state.height,
        scale: 1,
        zoom: state.zoom.clamp(ZOOM_MIN, ZOOM_MAX),
        csd: false,
        focused: false,
        cursor_visible: true,
        hover: (-1, -1),
        hits: Hits::default(),
        col_hover: None,
        col_drag: None,
        traced: false,
        prof: Prof::new(),
        labels: Vec::new(),
        glyphs: None,
        images: ImageCache::new(store.base_dir.clone()),
        store,
        list_open: false,
        notes: Vec::new(),
        filtered: Vec::new(),
        query: String::new(),
        list_sel: 0,
        list_top: 0,
        picker: None,
        slash: None,
    };
    // Reabre as abas da sessão anterior; senão a última nota; senão a mais recente.
    let mut opened: Vec<PathBuf> = state.tabs.iter().map(|n| ui.store.notes_dir.join(n)).filter(|p| p.is_file()).collect();
    if opened.is_empty() {
        if let Some(p) = state.last.map(|n| ui.store.notes_dir.join(n)).filter(|p| p.is_file()) {
            opened.push(p);
        } else if let Some(n) = ui.store.list().into_iter().next() {
            opened.push(n.path);
        }
    }
    for p in &opened {
        ui.open_in_tab(Some(p.clone()));
    }
    if ui.tabs.is_empty() {
        ui.open_in_tab(None);
    }
    ui.active = state.active.min(ui.tabs.len() - 1);
    ui.persist_state();
    trace("notas carregadas");

    let mut app = App {
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: loop_handle.clone(),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
        pool,
        window,
        activation,
        ddm,
        data_device: None,
        psm,
        primary_device: None,
        seat: None,
        keyboard: None,
        pointer: None,
        cursor_icon: CursorIcon::Default,
        copy_source: None,
        primary_source: None,
        clipboard_text: Arc::new(String::new()),
        primary_text: Arc::new(String::new()),
        buffers: [None, None, None],
        pool_frame,
        configured: false,
        exit: false,
        frame_pending: false,
        needs_redraw: false,
        pending_token: std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty()),
        modifiers: Modifiers::default(),
        last_serial: 0,
        pointer_pos: (0.0, 0.0),
        pointer_down: false,
        img_drag: None,
        last_click: (Instant::now() - Duration::from_secs(10), (0.0, 0.0), 0),
        compose,
        blink_token: None,
        blink_until: Instant::now(),
        autosave_token: None,
        first_draw: true,
        test_input: String::new(),
        ui,
    };

    if let Some(listener) = listener {
        loop_handle
            .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, listener, app| {
                // SAFETY: só lemos do listener; ele continua vivo no source.
                let listener = unsafe { listener.get_mut() };
                while let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut line = String::new();
                    let _ = stream.read_to_string(&mut line);
                    let token = line.trim().strip_prefix("activate").map(str::trim).unwrap_or("");
                    app.activate(token);
                }
                Ok(PostAction::Continue)
            })
            .expect("socket source");
    }

    // Banco completo de fontes só se alguma nota aberta precisar dele.
    app.ensure_fonts_for_tabs();

    // Notas mudadas fora do app (bot do Telegram, outro editor) → recarrega a aba.
    if let Some(f) = watch_notes(&app.ui.store.notes_dir) {
        let _ = loop_handle.insert_source(Generic::new(f, Interest::READ, Mode::Level), |_, f, app| {
            // SAFETY: só lemos do fd do inotify; ele continua vivo no source.
            let names = read_inotify(unsafe { f.get_mut() });
            if !names.is_empty() {
                app.on_disk_change(&names);
            }
            Ok(PostAction::Continue)
        });
    }

    // `FASTNOTES_TEST_FIFO=caminho` lê comandos de teste (eventos sintéticos) de um FIFO.
    if let Some(path) = std::env::var_os("FASTNOTES_TEST_FIFO") {
        if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            let _ = loop_handle.insert_source(Generic::new(f, Interest::READ, Mode::Level), |_, f, app| {
                // SAFETY: só lemos do FIFO; ele continua vivo no source.
                let f = unsafe { f.get_mut() };
                let mut buf = [0u8; 4096];
                let n = f.read(&mut buf).unwrap_or(0);
                app.test_input.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(i) = app.test_input.find('\n') {
                    let line = app.test_input[..i].to_string();
                    app.test_input.drain(..=i);
                    app.test_command(&line);
                }
                Ok(PostAction::Continue)
            });
        }
    }

    loop {
        if let Err(e) = event_loop.dispatch(None, &mut app) {
            eprintln!("erro no loop de eventos: {e}");
            break;
        }
        if app.exit {
            break;
        }
    }
    app.ui.save_all();
    app.ui.persist_state();
}

impl App {
    /// Comandos do gancho de teste: `mods ctrl,shift` · `key <keysym>` · `text <utf8>` ·
    /// `move x y` · `press|release left|middle` · `scroll <px>`.
    fn test_command(&mut self, line: &str) {
        let t0 = Instant::now();
        self.test_command_inner(line);
        prof_add(&mut self.ui.prof, P_INPUT, t0);
    }

    fn test_command_inner(&mut self, line: &str) {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        let btn = |a: &str| if a == "middle" { BTN_MIDDLE } else { BTN_LEFT };
        match cmd {
            "mods" => {
                let mut m = Modifiers::default();
                for part in arg.split(',') {
                    match part.trim() {
                        "ctrl" => m.ctrl = true,
                        "shift" => m.shift = true,
                        "alt" => m.alt = true,
                        _ => {}
                    }
                }
                self.modifiers = m;
            }
            "key" => {
                let keysym = xkb::keysym_from_name(arg.trim(), xkb::KEYSYM_NO_FLAGS);
                let utf8 = xkb::keysym_to_utf8(keysym);
                let utf8 = utf8.trim_end_matches('\0').to_string();
                let raw_code = match arg.trim() {
                    "0" => 11,
                    d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => (d.as_bytes()[0] - b'0' + 1) as u32,
                    _ => 0,
                };
                let ev = KeyEvent { time: 0, raw_code, keysym, utf8: (!utf8.is_empty()).then_some(utf8) };
                self.on_key(&ev);
            }
            "text" => {
                for c in arg.chars() {
                    let ev = KeyEvent { time: 0, raw_code: 0, keysym: Keysym::from_char(c), utf8: Some(c.to_string()) };
                    self.on_key(&ev);
                }
            }
            "move" => {
                let mut it = arg.split_whitespace().filter_map(|v| v.parse::<f64>().ok());
                if let (Some(x), Some(y)) = (it.next(), it.next()) {
                    self.pointer_pos = (x, y);
                    self.on_pointer_motion();
                }
            }
            "press" => self.on_pointer_press(btn(arg.trim()), self.last_serial),
            "release" => self.on_pointer_release(btn(arg.trim())),
            "scroll" => self.on_scroll(arg.trim().parse().unwrap_or(0.0)),
            "resize" => {
                if let Some((w, h)) = arg.split_once(' ') {
                    self.ui.width = w.trim().parse().unwrap_or(self.ui.width);
                    self.ui.height = h.trim().parse().unwrap_or(self.ui.height);
                    self.request_redraw();
                }
            }
            _ => {}
        }
        self.request_redraw();
    }

    fn activate(&mut self, token: &str) {
        if !token.is_empty() {
            if let Some(a) = &self.activation {
                a.activate::<App>(self.window.wl_surface(), token.to_string());
            }
        }
        self.request_redraw();
    }

    /// Carrega o banco completo de fontes em 2º plano se `text` tiver algum
    /// caractere fora das fontes curadas.
    fn ensure_fonts(&mut self, text: &str) {
        if !self.ui.text_needs_full_fonts(text) {
            return;
        }
        self.ui.fonts_loading = true;
        trace("caractere fora das fontes curadas: carregando fontes do sistema");
        let (tx, rx) = channel::channel::<fontdb::Database>();
        std::thread::spawn(move || {
            let _ = tx.send(full_db());
        });
        let _ = self.loop_handle.insert_source(rx, |ev, _, app| {
            if let channel::Event::Msg(db) = ev {
                app.swap_fonts(db);
            }
        });
    }

    fn ensure_fonts_for_tabs(&mut self) {
        if self.ui.fonts_full || self.ui.fonts_loading {
            return;
        }
        let texts: Vec<String> = self.ui.tabs.iter().map(|t| t.text()).collect();
        for t in &texts {
            self.ensure_fonts(t);
        }
    }

    fn swap_fonts(&mut self, db: fontdb::Database) {
        self.ui.font_system = make_font_system(db);
        self.ui.swash = SwashCache::new();
        self.ui.labels.clear();
        self.ui.fonts_full = true;
        self.ui.fonts_loading = false;
        for t in &mut self.ui.tabs {
            t.editor.with_buffer_mut(|b| {
                for l in &mut b.lines {
                    l.reset();
                }
                b.set_redraw(true);
            });
        }
        trace("fontes do sistema carregadas (2º plano)");
        self.request_redraw();
    }

    // ---------- redesenho ----------

    fn request_redraw(&mut self) {
        if !self.configured {
            return;
        }
        if self.frame_pending {
            self.needs_redraw = true;
        } else {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let scale = self.ui.scale.max(1);
        let width = (self.ui.width as i32 * scale).max(1);
        let height = (self.ui.height as i32 * scale).max(1);
        let stride = width * 4;
        let frame_len = (height * stride) as usize;
        let App { pool, pool_frame, buffers, shm, ui, window, qh, needs_redraw, compositor, .. } = self;

        // O pool do wl_shm só cresce (a biblioteca dobra o tamanho a cada
        // alocação que não cabe). Para a memória ficar em exatamente dois
        // quadros, o pool é recriado quando o tamanho da janela muda.
        if *pool_frame != frame_len {
            *buffers = [None, None, None];
            *pool = SlotPool::new(frame_len * 2, shm).expect("shm pool");
            *pool_frame = frame_len;
            // Janela opaca: o compositor não precisa misturar (blend) nada.
            if let Ok(region) = Region::new(compositor) {
                region.add(0, 0, ui.width as i32, ui.height as i32);
                window.wl_surface().set_opaque_region(Some(region.wl_region()));
            }
        }
        let mut chosen = None;
        for (i, slot) in buffers.iter().enumerate().take(2) {
            let free = match slot {
                None => true,
                Some(b) => pool.canvas(b).is_some(),
            };
            if free {
                chosen = Some(i);
                break;
            }
        }
        // O compositor pode segurar dois quadros ao mesmo tempo; nesse caso
        // (raro) o pool cresce para exatamente três.
        let i = match chosen {
            Some(i) => i,
            None => {
                if buffers[2].as_ref().is_some_and(|b| pool.canvas(b).is_none()) {
                    // Três quadros presos: o frame callback pendente redesenha.
                    *needs_redraw = true;
                    return;
                }
                let _ = pool.resize(frame_len * 3);
                2
            }
        };
        let buf = buffers[i].get_or_insert_with(|| {
            pool.create_buffer(width, height, stride, wl_shm::Format::Xrgb8888).expect("create buffer").0
        });
        let canvas_bytes = pool.canvas(buf).expect("quadro livre");
        let mut canvas = Canvas::new(canvas_bytes, width, height);
        let t0 = Instant::now();
        ui.paint(&mut canvas);
        prof_add(&mut ui.prof, P_FRAME, t0);
        let t0 = Instant::now();
        // `FASTNOTES_SNAPSHOT=arquivo.ppm` grava o último quadro (para testes).
        if let Some(path) = SNAPSHOT.get_or_init(|| std::env::var_os("FASTNOTES_SNAPSHOT")) {
            let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
            for px in canvas_bytes.chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0]]);
            }
            let _ = std::fs::write(path, out);
        }
        prof_add(&mut ui.prof, P_SNAPSHOT, t0);
        if let Some(p) = &mut ui.prof {
            p.frame_done();
        }

        let surface = window.wl_surface();
        surface.damage_buffer(0, 0, width, height);
        surface.frame(qh, FrameCallbackData(surface.clone()));
        buf.attach_to(surface).expect("attach");
        window.commit();
        self.frame_pending = true;
        self.needs_redraw = false;
        if self.first_draw {
            self.first_draw = false;
            trace("primeiro quadro enviado");
        }
    }

    // ---------- timers ----------

    fn wake_cursor(&mut self) {
        self.ui.cursor_visible = true;
        self.blink_until = Instant::now() + Duration::from_secs(BLINK_FOR_SECS);
        if self.blink_token.is_none() {
            let token = self
                .loop_handle
                .insert_source(Timer::from_duration(Duration::from_millis(BLINK_MS)), |_, _, app| {
                    if !app.ui.focused || Instant::now() > app.blink_until || app.ui.list_open {
                        app.ui.cursor_visible = true;
                        app.blink_token = None;
                        app.request_redraw();
                        return TimeoutAction::Drop;
                    }
                    app.ui.cursor_visible = !app.ui.cursor_visible;
                    app.request_redraw();
                    TimeoutAction::ToDuration(Duration::from_millis(BLINK_MS))
                })
                .ok();
            self.blink_token = token;
        }
    }

    fn schedule_autosave(&mut self) {
        if let Some(t) = self.autosave_token.take() {
            self.loop_handle.remove(t);
        }
        self.autosave_token = self
            .loop_handle
            .insert_source(Timer::from_duration(Duration::from_millis(AUTOSAVE_MS)), |_, _, app| {
                app.autosave_token = None;
                app.ui.save_all();
                app.request_redraw();
                TimeoutAction::Drop
            })
            .ok();
    }

    fn flush(&mut self) {
        if let Some(t) = self.autosave_token.take() {
            self.loop_handle.remove(t);
        }
        self.ui.save_all();
    }

    // ---------- edição ----------

    fn edit(&mut self, action: Action, boundary: bool) {
        let (fs, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.action(fs, action);
        let change = tab.editor.finish_change();
        self.after_change(change, boundary);
    }

    fn after_change(&mut self, change: Option<Change>, boundary: bool) {
        let mut changed = false;
        if let Some(ch) = change {
            if !ch.items.is_empty() {
                self.ui.tab_mut().undo.record(ch, boundary);
                changed = true;
            }
        }
        if changed {
            let t0 = Instant::now();
            self.ui.restyle_active();
            let line = self.ui.tab().editor.cursor().line;
            if self.ui.format_tables(Some(line)) {
                self.ui.restyle_active();
            }
            prof_add(&mut self.ui.prof, P_RESTYLE, t0);
            let t0 = Instant::now();
            self.ui.tab_mut().last_line = line;
            self.ui.tab_mut().on_text_changed();
            self.schedule_autosave();
            prof_add(&mut self.ui.prof, P_DERIVED, t0);
        }
        self.wake_cursor();
        self.request_redraw();
    }

    /// Reformata o que ficou pendente depois de cada ação: assim a próxima
    /// ação (mesmo antes de um redesenho) vê layout e rolagem atualizados.
    fn settle(&mut self) {
        let t0 = Instant::now();
        let (fs, tab) = self.ui.ed();
        tab.editor.shape_as_needed(fs, true);
        prof_add(&mut self.ui.prof, P_SETTLE, t0);
    }

    /// Ao mudar de linha: alinha a tabela que o cursor acabou de deixar e
    /// reaplica os estilos (marcadores só aparecem na linha em edição).
    fn after_input(&mut self) {
        let line = self.ui.tab().editor.cursor().line;
        if line != self.ui.tab().last_line {
            self.ui.tab_mut().last_line = line;
            if self.ui.format_tables(Some(line)) {
                self.ui.tab_mut().on_text_changed();
                self.schedule_autosave();
            }
            self.ui.restyle_active();
            self.request_redraw();
        }
        self.settle();
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.ensure_fonts(text);
        self.leave_structural();
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let boundary = text.chars().last().is_some_and(char::is_whitespace) || text.chars().count() > 1;
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.insert_string(&text, None);
        let change = tab.editor.finish_change();
        self.after_change(change, boundary);
    }

    fn motion(&mut self, m: Motion, shift: bool) {
        let (_, tab) = self.ui.ed();
        if shift {
            if tab.editor.selection() == Selection::None {
                tab.editor.set_selection(Selection::Normal(tab.editor.cursor()));
            }
        } else {
            tab.editor.set_selection(Selection::None);
        }
        let before = tab.editor.cursor();
        if matches!(m, Motion::Left | Motion::Right | Motion::LeftWord | Motion::RightWord)
            && self.column_grid_move(matches!(m, Motion::Right | Motion::RightWord))
        {
            if shift {
                self.update_primary();
            }
            self.wake_cursor();
            self.request_redraw();
            return;
        }
        let (fs, tab) = self.ui.ed();
        // Home alterna entre o primeiro caractere não-branco e a coluna 0.
        let m = if m == Motion::Home {
            let text = tab.line_text(before.line);
            let first = text.len() - text.trim_start().len();
            if before.index == first && first > 0 { Motion::Home } else { Motion::SoftHome }
        } else {
            m
        };
        tab.editor.action(fs, Action::Motion(m));
        // ↑ na primeira linha vai ao início do texto; ↓ na última, ao fim.
        if tab.editor.cursor() == before {
            match m {
                Motion::Up => tab.editor.set_cursor(Cursor::new(0, 0)),
                Motion::Down => {
                    let end = tab.end_cursor();
                    tab.editor.set_cursor(end);
                }
                _ => {}
            }
        }
        let horizontal = !matches!(m, Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown | Motion::Vertical(_));
        let forward = match m {
            Motion::BufferStart => true,
            Motion::BufferEnd => false,
            _ => tab.editor.cursor() > before,
        };
        self.settle_cursor(before, forward, horizontal);
        if shift {
            self.update_primary();
        }
        self.wake_cursor();
        self.request_redraw();
    }

    /// Depois de mover o cursor: se ele parou numa linha estrutural (`:::`,
    /// `|||`, separador de tabela) ou dobrada, leva-o à linha editável vizinha.
    /// Num bloco de colunas as setas andam em grade: → no fim de uma coluna vai
    /// à próxima, ← no início volta à anterior, ↑/↓ saem do bloco por cima ou
    /// por baixo; vindo de fora, ↓/→ entram na primeira coluna. `before` é a
    /// posição anterior ao movimento (restaurada se não houver para onde ir).
    fn settle_cursor(&mut self, before: Cursor, forward: bool, horizontal: bool) {
        for _ in 0..64 {
            let tab = self.ui.tab();
            let c = tab.editor.cursor();
            if !tab.lines.get(c.line).is_some_and(|l| l.skip_cursor()) {
                return;
            }
            let n = tab.lines.len();
            let after = |l: usize| (l + 1 < n).then(|| Cursor::new(l + 1, 0));
            let above = |l: usize| (l > 0).then(|| Cursor::new(l - 1, tab.line_text(l - 1).len()));
            let mut heal: Option<(usize, usize)> = None;
            let target = match tab.col_block_of(c.line) {
                Some(bi) => {
                    let b = &tab.columns[bi];
                    let entry = |ci: usize, at_start: bool| -> Option<Cursor> {
                        let col = &b.cols[ci];
                        if col.map.is_empty() {
                            return None;
                        }
                        Some(if at_start { Cursor::new(col.first, 0) } else { Cursor::new(col.last, tab.line_text(col.last).len()) })
                    };
                    let from = tab.column_of(before.line).filter(|&(fb, _)| fb == bi).map(|(_, ci)| ci);
                    let last = b.cols.len() - 1;
                    let (ci, at_start, exit) = match (from, forward, horizontal) {
                        // De dentro do bloco só se chega aqui por ↑/↓ (← e → andam
                        // em grade em `column_grid_move`): sai por baixo ou por cima.
                        (Some(_), true, _) => (0, true, Some(after(b.end))),
                        (Some(_), false, _) => (0, true, Some(above(b.start))),
                        (None, true, _) => (0, true, None),
                        (None, false, true) => (last, false, None),
                        (None, false, false) => (0, false, None),
                    };
                    match exit {
                        Some(t) => t,
                        None => match entry(ci, at_start) {
                            Some(t) => Some(t),
                            None => {
                                heal = Some((bi, ci));
                                None
                            }
                        },
                    }
                }
                None => if forward { after(c.line) } else { above(c.line) },
            };
            if let Some((bi, ci)) = heal {
                self.create_col_line(bi, ci);
                return;
            }
            let target = target.or_else(|| if forward { above(c.line) } else { after(c.line) });
            let (_, tab) = self.ui.ed();
            match target {
                Some(t) if t != c => tab.editor.set_cursor(t),
                _ => {
                    tab.editor.set_cursor(before);
                    return;
                }
            }
        }
    }

    /// Cria uma linha vazia no fim da coluna `ci` (ou logo após o seu separador,
    /// se ela não tiver nenhuma) e põe o cursor nela.
    fn create_col_line(&mut self, bi: usize, ci: usize) {
        let tab = self.ui.tab();
        let Some(b) = tab.columns.get(bi) else { return };
        let at = match b.cols.get(ci).filter(|c| !c.map.is_empty()) {
            Some(c) => Cursor::new(c.last, tab.line_text(c.last).len()),
            None => match tab.col_sep_line(bi, ci) {
                Some(sep) => Cursor::new(sep, tab.line_text(sep).len()),
                None => return,
            },
        };
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.insert_at(at, "\n", None);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(Cursor::new(at.line + 1, 0));
        self.after_change(change, true);
    }

    /// ← / → nas bordas de uma célula de colunas andam em grade, como numa
    /// tabela: → no fim da linha `r` da coluna `ci` vai à linha `r` da coluna
    /// seguinte (criando-a se não existir); no fim da última coluna vai à linha
    /// `r+1` da coluna 1. ← faz o inverso; na primeira célula sai do bloco por
    /// cima. Devolve `false` se o cursor não está numa borda de célula.
    fn column_grid_move(&mut self, right: bool) -> bool {
        let tab = self.ui.tab();
        let c = tab.editor.cursor();
        let Some((bi, ci)) = tab.column_of(c.line) else { return false };
        let len = tab.line_text(c.line).len();
        if (right && c.index < len) || (!right && c.index > 0) {
            return false;
        }
        let b = &tab.columns[bi];
        let last = b.cols.len() - 1;
        let r = b.cols[ci].map.iter().position(|&m| m == c.line).unwrap_or(0);
        let end_of = |l: usize| Cursor::new(l, tab.line_text(l).len());
        let target: Result<Cursor, usize> = if right {
            let (tci, tr) = if ci < last { (ci + 1, r) } else { (0, r + 1) };
            b.cols[tci].map.get(tr).map(|&l| Cursor::new(l, 0)).ok_or(tci)
        } else if ci > 0 {
            let m = &b.cols[ci - 1].map;
            m.get(r).or(m.last()).map(|&l| end_of(l)).ok_or(ci - 1)
        } else if r > 0 {
            let m = &b.cols[last].map;
            m.get(r - 1).or(m.last()).map(|&l| end_of(l)).ok_or(last)
        } else {
            match (b.start > 0).then(|| end_of(b.start - 1)) {
                Some(t) => Ok(t),
                None => return true,
            }
        };
        match target {
            Ok(t) => {
                let (_, tab) = self.ui.ed();
                tab.editor.set_cursor(t);
                self.settle_cursor(c, !right, true);
            }
            Err(tci) => self.create_col_line(bi, tci),
        }
        true
    }

    /// Antes de editar: um cursor parado numa linha estrutural vai para a
    /// próxima linha editável, para nunca corromper `:::`, `|||` ou `|---|`.
    fn leave_structural(&mut self) {
        let tab = self.ui.tab();
        let c = tab.editor.cursor();
        if tab.lines.get(c.line).is_some_and(|l| l.skip_cursor()) {
            self.settle_cursor(c, true, false);
        }
    }

    fn delete_word(&mut self, backward: bool) {
        let (fs, tab) = self.ui.ed();
        if tab.editor.selection() == Selection::None {
            tab.editor.set_selection(Selection::Normal(tab.editor.cursor()));
            let m = if backward { Motion::PreviousWord } else { Motion::NextWord };
            tab.editor.action(fs, Action::Motion(m));
        }
        tab.editor.start_change();
        tab.editor.delete_selection();
        let change = tab.editor.finish_change();
        self.after_change(change, true);
    }

    fn select_all(&mut self) {
        let tab = self.ui.tab_mut();
        let end = tab.end_cursor();
        tab.editor.set_selection(Selection::Normal(Cursor::new(0, 0)));
        tab.editor.set_cursor(end);
        self.update_primary();
        self.request_redraw();
    }

    fn undo(&mut self) {
        let change = self.ui.tab_mut().undo.undo();
        self.apply(change);
    }

    fn redo(&mut self) {
        let change = self.ui.tab_mut().undo.redo();
        self.apply(change);
    }

    fn apply(&mut self, change: Option<Change>) {
        let Some(change) = change else { return };
        {
            let tab = self.ui.tab_mut();
            tab.editor.set_selection(Selection::None);
            tab.editor.apply_change(&change);
        }
        self.ui.restyle_active();
        let tab = self.ui.tab_mut();
        tab.last_line = tab.editor.cursor().line;
        tab.on_text_changed();
        self.schedule_autosave();
        self.wake_cursor();
        self.request_redraw();
        self.settle();
    }

    /// Enter: continua listas, encerra item vazio, adiciona linha em tabela.
    /// Shift+Enter: quebra curta. Põe `\` no fim do trecho (quebra dura do
    /// CommonMark) e faz a quebra normal (continua listas e toggles): a linha
    /// seguinte fica 0.25 em mais perto que numa quebra comum. Vale para
    /// texto, listas, títulos, citações e `---`; em tabela vira nova linha.
    fn soft_enter(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let block = tab.lines.get(cur.line).map(|l| l.block);
        if matches!(block, Some(Block::Table | Block::TableSep)) {
            self.smart_enter();
            return;
        }
        if matches!(block, Some(Block::ColStart | Block::ColSep | Block::ColEnd | Block::Fence | Block::Code | Block::Image | Block::Space)) {
            self.edit(Action::Enter, true);
            return;
        }
        let before = tab.line_text(cur.line);
        let head = &before[..cur.index.min(before.len())];
        if !head.trim_end().ends_with('\\') {
            self.insert_text("\\");
        }
        self.smart_enter();
    }

    fn is_space_line(&self, i: usize) -> bool {
        self.ui.tab().lines.get(i).is_some_and(|l| l.block == Block::Space)
    }

    /// Apaga as linhas `a..=b` inteiras; o cursor fica no início do que as seguia.
    fn delete_lines(&mut self, a: usize, b: usize) {
        let n = self.ui.tab().lines.len();
        let (start, end, at) = if b + 1 < n {
            (Cursor::new(a, 0), Cursor::new(b + 1, 0), Cursor::new(a, 0))
        } else if a > 0 {
            let prev_len = self.ui.tab().line_text(a - 1).len();
            (Cursor::new(a - 1, prev_len), Cursor::new(b, self.ui.tab().line_text(b).len()), Cursor::new(a - 1, prev_len))
        } else {
            (Cursor::new(0, 0), Cursor::new(b, self.ui.tab().line_text(b).len()), Cursor::new(0, 0))
        };
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.delete_range(start, end);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(at);
        self.after_change(change, true);
    }

    /// Põe o cursor em `to` e, se for linha estrutural, acomoda-o como uma seta faria.
    fn settle_after_move(&mut self, to: Cursor, before: Cursor, forward: bool, horizontal: bool) {
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.set_cursor(to);
        self.settle_cursor(before, forward, horizontal);
        self.wake_cursor();
        self.request_redraw();
    }

    /// Apaga a linha `i` inteira (com a quebra que a segue, ou a anterior se for a última).
    fn delete_whole_line(&mut self, i: usize) {
        let n = self.ui.tab().lines.len();
        let (start, end) = if i + 1 < n {
            (Cursor::new(i, 0), Cursor::new(i + 1, 0))
        } else if i > 0 {
            let prev_len = self.ui.tab().line_text(i - 1).len();
            (Cursor::new(i - 1, prev_len), Cursor::new(i, self.ui.tab().line_text(i).len()))
        } else {
            (Cursor::new(0, 0), Cursor::new(0, self.ui.tab().line_text(0).len()))
        };
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.delete_range(start, end);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(start);
        self.after_change(change, true);
    }

    /// Backspace: um meio espaço some inteiro (na linha dele ou no início da linha seguinte).
    fn smart_backspace(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        if tab.editor.selection() == Selection::None {
            if self.is_space_line(cur.line) {
                self.delete_whole_line(cur.line);
                return;
            }
            if cur.index == 0 && cur.line > 0 && self.is_space_line(cur.line - 1) {
                self.delete_whole_line(cur.line - 1);
                let (_, tab) = self.ui.ed();
                tab.editor.set_cursor(Cursor::new(cur.line - 1, 0));
                return;
            }
            // No início de uma linha após `:::`/`|||`/`|---|`: nunca funde com a
            // linha estrutural. Bloco de colunas vazio some inteiro; senão o
            // cursor só anda para a célula anterior.
            if cur.index == 0 && cur.line > 0 && tab.lines.get(cur.line - 1).is_some_and(|l| l.skip_cursor() && !l.folded) {
                if let Some(bi) = tab.col_block_of(cur.line - 1).filter(|&bi| tab.col_block_empty(bi)) {
                    let (start, end) = (tab.columns[bi].start, tab.columns[bi].end);
                    self.delete_lines(start, end);
                    return;
                }
                if tab.column_of(cur.line).is_some() {
                    self.column_grid_move(false);
                } else {
                    self.settle_after_move(Cursor::new(cur.line - 1, 0), cur, false, true);
                }
                self.wake_cursor();
                self.request_redraw();
                return;
            }
        }
        self.edit(Action::Backspace, false);
    }

    /// Delete: no fim de uma linha seguida de meio espaço, apaga o meio espaço.
    fn smart_delete(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        if tab.editor.selection() == Selection::None {
            if self.is_space_line(cur.line) {
                self.delete_whole_line(cur.line);
                return;
            }
            let len = tab.line_text(cur.line).len();
            if cur.index >= len && self.is_space_line(cur.line + 1) {
                self.delete_whole_line(cur.line + 1);
                let (_, tab) = self.ui.ed();
                tab.editor.set_cursor(cur);
                return;
            }
            // No fim de uma linha antes de `|||`/`:::`/`|---|`: não funde.
            if cur.index >= len && tab.lines.get(cur.line + 1).is_some_and(|l| l.skip_cursor() && !l.folded) {
                return;
            }
        }
        self.edit(Action::Delete, false);
    }

    /// Ctrl+Shift+N: cópia da nota atual numa aba nova (vira um arquivo novo ao salvar).
    fn duplicate_note(&mut self) {
        let text = self.ui.tab().text();
        if text.trim().is_empty() {
            return;
        }
        self.flush();
        let i = self.ui.new_tab();
        self.ui.active = i;
        let fp = self.ui.font_px();
        let Ui { font_system, images, tabs, .. } = &mut self.ui;
        tabs[i].load(&text, font_system, images, fp);
        tabs[i].saved_text.clear();
        tabs[i].status = "cópia (não salva)".to_string();
        tabs[i].on_text_changed();
        self.schedule_autosave();
        self.ui.persist_state();
        self.wake_cursor();
        self.request_redraw();
    }

    // ---------- seletor de emoji / símbolos ----------

    fn open_glyphs(&mut self, kind: GlyphKind) {
        self.ui.slash = None;
        self.ui.glyphs = Some(Glyphs::new(kind));
        self.request_redraw();
    }

    fn close_glyphs(&mut self) {
        self.ui.glyphs = None;
        self.wake_cursor();
        self.request_redraw();
    }

    fn glyph_insert(&mut self, fi: usize) {
        let text = self.ui.glyphs.as_ref().and_then(|g| g.filtered.get(fi).map(|&i| glyphs(g.kind)[i].text));
        if let Some(t) = text {
            self.insert_text(t);
        }
        self.close_glyphs();
        self.after_input();
    }

    fn glyph_key(&mut self, sym: Keysym, ch: Option<char>, m: &Modifiers) {
        let Some(g) = self.ui.glyphs.as_mut() else { return };
        match sym {
            Keysym::Escape => {
                self.close_glyphs();
                return;
            }
            Keysym::Return | Keysym::KP_Enter => {
                let sel = g.sel;
                self.glyph_insert(sel);
                return;
            }
            Keysym::Left => g.move_sel(-1),
            Keysym::Right | Keysym::Tab => g.move_sel(1),
            Keysym::Up => g.move_sel(-(GLYPH_COLS as i32)),
            Keysym::Down => g.move_sel(GLYPH_COLS as i32),
            Keysym::Page_Up => g.move_sel(-((GLYPH_COLS * GLYPH_ROWS) as i32)),
            Keysym::Page_Down => g.move_sel((GLYPH_COLS * GLYPH_ROWS) as i32),
            Keysym::Home => g.move_sel(-(g.sel as i32)),
            Keysym::End => g.move_sel(g.filtered.len() as i32),
            Keysym::BackSpace => {
                if m.ctrl {
                    g.query.clear();
                } else {
                    g.query.pop();
                }
                g.refilter();
            }
            _ => {
                if let Some(c) = ch.filter(|c| !c.is_control() && !m.ctrl && !m.alt) {
                    g.query.push(c);
                    g.refilter();
                } else if m.ctrl && (ch == Some('.') || ch == Some(',') || sym == Keysym::period || sym == Keysym::comma) {
                    self.close_glyphs();
                    return;
                }
            }
        }
        self.request_redraw();
    }

    fn smart_enter(&mut self) {
        self.leave_structural();
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let line = tab.line_text(cur.line);
        let block = tab.lines.get(cur.line).map(|l| l.block);
        if matches!(block, Some(Block::Table | Block::TableSep)) {
            // Enter numa última linha vazia da tabela sai da tabela (como lista vazia).
            let is_last = !tab.lines.get(cur.line + 1).is_some_and(|l| matches!(l.block, Block::Table | Block::TableSep));
            let empty_row = line.trim().trim_matches('|').split('|').all(|c| c.trim().is_empty());
            if is_last && empty_row && block == Some(Block::Table) {
                let (_, tab) = self.ui.ed();
                tab.editor.set_selection(Selection::None);
                tab.editor.start_change();
                tab.editor.delete_range(Cursor::new(cur.line, 0), Cursor::new(cur.line, line.len()));
                let change = tab.editor.finish_change();
                tab.editor.set_cursor(Cursor::new(cur.line, 0));
                self.after_change(change, true);
                return;
            }
            let cols = md::table_cols(&line);
            let row = format!("\n|{}", "  |".repeat(cols));
            // No cabeçalho, a nova linha entra depois do separador.
            let tl = cur.line + usize::from(tab.lines.get(cur.line + 1).is_some_and(|l| l.block == Block::TableSep));
            let tl_len = tab.line_text(tl).len();
            let (_, tab) = self.ui.ed();
            tab.editor.set_selection(Selection::None);
            tab.editor.set_cursor(Cursor::new(tl, tl_len));
            tab.editor.start_change();
            tab.editor.insert_string(&row, None);
            let change = tab.editor.finish_change();
            tab.editor.set_cursor(Cursor::new(tl + 1, 2));
            self.after_change(change, true);
            return;
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        if let Some(Block::Toggle(open)) = block {
            if cur.index == line.len() {
                if open {
                    let (_, tab) = self.ui.ed();
                    tab.editor.start_change();
                    tab.editor.insert_string(&format!("\n{indent}  "), None);
                    let change = tab.editor.finish_change();
                    self.after_change(change, true);
                } else {
                    // Fechado: a nova linha vai depois do conteúdo dobrado.
                    let mut last = cur.line;
                    while tab.lines.get(last + 1).is_some_and(|l| l.folded) {
                        last += 1;
                    }
                    let end = tab.line_text(last).len();
                    let (_, tab) = self.ui.ed();
                    tab.editor.set_cursor(Cursor::new(last, end));
                    tab.editor.start_change();
                    tab.editor.insert_string(&format!("\n{indent}"), None);
                    let change = tab.editor.finish_change();
                    self.after_change(change, true);
                }
                return;
            }
        }
        if !matches!(block, Some(Block::Code | Block::Fence)) && cur.index == line.len() {
            if let Some((prefix, empty)) = md::list_continuation(&line) {
                let (_, tab) = self.ui.ed();
                tab.editor.start_change();
                if empty {
                    tab.editor.delete_range(Cursor::new(cur.line, 0), Cursor::new(cur.line, line.len()));
                    tab.editor.set_cursor(Cursor::new(cur.line, 0));
                } else {
                    tab.editor.insert_string(&format!("\n{prefix}"), None);
                }
                let change = tab.editor.finish_change();
                self.after_change(change, true);
                return;
            }
        }
        // Mantém o recuo da linha atual (sub-itens, conteúdo de toggle).
        if !indent.is_empty() && cur.index >= indent.len() && !matches!(block, Some(Block::Code)) {
            let (_, tab) = self.ui.ed();
            tab.editor.start_change();
            tab.editor.insert_string(&format!("\n{indent}"), None);
            let change = tab.editor.finish_change();
            self.after_change(change, true);
            return;
        }
        self.edit(Action::Enter, true);
    }

    /// Abre/fecha um toggle trocando `▾`/`▸`.
    fn toggle_fold(&mut self, line: usize, idx: usize) {
        let tab = self.ui.tab();
        let text = tab.line_text(line);
        let Some(cur_ch) = text.get(idx..).and_then(|t| t.chars().next()) else { return };
        let new = if cur_ch.to_string() == md::TOGGLE_OPEN { md::TOGGLE_CLOSED } else { md::TOGGLE_OPEN };
        let cur = tab.editor.cursor();
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.delete_range(Cursor::new(line, idx), Cursor::new(line, idx + cur_ch.len_utf8()));
        tab.editor.insert_at(Cursor::new(line, idx), new, None);
        let change = tab.editor.finish_change();
        // Cursor dentro do conteúdo que vai dobrar volta para a linha do toggle.
        tab.editor.set_cursor(if cur.line > line { Cursor::new(line, text.len()) } else { cur });
        self.after_change(change, true);
    }

    /// Insere `text` no cursor e coloca o cursor em (linha + dl, índice).
    fn insert_block(&mut self, text: &str, dl: usize, idx: usize) {
        let tab = self.ui.tab();
        let mut cur = tab.editor.cursor();
        let line = tab.line_text(cur.line);
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        if line.trim().is_empty() && !line.is_empty() {
            tab.editor.delete_range(Cursor::new(cur.line, 0), Cursor::new(cur.line, line.len()));
            cur = Cursor::new(cur.line, 0);
            tab.editor.set_cursor(cur);
        }
        tab.editor.insert_string(text, None);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(Cursor::new(cur.line + dl, idx));
        self.after_change(change, true);
    }

    /// Menu `/`: aplica o item escolhido no lugar de `/consulta`.
    fn slash_apply(&mut self) {
        let matches = self.ui.slash_matches();
        let Some(sl) = self.ui.slash.take() else { return };
        let Some(&mi) = matches.get(sl.sel.min(matches.len().saturating_sub(1))) else {
            self.request_redraw();
            return;
        };
        let key = SLASH_ITEMS[mi].1;
        let cur = self.ui.tab().editor.cursor();
        {
            let (_, tab) = self.ui.ed();
            tab.editor.set_selection(Selection::None);
            tab.editor.start_change();
            tab.editor.delete_range(Cursor::new(sl.line, sl.start), Cursor::new(sl.line, cur.index.max(sl.start)));
            let change = tab.editor.finish_change();
            tab.editor.set_cursor(Cursor::new(sl.line, sl.start));
            self.after_change(change, true);
        }
        match key {
            "h1" => self.set_prefix(Some(md::Prefix::Heading(1))),
            "h2" => self.set_prefix(Some(md::Prefix::Heading(2))),
            "h3" => self.set_prefix(Some(md::Prefix::Heading(3))),
            "p" => self.set_prefix(None),
            "ul" => self.set_prefix(Some(md::Prefix::Bullet)),
            "ol" => self.set_prefix(Some(md::Prefix::Numbered)),
            "todo" => self.set_prefix(Some(md::Prefix::Task)),
            "toggle" => self.set_prefix(Some(md::Prefix::Toggle)),
            "quote" => self.set_prefix(Some(md::Prefix::Quote)),
            "hr" => self.insert_block("---\n", 1, 0),
            "table" => self.insert_table(),
            "col2" | "col3" | "col4" => {
                let n = key.as_bytes()[3] - b'0';
                let mut t = String::from(":::\n");
                for i in 0..n {
                    if i > 0 {
                        t.push_str("|||\n");
                    }
                    t.push_str("\n");
                }
                t.push_str(":::\n");
                self.insert_block(&t, 1, 0);
            }
            "image" => {
                self.insert_block("![]()", 0, 4);
            }
            "code" => self.insert_block("```\n\n```", 1, 0),
            _ => {}
        }
    }

    /// Ctrl+Enter num toggle abre/fecha; senão alterna checkbox.

    /// Tab dentro de tabela: pula para a célula seguinte/anterior.
    fn table_tab(&mut self, back: bool) -> bool {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        if !matches!(tab.lines.get(cur.line).map(|l| l.block), Some(Block::Table | Block::TableSep)) {
            return false;
        }
        let line = tab.line_text(cur.line);
        let pipes = &tab.lines[cur.line].pipes;
        let k = pipes.iter().filter(|&&p| p < cur.index).count();
        let cell_start = |p: usize| p + 1 + usize::from(line.as_bytes().get(p + 1) == Some(&b' '));
        let target: Option<Cursor> = if !back {
            if k < pipes.len().saturating_sub(1) {
                Some(Cursor::new(cur.line, cell_start(pipes[k])))
            } else {
                let nl = cur.line + 1 + usize::from(tab.lines.get(cur.line + 1).is_some_and(|l| l.block == Block::TableSep));
                let next = tab.line_text(nl);
                let next_pipes = tab.lines.get(nl).map(|l| l.pipes.clone()).unwrap_or_default();
                if md::is_table_line(&next) && !next_pipes.is_empty() {
                    Some(Cursor::new(nl, next_pipes[0] + 1 + usize::from(next.as_bytes().get(next_pipes[0] + 1) == Some(&b' '))))
                } else {
                    None
                }
            }
        } else if k >= 2 {
            Some(Cursor::new(cur.line, cell_start(pipes[k - 2])))
        } else if cur.line > 0 {
            let pl = cur.line - 1 - usize::from(cur.line >= 2 && tab.lines.get(cur.line - 1).is_some_and(|l| l.block == Block::TableSep));
            let prev_pipes = tab.lines.get(pl).map(|l| l.pipes.clone()).unwrap_or_default();
            if prev_pipes.len() >= 2 {
                let prev = tab.line_text(pl);
                let p = prev_pipes[prev_pipes.len() - 2];
                Some(Cursor::new(pl, p + 1 + usize::from(prev.as_bytes().get(p + 1) == Some(&b' '))))
            } else {
                None
            }
        } else {
            None
        };
        match target {
            Some(c) => {
                let tab = self.ui.tab_mut();
                tab.editor.set_selection(Selection::None);
                tab.editor.set_cursor(c);
                self.wake_cursor();
                self.request_redraw();
            }
            None if !back => self.smart_enter(),
            None => {}
        }
        true
    }

    /// Substitui o conteúdo da linha `line` (dentro de uma mudança já aberta).
    fn replace_line(tab: &mut Tab, line: usize, new: &str) {
        let old_len = tab.line_text(line).len();
        tab.editor.delete_range(Cursor::new(line, 0), Cursor::new(line, old_len));
        tab.editor.insert_at(Cursor::new(line, 0), new, None);
    }

    /// Alt+↑/↓: move a linha do cursor uma posição.
    fn move_line(&mut self, delta: i32) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let n = tab.editor.with_buffer(|b| b.lines.len());
        let j = cur.line as i32 + delta;
        if j < 0 || j >= n as i32 {
            return;
        }
        let j = j as usize;
        if tab.lines.get(j).is_some_and(|l| l.skip_cursor()) || tab.lines.get(cur.line).is_some_and(|l| l.skip_cursor()) {
            return;
        }
        let a = tab.line_text(cur.line);
        let b = tab.line_text(j);
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        Self::replace_line(tab, j, &a);
        Self::replace_line(tab, cur.line, &b);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(Cursor::new(j, cur.index.min(a.len())));
        self.after_change(change, true);
    }

    /// Ctrl+D: duplica a linha do cursor logo abaixo.
    fn duplicate_line(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let text = tab.line_text(cur.line);
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.insert_at(Cursor::new(cur.line, text.len()), &format!("\n{text}"), None);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(Cursor::new(cur.line + 1, cur.index));
        self.after_change(change, true);
    }

    /// Ctrl+Shift+K: apaga a linha do cursor inteira.
    fn delete_line(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let n = tab.editor.with_buffer(|b| b.lines.len());
        let len = tab.line_text(cur.line).len();
        if tab.lines.get(cur.line).is_some_and(|l| l.skip_cursor()) {
            return;
        }
        // Única linha da sua coluna: só esvazia, para a coluna continuar existindo.
        if tab.column_of(cur.line).is_some_and(|(bi, ci)| tab.columns[bi].cols[ci].map.len() == 1) {
            let (_, tab) = self.ui.ed();
            tab.editor.set_selection(Selection::None);
            tab.editor.start_change();
            tab.editor.delete_range(Cursor::new(cur.line, 0), Cursor::new(cur.line, len));
            let change = tab.editor.finish_change();
            self.after_change(change, true);
            return;
        }
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        let (start, end, target) = if cur.line + 1 < n {
            (Cursor::new(cur.line, 0), Cursor::new(cur.line + 1, 0), cur.line)
        } else if cur.line > 0 {
            let prev_len = tab.line_text(cur.line - 1).len();
            (Cursor::new(cur.line - 1, prev_len), Cursor::new(cur.line, len), cur.line - 1)
        } else {
            (Cursor::new(0, 0), Cursor::new(0, len), 0)
        };
        tab.editor.delete_range(start, end);
        let change = tab.editor.finish_change();
        let tl = tab.line_text(target).len();
        tab.editor.set_cursor(Cursor::new(target, cur.index.min(tl)));
        self.after_change(change, true);
    }

    /// Envolve a seleção (ou o cursor) com `open`…`close`; se já estiver
    /// envolta, remove (alterna). Seleções de várias linhas: por linha.
    fn wrap_selection(&mut self, open: &str, close: &str) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let (start, end) = tab.editor.selection_bounds().unwrap_or((cur, cur));
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        if start == end {
            tab.editor.insert_at(cur, &format!("{open}{close}"), None);
            tab.editor.set_selection(Selection::None);
            tab.editor.set_cursor(Cursor::new(cur.line, cur.index + open.len()));
        } else {
            let mut delta_start = 0i64;
            let mut delta_end = 0i64;
            for line in (start.line..=end.line).rev() {
                let text = tab.line_text(line);
                let s = if line == start.line { start.index } else { text.len() - text.trim_start().len() };
                let e = if line == end.line { end.index } else { text.len() };
                if s >= e {
                    continue;
                }
                // `*` e `**` se confundem: decide pela contagem de asteriscos em volta.
                let already = if open.bytes().all(|b| b == b'*') {
                    let before = text[..s].bytes().rev().take_while(|b| *b == b'*').count();
                    let after = text[e..].bytes().take_while(|b| *b == b'*').count();
                    if open.len() == 1 { before % 2 == 1 && after % 2 == 1 } else { before >= 2 && after >= 2 }
                } else {
                    text[..s].ends_with(open) && text[e..].starts_with(close)
                };
                if already {
                    tab.editor.delete_range(Cursor::new(line, e), Cursor::new(line, e + close.len()));
                    tab.editor.delete_range(Cursor::new(line, s - open.len()), Cursor::new(line, s));
                } else {
                    tab.editor.insert_at(Cursor::new(line, e), close, None);
                    tab.editor.insert_at(Cursor::new(line, s), open, None);
                }
                let d = if already { -(open.len() as i64) } else { open.len() as i64 };
                if line == start.line {
                    delta_start = d;
                }
                if line == end.line {
                    delta_end = d;
                }
            }
            let ns = Cursor::new(start.line, (start.index as i64 + delta_start).max(0) as usize);
            let ne = Cursor::new(end.line, (end.index as i64 + delta_end).max(0) as usize);
            tab.editor.set_selection(Selection::Normal(ns));
            tab.editor.set_cursor(ne);
        }
        let change = tab.editor.finish_change();
        self.after_change(change, true);
    }

    /// Define/alterna o prefixo de bloco (título, lista, checkbox, citação)
    /// nas linhas selecionadas ou na linha do cursor.
    fn set_prefix(&mut self, kind: Option<md::Prefix>) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let (start, end) = tab.editor.selection_bounds().unwrap_or((cur, cur));
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        let mut new_cursor = cur;
        for line in start.line..=end.line {
            let text = tab.line_text(line);
            let (indent, plen, have) = md::line_prefix(&text);
            let new = match kind {
                Some(k) if have != Some(k) => k.text(),
                _ => String::new(),
            };
            if plen > 0 {
                tab.editor.delete_range(Cursor::new(line, indent), Cursor::new(line, indent + plen));
            }
            if !new.is_empty() {
                tab.editor.insert_at(Cursor::new(line, indent), &new, None);
            }
            if line == cur.line {
                let idx = if cur.index >= indent + plen {
                    cur.index + new.len() - plen
                } else if cur.index > indent {
                    indent + new.len()
                } else {
                    cur.index
                };
                new_cursor = Cursor::new(line, idx);
            }
        }
        tab.editor.set_selection(Selection::None);
        tab.editor.set_cursor(new_cursor);
        let change = tab.editor.finish_change();
        self.after_change(change, true);
    }

    /// Insere uma tabela 3×2 vazia abaixo da linha atual e entra na 1ª célula.
    fn insert_table(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let line = tab.line_text(cur.line);
        let template = md::table_template(3, 2);
        if line.trim().is_empty() {
            self.insert_block(&template, 0, 2);
        } else {
            let (_, tab) = self.ui.ed();
            tab.editor.set_cursor(Cursor::new(cur.line, line.len()));
            self.insert_block(&format!("\n{template}"), 1, 2);
        }
    }

    fn open_picker(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let (start, end) = tab.editor.selection_bounds().unwrap_or((cur, cur));
        // Cor inicial: a que já envolve a seleção, senão laranja.
        let text = tab.line_text(start.line);
        let (mut h, mut sv, mut vv) = (28.0, 0.9, 1.0);
        if start.line == end.line {
            if let Some(n) = md::color_wrap_len(&text[..start.index], &text[end.index..]) {
                let open = &text[start.index - n..start.index];
                if let Some(c) = tab.lines.get(start.line).and_then(|l| l.spans.iter().find(|(r, _)| r.start >= start.index && r.start < end.index.max(start.index + 1)).and_then(|(_, f)| f.color)).or_else(|| md::parse_color_name(open)) {
                    (h, sv, vv) = rgb_to_hsv(c);
                }
            }
        }
        self.ui.picker = Some(Picker { h, s: sv, v: vv, panel: Rect::default(), center: (0, 0), radius: 1, bar: Rect::default() });
        self.update_preview();
    }

    fn close_picker(&mut self) {
        self.ui.picker = None;
        if self.ui.tab_mut().preview.take().is_some() {
            self.ui.restyle_active();
        }
        self.request_redraw();
    }

    /// Mostra a cor atual da roda na seleção, sem alterar o texto.
    fn update_preview(&mut self) {
        let Some(hex) = self.ui.picker.as_ref().map(Picker::hex) else { return };
        let tab = self.ui.tab_mut();
        let cur = tab.editor.cursor();
        let (start, end) = tab.editor.selection_bounds().unwrap_or((cur, cur));
        tab.preview = Some((start, end, hex));
        self.ui.restyle_active();
        self.request_redraw();
    }

    /// Aplica a cor da roda à seleção: substitui o `{#hex ` existente ou envolve.
    fn apply_picker(&mut self) {
        let Some(hex) = self.ui.picker.as_ref().map(Picker::hex) else { return };
        self.ui.picker = None;
        self.ui.tab_mut().preview = None;
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let (start, end) = tab.editor.selection_bounds().unwrap_or((cur, cur));
        let open = format!("{{#{hex:06x} ");
        if start.line == end.line {
            let text = tab.line_text(start.line);
            if let Some(n) = md::color_wrap_len(&text[..start.index], &text[end.index..]) {
                let (_, tab) = self.ui.ed();
                tab.editor.start_change();
                tab.editor.delete_range(Cursor::new(start.line, start.index - n), start);
                tab.editor.insert_at(Cursor::new(start.line, start.index - n), &open, None);
                let change = tab.editor.finish_change();
                let shift = open.len() as i64 - n as i64;
                let ns = Cursor::new(start.line, (start.index as i64 + shift) as usize);
                let ne = Cursor::new(end.line, (end.index as i64 + shift) as usize);
                tab.editor.set_selection(Selection::Normal(ns));
                tab.editor.set_cursor(ne);
                self.after_change(change, true);
                return;
            }
        }
        self.wrap_selection(&open, "}");
    }

    /// Remove marcações de cor da seleção.
    fn remove_color(&mut self) {
        self.ui.picker = None;
        self.ui.tab_mut().preview = None;
        let tab = self.ui.tab();
        let Some((start, end)) = tab.editor.selection_bounds() else {
            self.close_picker();
            return;
        };
        let Some(sel) = tab.editor.copy_selection() else { return };
        let mut s = start;
        let mut e = end;
        let mut sel_text = sel;
        // Seleção exatamente dentro de `{cor … }`: remove também o envoltório.
        if start.line == end.line {
            let text = tab.line_text(start.line);
            if let Some(n) = md::color_wrap_len(&text[..start.index], &text[end.index..]) {
                s = Cursor::new(start.line, start.index - n);
                e = Cursor::new(end.line, end.index + 1);
                sel_text = text[s.index..e.index].to_string();
            }
        }
        let clean = md::strip_colors(&sel_text);
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.delete_range(s, e);
        let ne = tab.editor.insert_at(s, &clean, None);
        tab.editor.set_selection(Selection::Normal(s));
        tab.editor.set_cursor(ne);
        let change = tab.editor.finish_change();
        self.after_change(change, true);
    }

    /// Marca/desmarca o checkbox da linha `line` (índice do `[`).
    fn toggle_checkbox(&mut self, line: usize, idx: usize) {
        let tab = self.ui.tab();
        let text = tab.line_text(line);
        let Some(c) = text.as_bytes().get(idx + 1) else { return };
        let new = if *c == b' ' { "x" } else { " " };
        let cur = tab.editor.cursor();
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.delete_range(Cursor::new(line, idx + 1), Cursor::new(line, idx + 2));
        tab.editor.insert_at(Cursor::new(line, idx + 1), new, None);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(cur);
        self.after_change(change, true);
    }

    /// Ctrl+Enter: alterna checkbox da linha atual (ou cria um).
    fn toggle_task_line(&mut self) {
        let tab = self.ui.tab();
        let cur = tab.editor.cursor();
        let text = tab.line_text(cur.line);
        if let Some(idx) = tab.lines.get(cur.line).filter(|l| matches!(l.block, Block::Toggle(_))).and_then(|l| l.toggle_idx) {
            self.toggle_fold(cur.line, idx);
            return;
        }
        if let Some((idx, _)) = tab.lines.get(cur.line).and_then(|l| l.checkbox) {
            self.toggle_checkbox(cur.line, idx);
            return;
        }
        let indent = text.len() - text.trim_start().len();
        let (at, ins) = match tab.lines.get(cur.line).map(|l| l.block) {
            Some(Block::Bullet) => (indent + 2, "[ ] "),
            _ => (indent, "- [ ] "),
        };
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.insert_at(Cursor::new(cur.line, at), ins, None);
        let change = tab.editor.finish_change();
        let new_idx = if cur.index >= at { cur.index + ins.len() } else { cur.index };
        tab.editor.set_cursor(Cursor::new(cur.line, new_idx));
        self.after_change(change, true);
    }

    // ---------- abas / notas ----------

    fn new_note(&mut self) {
        if self.ui.tab().is_empty_new() {
            return;
        }
        self.flush();
        self.ui.open_in_tab(None);
        self.ui.persist_state();
        self.wake_cursor();
        self.request_redraw();
    }

    /// Arquivos `.md` mudaram no disco. Para cada aba com uma dessas notas e
    /// mtime diferente da última leitura/gravação do app: sem edição pendente,
    /// recarrega mantendo o cursor; com edição pendente, a versão do app vence
    /// e a de fora é guardada na lixeira como conflito (nada se perde).
    fn on_disk_change(&mut self, names: &[String]) {
        let mut touched = false;
        for i in 0..self.ui.tabs.len() {
            let Some(path) = self.ui.tabs[i].path.clone() else { continue };
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            if !names.contains(&name) {
                continue;
            }
            let disk = file_mtime(&path);
            if disk == self.ui.tabs[i].disk_mtime {
                continue; // gravação do próprio app
            }
            touched = true;
            let dirty = self.ui.tabs[i].text() != self.ui.tabs[i].saved_text;
            let Some(mtime) = disk else {
                // Apagada/movida por fora: a aba fica com o texto, sem arquivo.
                let tab = &mut self.ui.tabs[i];
                tab.path = None;
                tab.disk_mtime = None;
                tab.saved_text = if dirty { String::new() } else { tab.text() };
                tab.status = "removida fora do app".to_string();
                continue;
            };
            if dirty {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("nota").to_string();
                let copy = self.ui.store.trash_dir.join(format!("{stem}.conflito-{}.md", store::stamp()));
                let _ = std::fs::copy(&path, copy);
                let tab = &mut self.ui.tabs[i];
                tab.disk_mtime = Some(mtime);
                tab.conflict = true;
                tab.status = "conflito: versão de fora guardada na lixeira".to_string();
                continue;
            }
            let text = self.ui.store.read(&path).unwrap_or_default();
            let fp = self.ui.font_px();
            let Ui { font_system, images, tabs, .. } = &mut self.ui;
            let tab = &mut tabs[i];
            let cur = tab.editor.cursor();
            tab.load(&text, font_system, images, fp);
            let n = tab.lines.len().max(1);
            let line = cur.line.min(n - 1);
            let len = tab.line_text(line).len();
            let mut index = cur.index.min(len);
            while index > 0 && !tab.line_text(line).is_char_boundary(index) {
                index -= 1;
            }
            tab.editor.set_cursor(Cursor::new(line, index));
            tab.disk_mtime = Some(mtime);
            tab.status = format!("atualizada fora do app · {}", store::now_hm());
        }
        if self.ui.list_open {
            // Atualiza a lista sem apagar a busca em andamento.
            let sel = self.ui.list_sel;
            self.ui.notes = self.ui.store.list();
            self.ui.refilter();
            self.ui.list_sel = sel.min(self.ui.filtered.len().saturating_sub(1));
            touched = true;
        }
        if touched {
            self.ensure_fonts_for_tabs();
            self.request_redraw();
        }
    }

    fn open_note(&mut self, path: PathBuf) {
        self.flush();
        self.ui.open_in_tab(Some(path));
        self.ensure_fonts_for_tabs();
        self.ui.persist_state();
        self.wake_cursor();
        self.request_redraw();
    }

    fn close_tab(&mut self, i: usize) {
        self.flush();
        self.ui.close_tab(i);
        self.wake_cursor();
        self.request_redraw();
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.ui.tabs.len() && i != self.ui.active {
            self.ui.active = i;
            self.ui.persist_state();
            self.wake_cursor();
            self.request_redraw();
        }
    }

    fn cycle_tab(&mut self, delta: i32) {
        let n = self.ui.tabs.len() as i32;
        let i = (self.ui.active as i32 + delta).rem_euclid(n) as usize;
        self.switch_tab(i);
    }

    fn trash_current(&mut self) {
        self.flush();
        let tab = self.ui.tab_mut();
        let path = tab.path.take();
        // Evita que o fechamento da aba regrave a nota que acabou de sair.
        tab.saved_text = tab.text();
        if let Some(p) = path {
            let _ = self.ui.store.trash(&p);
        }
        let i = self.ui.active;
        self.ui.close_tab(i);
        if self.ui.tabs.len() == 1 && self.ui.tab().is_empty_new() {
            if let Some(next) = self.ui.store.list().into_iter().next() {
                self.ui.open_in_tab(Some(next.path));
                self.ensure_fonts_for_tabs();
            }
        }
        self.wake_cursor();
        self.request_redraw();
    }

    fn zoom_by(&mut self, delta: i32) {
        let z = if delta == 0 { ZOOM_DEFAULT } else { self.ui.zoom + delta };
        self.ui.set_zoom(z);
        self.request_redraw();
    }

    // ---------- clipboard ----------

    fn copy(&mut self) -> bool {
        let Some(text) = self.ui.tab().editor.copy_selection() else { return false };
        let (Some(ddm), Some(dd)) = (&self.ddm, &self.data_device) else { return false };
        let src = ddm.create_copy_paste_source(&self.qh, MIMES);
        src.set_selection(dd, self.last_serial);
        self.copy_source = Some(src);
        self.clipboard_text = Arc::new(text);
        true
    }

    fn cut(&mut self) {
        if !self.copy() {
            return;
        }
        let (_, tab) = self.ui.ed();
        tab.editor.start_change();
        tab.editor.delete_selection();
        let change = tab.editor.finish_change();
        self.after_change(change, true);
    }

    fn update_primary(&mut self) {
        let Some(text) = self.ui.tab().editor.copy_selection() else { return };
        if text.is_empty() {
            return;
        }
        let (Some(psm), Some(dev)) = (&self.psm, &self.primary_device) else { return };
        let src = psm.create_selection_source(&self.qh, MIMES);
        src.set_selection(dev, self.last_serial);
        self.primary_source = Some(src);
        self.primary_text = Arc::new(text);
    }

    /// Lê um pipe de dados (colar / arrastar) e entrega o conteúdo a `done`.
    fn read_pipe(&mut self, pipe: smithay_client_toolkit::data_device_manager::ReadPipe, kind: PasteKind, finish: Option<DragOffer>) {
        // SAFETY: só ajustamos flags do fd; ele continua sendo do pipe.
        unsafe {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&pipe);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut data: Vec<u8> = Vec::new();
        let _ = self.loop_handle.insert_source(pipe, move |_, pipe, app| {
            // SAFETY: não fechamos o arquivo; só lemos.
            let f: &mut File = unsafe { pipe.get_mut() };
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => {
                        let bytes = std::mem::take(&mut data);
                        app.paste_done(bytes, kind);
                        if let Some(o) = &finish {
                            o.finish();
                        }
                        return PostAction::Remove;
                    }
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return PostAction::Continue,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return PostAction::Remove,
                }
            }
        });
    }

    fn paste(&mut self, primary: bool) {
        let pick = |mimes: &[String]| -> Option<(String, PasteKind)> {
            if let Some(m) = IMAGE_MIMES.iter().find(|m| mimes.iter().any(|x| x == *m)) {
                return Some((m.to_string(), PasteKind::Image));
            }
            if mimes.iter().any(|x| x == "text/uri-list") {
                return Some(("text/uri-list".to_string(), PasteKind::Uris));
            }
            MIMES.iter().find(|m| mimes.iter().any(|x| x == *m)).map(|m| (m.to_string(), PasteKind::Text))
        };
        let got = if primary {
            let Some(offer) = self.primary_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
            let Some((mime, kind)) = offer.with_mime_types(pick) else { return };
            offer.receive(mime).ok().map(|p| (p, kind))
        } else {
            let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
            let Some((mime, kind)) = offer.with_mime_types(pick) else { return };
            offer.receive(mime).ok().map(|p| (p, kind))
        };
        if let Some((pipe, kind)) = got {
            self.read_pipe(pipe, kind, None);
        }
    }

    fn paste_done(&mut self, bytes: Vec<u8>, kind: PasteKind) {
        match kind {
            PasteKind::Text => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                self.insert_text(&text);
            }
            PasteKind::Image => {
                if let Some(rel) = img::import(&bytes, &self.ui.store.images_dir, &store::stamp()) {
                    self.insert_image_line(&rel);
                } else {
                    self.ui.tab_mut().status = "imagem não reconhecida (PNG, JPEG ou WebP)".to_string();
                    self.request_redraw();
                }
            }
            PasteKind::Uris => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let mut others = Vec::new();
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    let path = img::percent_decode(line.strip_prefix("file://").unwrap_or(line));
                    if img::is_image_path(&path) {
                        if let Some(rel) = std::fs::read(&path).ok().and_then(|b| img::import(&b, &self.ui.store.images_dir, &store::stamp())) {
                            self.insert_image_line(&rel);
                            continue;
                        }
                    }
                    others.push(path);
                }
                if !others.is_empty() {
                    self.insert_text(&others.join("\n"));
                }
            }
        }
    }

    /// Insere `![](rel)` numa linha própria e deixa o cursor na linha seguinte.
    /// Insere a imagem no ponto do cursor (em linha: pode ficar no meio do texto).
    fn insert_image_line(&mut self, rel: &str) {
        self.ui.images.forget(rel);
        self.insert_text(&format!("![]({rel})"));
    }

    // ---------- redimensionar imagem pelos cantos ----------

    /// Canto de imagem sob o mouse: (índice do hit, canto direito?).
    fn image_corner_at(&self, px: i32, py: i32) -> Option<(usize, bool)> {
        let r = self.ui.px(IMG_CORNER);
        self.ui.hits.images.iter().enumerate().find_map(|(i, h)| {
            let near = |x: i32, y: i32| (px - x).abs() <= r && (py - y).abs() <= r;
            let (l, rt, t, b) = (h.rect.x, h.rect.right(), h.rect.y, h.rect.bottom());
            if near(rt, t) || near(rt, b) {
                Some((i, true))
            } else if near(l, t) || near(l, b) {
                Some((i, false))
            } else {
                None
            }
        })
    }

    /// Reescreve o marcador `![alt](caminho =Wx)` da imagem com a largura nova.
    fn set_image_width(&mut self, line: usize, start: usize, end: usize, w: u32) -> Option<usize> {
        let text = self.ui.tab().line_text(line);
        let markup = text.get(start..end)?;
        let (_, alt, path, _) = md::parse_image(markup)?;
        let new = format!("![{alt}]({path} ={w}x)");
        let (_, tab) = self.ui.ed();
        tab.editor.set_selection(Selection::None);
        tab.editor.start_change();
        tab.editor.delete_range(Cursor::new(line, start), Cursor::new(line, end));
        tab.editor.set_cursor(Cursor::new(line, start));
        tab.editor.insert_string(&new, None);
        let change = tab.editor.finish_change();
        let new_end = start + new.len();
        tab.editor.set_cursor(Cursor::new(line, new_end));
        self.after_change(change, false);
        Some(new_end)
    }

    /// Grava as larguras (px) do bloco como porcentagens na linha `:::`
    /// de abertura; larguras praticamente iguais voltam a `:::` puro.
    fn set_col_widths(&mut self, line: usize, w: &[f32], base: f32) {
        let base = base.max(1.0);
        let mut pct: Vec<i32> = w.iter().map(|x| (x / base * 100.0).round().max(1.0) as i32).collect();
        let total = (w.iter().sum::<f32>() / base * 100.0).round() as i32;
        let diff = total - pct.iter().sum::<i32>();
        if let Some(m) = (0..pct.len()).max_by_key(|&i| pct[i]) {
            pct[m] += diff;
        }
        let equal = (total - 100).abs() <= 1 && pct.iter().max().zip(pct.iter().min()).is_some_and(|(a, b)| a - b <= 1);
        let new = if equal { ":::".to_string() } else { format!("::: {}", pct.iter().map(i32::to_string).collect::<Vec<_>>().join(" ")) };
        let tab = self.ui.tab();
        if tab.line_text(line).trim() == new || md::col_fence(&tab.line_text(line)).is_none() {
            return;
        }
        let (_, tab) = self.ui.ed();
        let cur = tab.editor.cursor();
        let sel = tab.editor.selection();
        tab.editor.start_change();
        Self::replace_line(tab, line, &new);
        let change = tab.editor.finish_change();
        tab.editor.set_cursor(cur);
        tab.editor.set_selection(sel);
        self.after_change(change, false);
    }

    fn col_drag_motion(&mut self, px: i32) {
        let Some(d) = &self.ui.col_drag else { return };
        let dx = (px - d.x0) as f32;
        let mut w = d.w0.clone();
        let n = w.len();
        if d.ci >= n {
            // Borda direita: só a última coluna muda, até a margem direita.
            let others: f32 = w[..n - 1].iter().sum();
            w[n - 1] = (d.w0[n - 1] + dx).clamp(COL_MIN_W, (d.max - others).max(COL_MIN_W));
        } else {
            let (a, b) = (d.ci - 1, d.ci);
            let pair = d.w0[a] + d.w0[b];
            let min = COL_MIN_W.min(pair / 2.0);
            let left = (d.w0[a] + dx).clamp(min, pair - min);
            w[a] = left;
            w[b] = pair - left;
        }
        let (line, base) = (d.line, d.base);
        self.set_col_widths(line, &w, base);
        self.request_redraw();
    }

    fn image_drag_motion(&mut self, px: i32) {
        let Some(d) = &self.img_drag else { return };
        let dx = px - d.x0;
        let target = if d.right { d.w0 + dx } else { d.w0 - dx };
        let max_w = self.ui.tab().wide_w.max(IMG_MIN_W as f32) as i32;
        let w = target.clamp(IMG_MIN_W as i32, max_w) as u32;
        let cur_w = self.ui.tab().inline_imgs.iter().find(|im| im.line == d.line && im.start == d.start).map(|im| im.w);
        if cur_w == Some(w) {
            return;
        }
        let (line, start, end) = (d.line, d.start, d.end);
        if let Some(new_end) = self.set_image_width(line, start, end, w) {
            if let Some(d) = self.img_drag.as_mut() {
                d.end = new_end;
            }
        }
    }

    // ---------- teclado ----------

    fn on_key(&mut self, ev: &KeyEvent) {
        let m = self.modifiers;
        let sym = ev.keysym;
        let plain = !(m.ctrl || m.alt || m.logo);

        // Dead keys / compose (ex.: ´ + a = á).
        if plain {
            if let Some(cs) = &mut self.compose {
                if cs.feed(sym) == xkb::compose::FeedResult::Accepted {
                    match cs.status() {
                        xkb::compose::Status::Composing => return,
                        xkb::compose::Status::Composed => {
                            let s = cs.utf8();
                            cs.reset();
                            if let Some(s) = s {
                                if self.ui.list_open {
                                    self.ui.query.push_str(&s);
                                    self.ui.refilter();
                                    self.request_redraw();
                                } else {
                                    self.insert_text(&s);
                                }
                            }
                            return;
                        }
                        xkb::compose::Status::Cancelled => {
                            cs.reset();
                            return;
                        }
                        xkb::compose::Status::Nothing => {}
                    }
                }
            }
        }

        if self.ui.list_open {
            self.on_key_list(ev);
            return;
        }

        let ch = sym.key_char().map(|c| c.to_ascii_lowercase());
        // Dígito físico (independe de layout/Shift): evdev KEY_1..KEY_9 = 2..10, KEY_0 = 11.
        let digit = match ev.raw_code {
            2..=10 => Some((ev.raw_code - 1) as u8),
            11 => Some(0),
            _ => ch.filter(|c| c.is_ascii_digit()).map(|c| c as u8 - b'0'),
        };

        if self.ui.slash.is_some() && !m.ctrl && !m.alt {
            match sym {
                Keysym::Escape => {
                    self.ui.slash = None;
                    self.request_redraw();
                    return;
                }
                Keysym::Up => {
                    let n = self.ui.slash_matches().len().max(1);
                    if let Some(sl) = self.ui.slash.as_mut() {
                        sl.sel = (sl.sel + n - 1) % n;
                    }
                    self.request_redraw();
                    return;
                }
                Keysym::Down | Keysym::Tab => {
                    let n = self.ui.slash_matches().len().max(1);
                    if let Some(sl) = self.ui.slash.as_mut() {
                        sl.sel = (sl.sel + 1) % n;
                    }
                    self.request_redraw();
                    return;
                }
                Keysym::Return | Keysym::KP_Enter => {
                    self.slash_apply();
                    return;
                }
                _ => {}
            }
        }

        if self.ui.glyphs.is_some() {
            self.glyph_key(sym, ch, &m);
            return;
        }

        if self.ui.picker.is_some() {
            match (sym, digit) {
                (Keysym::Escape, _) => self.close_picker(),
                (Keysym::Return, _) | (Keysym::KP_Enter, _) => self.apply_picker(),
                (_, Some(0)) => self.remove_color(),
                _ => {
                    if m.ctrl && m.shift && ch == Some('c') {
                        self.close_picker();
                    }
                }
            }
            return;
        }

        if m.ctrl && m.shift && !m.alt {
            let handled = match (digit, ch) {
                (Some(0), _) => { self.set_prefix(None); true }
                (Some(n @ 1..=6), _) => { self.set_prefix(Some(md::Prefix::Heading(n))); true }
                (Some(7), _) => { self.set_prefix(Some(md::Prefix::Numbered)); true }
                (Some(8), _) => { self.set_prefix(Some(md::Prefix::Bullet)); true }
                (Some(9), _) => { self.set_prefix(Some(md::Prefix::Task)); true }
                (_, Some('q')) => { self.set_prefix(Some(md::Prefix::Quote)); true }
                (_, Some('t')) => { self.insert_table(); true }
                (_, Some('s')) => { self.wrap_selection("~~", "~~"); true }
                (_, Some('c')) => {
                    self.open_picker();
                    true
                }
                (_, Some('k')) => { self.delete_line(); true }
                (_, Some('n')) => { self.duplicate_note(); true }
                (_, Some('e')) => { self.open_glyphs(GlyphKind::Emoji); true }
                (_, Some('i')) => { self.open_glyphs(GlyphKind::Symbol); true }
                _ => match sym {
                    Keysym::Up => { self.move_line(-1); true }
                    Keysym::Down => { self.move_line(1); true }
                    _ => false,
                },
            };
            if handled {
                self.after_input();
                return;
            }
        }
        if m.ctrl && !m.alt {
            match (ch, sym) {
                (Some('b'), _) => self.wrap_selection("**", "**"),
                (Some('i'), _) => self.wrap_selection("*", "*"),
                (Some('e'), _) => self.wrap_selection("`", "`"),
                (Some('k'), _) if !m.shift => self.wrap_selection("[", "](https://)"),
                (Some('d'), _) if !m.shift => self.duplicate_line(),
                (_, Keysym::Up) => self.motion(Motion::ParagraphStart, m.shift),
                (_, Keysym::Down) => self.motion(Motion::ParagraphEnd, m.shift),
                (Some('n'), _) | (Some('t'), _) => self.new_note(),
                (Some('.'), _) | (_, Keysym::period) => self.open_glyphs(GlyphKind::Emoji),
                (Some(','), _) | (_, Keysym::comma) => self.open_glyphs(GlyphKind::Symbol),
                (Some('s'), _) => {
                    self.flush();
                    self.request_redraw();
                }
                (Some('l'), _) | (Some('o'), _) | (Some('k'), _) => self.toggle_list(),
                (Some('z'), _) if m.shift => self.redo(),
                (Some('z'), _) => self.undo(),
                (Some('y'), _) => self.redo(),
                (Some('w'), _) => {
                    let i = self.ui.active;
                    self.close_tab(i);
                }
                (Some('q'), _) => self.exit = true,
                (Some('d'), _) if m.shift => self.trash_current(),
                (Some('c'), _) => {
                    self.copy();
                }
                (Some('x'), _) => self.cut(),
                (Some('v'), _) => self.paste(false),
                (Some('a'), _) => self.select_all(),
                (Some('='), _) | (Some('+'), _) => self.zoom_by(1),
                (Some('-'), _) | (Some('_'), _) => self.zoom_by(-1),
                (Some('0'), _) => self.zoom_by(0),
                (Some(d @ '1'..='9'), _) => self.switch_tab(d as usize - '1' as usize),
                (_, Keysym::KP_Add) => self.zoom_by(1),
                (_, Keysym::KP_Subtract) => self.zoom_by(-1),
                (_, Keysym::Tab) => self.cycle_tab(1),
                (_, Keysym::ISO_Left_Tab) => self.cycle_tab(-1),
                (_, Keysym::Page_Down) => self.cycle_tab(1),
                (_, Keysym::Page_Up) => self.cycle_tab(-1),
                (_, Keysym::Return) | (_, Keysym::KP_Enter) => self.toggle_task_line(),
                (_, Keysym::Left) => self.motion(Motion::LeftWord, m.shift),
                (_, Keysym::Right) => self.motion(Motion::RightWord, m.shift),
                (_, Keysym::Home) => self.motion(Motion::BufferStart, m.shift),
                (_, Keysym::End) => self.motion(Motion::BufferEnd, m.shift),
                (_, Keysym::BackSpace) => self.delete_word(true),
                (_, Keysym::Delete) => self.delete_word(false),
                _ => {}
            }
            self.after_input();
            return;
        }
        if m.alt && !m.logo {
            match sym {
                Keysym::Up => self.move_line(-1),
                Keysym::Down => self.move_line(1),
                _ => {}
            }
            self.after_input();
            return;
        }
        if m.logo {
            return;
        }
        match sym {
            // Esc: primeiro limpa a seleção; sem seleção, sai.
            Keysym::Escape => {
                if self.ui.tab().editor.selection() != Selection::None {
                    self.ui.tab_mut().editor.set_selection(Selection::None);
                    self.request_redraw();
                } else {
                    self.exit = true;
                }
            }
            // Shift+Enter: quebra de linha simples (sem continuar listas).
            Keysym::Return | Keysym::KP_Enter if m.shift => self.soft_enter(),
            Keysym::Return | Keysym::KP_Enter => self.smart_enter(),
            Keysym::BackSpace => self.smart_backspace(),
            Keysym::Delete | Keysym::KP_Delete => self.smart_delete(),
            Keysym::Tab => {
                if !self.table_tab(false) {
                    self.edit(Action::Indent, true);
                }
            }
            Keysym::ISO_Left_Tab => {
                if !self.table_tab(true) {
                    self.edit(Action::Unindent, true);
                }
            }
            Keysym::Left => self.motion(Motion::Left, m.shift),
            Keysym::Right => self.motion(Motion::Right, m.shift),
            Keysym::Up => self.motion(Motion::Up, m.shift),
            Keysym::Down => self.motion(Motion::Down, m.shift),
            Keysym::Home => self.motion(Motion::Home, m.shift),
            Keysym::End => self.motion(Motion::End, m.shift),
            Keysym::Page_Up => self.motion(Motion::PageUp, m.shift),
            Keysym::Page_Down => self.motion(Motion::PageDown, m.shift),
            _ => {
                if let Some(t) = &ev.utf8 {
                    if !t.is_empty() && !t.chars().any(char::is_control) {
                        let open_slash = t == "/" && {
                            let tab = self.ui.tab();
                            let cur = tab.editor.cursor();
                            let line = tab.line_text(cur.line);
                            line[..cur.index.min(line.len())].trim().is_empty()
                        };
                        let cur = self.ui.tab().editor.cursor();
                        self.insert_text(t);
                        if open_slash {
                            self.ui.slash = Some(Slash { line: cur.line, start: cur.index, sel: 0 });
                        }
                    }
                }
            }
        }
        // Menu `/` fecha se o cursor saiu da consulta.
        if let Some(sl) = &self.ui.slash {
            let cur = self.ui.tab().editor.cursor();
            let text = self.ui.tab().line_text(sl.line);
            if cur.line != sl.line || cur.index <= sl.start || text.as_bytes().get(sl.start) != Some(&b'/') {
                self.ui.slash = None;
            }
        }
        self.after_input();
    }

    fn on_key_list(&mut self, ev: &KeyEvent) {
        let m = self.modifiers;
        let sym = ev.keysym;
        let ch = sym.key_char().map(|c| c.to_ascii_lowercase());
        if m.ctrl {
            match ch {
                Some('l') | Some('o') | Some('k') => self.toggle_list(),
                Some('n') | Some('t') => {
                    self.ui.list_open = false;
                    self.new_note();
                }
                Some('q') => self.exit = true,
                _ => {}
            }
            return;
        }
        match sym {
            Keysym::Escape => self.toggle_list(),
            Keysym::Up => self.ui.list_move(-1),
            Keysym::Down | Keysym::Tab => self.ui.list_move(1),
            Keysym::Page_Up => self.ui.list_move(-8),
            Keysym::Page_Down => self.ui.list_move(8),
            Keysym::Home => self.ui.list_sel = 0,
            Keysym::End => self.ui.list_sel = self.ui.filtered.len().saturating_sub(1),
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(p) = self.ui.selected_note_path() {
                    self.ui.list_open = false;
                    self.open_note(p);
                }
            }
            Keysym::BackSpace => {
                self.ui.query.pop();
                self.ui.refilter();
            }
            _ => {
                if let Some(t) = &ev.utf8 {
                    if !t.is_empty() && !t.chars().any(char::is_control) {
                        self.ui.query.push_str(t);
                        self.ui.refilter();
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn toggle_list(&mut self) {
        if self.ui.list_open {
            self.ui.list_open = false;
            self.wake_cursor();
        } else {
            self.flush();
            self.ui.open_list();
            let previews: String = self.ui.notes.iter().map(|n| format!("{} {}\n", n.title, n.preview)).collect();
            self.ensure_fonts(&previews);
        }
        self.request_redraw();
    }

    // ---------- mouse ----------

    fn set_cursor_icon(&mut self, icon: CursorIcon) {
        if icon == self.cursor_icon {
            return;
        }
        self.cursor_icon = icon;
        if let Some(p) = &self.pointer {
            let _ = p.set_cursor(&self.conn, icon);
        }
    }

    fn phys(&self) -> (i32, i32) {
        let s = self.ui.scale.max(1) as f64;
        ((self.pointer_pos.0 * s) as i32, (self.pointer_pos.1 * s) as i32)
    }

    fn clickable_rects(&self) -> Vec<Rect> {
        let h = &self.ui.hits;
        let mut v = vec![h.list_btn, h.new_btn, h.trash_btn, h.close_btn];
        v.extend(h.tabs.iter().map(|(r, _)| *r));
        v.extend(h.tab_closes.iter().map(|(r, _)| *r));
        v.extend(h.checkboxes.iter().map(|(r, _, _)| *r));
        v.extend(h.toggles.iter().map(|(r, _, _)| *r));
        if self.ui.list_open {
            v.extend(h.rows.iter().map(|(r, _)| *r));
        }
        v
    }

    /// Devolve true se o ponteiro está na roda ou na barra (e atualiza a cor).
    fn picker_track(&mut self, px: i32, py: i32) -> bool {
        let Some(pk) = self.ui.picker.as_mut() else { return false };
        let (cx, cy) = pk.center;
        let dx = (px - cx) as f32;
        let dy = (py - cy) as f32;
        let d = (dx * dx + dy * dy).sqrt();
        if d <= pk.radius as f32 + 2.0 {
            pk.h = dy.atan2(dx).to_degrees().rem_euclid(360.0);
            pk.s = (d / pk.radius as f32).min(1.0);
            self.update_preview();
            return true;
        }
        let bar = pk.bar;
        if Rect::new(bar.x - 4, bar.y, bar.w + 8, bar.h).contains(px, py) {
            pk.v = (1.0 - (py - bar.y) as f32 / bar.h as f32).clamp(0.0, 1.0);
            self.update_preview();
            return true;
        }
        false
    }

    fn on_pointer_motion(&mut self) {
        let (px, py) = self.phys();
        if let Some(g) = self.ui.glyphs.as_mut() {
            self.ui.hover = (px, py);
            if let Some(&(_, fi)) = g.cells.iter().find(|(r, _)| r.contains(px, py)) {
                g.sel = fi;
            }
            self.set_cursor_icon(CursorIcon::Default);
            self.request_redraw();
            return;
        }
        if self.ui.picker.is_some() {
            self.ui.hover = (px, py);
            self.set_cursor_icon(CursorIcon::Default);
            self.picker_track(px, py);
            return;
        }
        if self.img_drag.is_some() {
            self.ui.hover = (px, py);
            self.image_drag_motion(px);
            return;
        }
        if self.ui.col_drag.is_some() {
            self.ui.hover = (px, py);
            self.col_drag_motion(px);
            return;
        }
        let div = if self.ui.list_open || self.pointer_down {
            None
        } else {
            self.ui.hits.col_divs.iter().find(|(r, _, _)| r.contains(px, py)).map(|&(_, bi, ci)| (bi, ci))
        };
        if div != self.ui.col_hover {
            self.ui.col_hover = div;
            self.request_redraw();
        }
        if div.is_some() {
            self.ui.hover = (px, py);
            self.set_cursor_icon(CursorIcon::EwResize);
            return;
        }
        let corner = if self.ui.list_open { None } else { self.image_corner_at(px, py) };
        let rects = self.clickable_rects();
        let over_button = rects.iter().any(|r| r.contains(px, py));
        let h = &self.ui.hits;
        let over_editor = h.editor.contains(px, py) && !(self.ui.list_open && h.panel.contains(px, py));
        let icon = if let Some((_, right)) = corner {
            if right { CursorIcon::NwseResize } else { CursorIcon::NeswResize }
        } else if over_button {
            CursorIcon::Pointer
        } else if over_editor {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        self.set_cursor_icon(icon);

        let was = self.ui.hover;
        self.ui.hover = (px, py);
        if self.pointer_down && !self.ui.list_open {
            if let Some(c) = self.cell_hit(px, py).or_else(|| self.column_hit(px, py)) {
                let tab = self.ui.tab_mut();
                if tab.editor.selection() == Selection::None {
                    tab.editor.set_selection(Selection::Normal(tab.editor.cursor()));
                }
                tab.editor.set_cursor(c);
            } else {
                let (x, y) = self.editor_coords(px, py);
                let (fs, tab) = self.ui.ed();
                tab.editor.action(fs, Action::Drag { x, y });
            }
            self.request_redraw();
        } else if rects.iter().any(|r| r.contains(was.0, was.1) != r.contains(px, py)) {
            self.request_redraw();
        }
    }

    /// Clique/arraste dentro de uma coluna → cursor no texto real.
    fn column_hit(&mut self, px: i32, py: i32) -> Option<Cursor> {
        let &(rect, bi, ci) = self.ui.hits.cols.iter().find(|(r, _, _)| r.contains(px, py))?;
        let col = self.ui.tabs[self.ui.active].columns.get(bi)?.cols.get(ci)?;
        let sub = col.buffer.hit((px - rect.x) as f32, (py - rect.y) as f32)?;
        let main = *col.map.get(sub.line).or(col.map.last())?;
        Some(Cursor::new(main, sub.index))
    }

    /// Clique numa célula de tabela → cursor na posição correspondente da linha crua.
    fn cell_hit(&self, px: i32, py: i32) -> Option<Cursor> {
        let h = self.ui.hits.cells.iter().find(|c| c.rect.contains(px, py))?;
        let tab = &self.ui.tabs[self.ui.active];
        let cell = tab.tables.get(h.bi)?.rows.get(h.ri)?.cells.get(h.ci)?;
        let len = cell.end - cell.start;
        let idx = if px < h.tx {
            0
        } else {
            cell.buffer.hit((px - h.tx) as f32, 1.0).map(|c| c.index).unwrap_or(len)
        };
        Some(Cursor::new(h.line, cell.start + idx.min(len)))
    }

    fn editor_coords(&self, px: i32, py: i32) -> (i32, i32) {
        let (ox, oy) = self.ui.hits.text_origin;
        (px - ox, py - oy)
    }

    fn on_pointer_press(&mut self, button: u32, serial: u32) {
        self.last_serial = serial;
        let (px, py) = self.phys();
        if let Some(g) = &self.ui.glyphs {
            if g.panel.contains(px, py) {
                if button == BTN_LEFT {
                    if let Some(&(_, fi)) = g.cells.iter().find(|(r, _)| r.contains(px, py)) {
                        self.glyph_insert(fi);
                    }
                }
            } else {
                self.close_glyphs();
            }
            return;
        }
        if let Some(pk) = &self.ui.picker {
            let inside = pk.panel.contains(px, py);
            if self.picker_track(px, py) {
                self.apply_picker();
            } else if !inside {
                self.close_picker();
            }
            return;
        }
        let h = &self.ui.hits;
        let (list_btn, new_btn, trash_btn, close_btn, header, editor, panel) =
            (h.list_btn, h.new_btn, h.trash_btn, h.close_btn, h.header, h.editor, h.panel);
        let tab_hit = h.tabs.iter().find(|(r, _)| r.contains(px, py)).map(|(_, i)| *i);
        let tab_close_hit = h.tab_closes.iter().find(|(r, _)| r.contains(px, py)).map(|(_, i)| *i);
        let checkbox_hit = h.checkboxes.iter().find(|(r, _, _)| r.contains(px, py)).map(|(_, l, i)| (*l, *i));
        let toggle_hit = h.toggles.iter().find(|(r, _, _)| r.contains(px, py)).map(|(_, l, i)| (*l, *i));
        if self.ui.slash.is_some() {
            self.ui.slash = None;
        }

        if self.ui.list_open {
            if panel.contains(px, py) {
                if button == BTN_LEFT {
                    if let Some(&(_, fi)) = self.ui.hits.rows.iter().find(|(r, _)| r.contains(px, py)) {
                        self.ui.list_sel = fi;
                        if let Some(p) = self.ui.selected_note_path() {
                            self.ui.list_open = false;
                            self.open_note(p);
                        }
                    }
                }
            } else if !list_btn.contains(px, py) || button != BTN_LEFT {
                self.ui.list_open = false;
                self.wake_cursor();
                self.request_redraw();
            } else {
                self.toggle_list();
            }
            return;
        }

        if let Some(i) = tab_close_hit {
            if button == BTN_LEFT {
                self.close_tab(i);
                return;
            }
        }
        if let Some(i) = tab_hit {
            match button {
                BTN_LEFT => self.switch_tab(i),
                BTN_MIDDLE => self.close_tab(i),
                _ => {}
            }
            return;
        }
        if button == BTN_LEFT {
            if list_btn.contains(px, py) {
                self.toggle_list();
                return;
            }
            if new_btn.contains(px, py) {
                self.new_note();
                return;
            }
            if trash_btn.contains(px, py) {
                self.trash_current();
                return;
            }
            if self.ui.csd && close_btn.contains(px, py) {
                self.exit = true;
                return;
            }
            if header.contains(px, py) {
                if let Some(seat) = &self.seat {
                    self.window.move_(seat, serial);
                }
                return;
            }
            if let Some((line, idx)) = checkbox_hit {
                self.toggle_checkbox(line, idx);
                return;
            }
            if let Some((line, idx)) = toggle_hit {
                self.toggle_fold(line, idx);
                return;
            }
        }
        if button == BTN_LEFT && !self.ui.list_open {
            if let Some(&(_, bi, ci)) = self.ui.hits.col_divs.iter().find(|(r, _, _)| r.contains(px, py)) {
                let now = Instant::now();
                let (t, pos, _) = self.last_click;
                let double = now - t < Duration::from_millis(400) && (pos.0 - self.pointer_pos.0).abs() < 5.0;
                self.last_click = (now, self.pointer_pos, 1);
                let Some(b) = self.ui.tab().columns.get(bi) else { return };
                let (line, base, max, w0) = (b.start, b.base, b.max, b.cols.iter().map(|c| c.w).collect::<Vec<_>>());
                if double {
                    // Duplo clique num divisor: colunas iguais na largura de leitura.
                    let n = w0.len();
                    self.set_col_widths(line, &vec![base / n as f32; n], base);
                    self.last_click = (now - Duration::from_secs(10), self.pointer_pos, 0);
                } else {
                    self.ui.tab_mut().undo.hold();
                    self.ui.col_drag = Some(ColDrag { bi, ci, line, x0: px, w0, base, max });
                }
                self.set_cursor_icon(CursorIcon::EwResize);
                self.request_redraw();
                return;
            }
            if let Some((i, right)) = self.image_corner_at(px, py) {
                let h = &self.ui.hits.images[i];
                self.img_drag = Some(ImgDrag { line: h.line, start: h.start, end: h.end, w0: h.rect.w, x0: px, right });
                self.ui.tab_mut().undo.hold();
                self.set_cursor_icon(if right { CursorIcon::NwseResize } else { CursorIcon::NeswResize });
                return;
            }
        }
        if button == BTN_LEFT {
            if let Some(c) = self.cell_hit(px, py).or_else(|| self.column_hit(px, py)) {
                let tab = self.ui.tab_mut();
                if self.modifiers.shift {
                    if tab.editor.selection() == Selection::None {
                        tab.editor.set_selection(Selection::Normal(tab.editor.cursor()));
                    }
                } else {
                    tab.editor.set_selection(Selection::None);
                }
                tab.editor.set_cursor(c);
                self.pointer_down = true;
                self.wake_cursor();
                self.request_redraw();
                self.after_input();
                return;
            }
        }
        if editor.contains(px, py) {
            let (x, y) = self.editor_coords(px, py);
            match button {
                BTN_LEFT => {
                    let now = Instant::now();
                    let (t, pos, count) = self.last_click;
                    let near = (pos.0 - self.pointer_pos.0).abs() < 5.0 && (pos.1 - self.pointer_pos.1).abs() < 5.0;
                    let count = if now - t < Duration::from_millis(400) && near { count + 1 } else { 1 };
                    self.last_click = (now, self.pointer_pos, count);
                    let action = if self.modifiers.shift {
                        Action::Drag { x, y }
                    } else {
                        match count {
                            1 => Action::Click { x, y },
                            2 => Action::DoubleClick { x, y },
                            _ => Action::TripleClick { x, y },
                        }
                    };
                    let (fs, tab) = self.ui.ed();
                    let before = tab.editor.cursor();
                    tab.editor.action(fs, action);
                    self.settle_cursor(before, true, false);
                    self.pointer_down = true;
                    self.wake_cursor();
                    self.request_redraw();
                    self.after_input();
                }
                BTN_MIDDLE => {
                    let (fs, tab) = self.ui.ed();
                    tab.editor.set_selection(Selection::None);
                    tab.editor.action(fs, Action::Click { x, y });
                    self.paste(true);
                }
                _ => {}
            }
        }
    }

    fn on_pointer_release(&mut self, button: u32) {
        if button == BTN_LEFT && self.ui.col_drag.take().is_some() {
            // Fecha o grupo de desfazer: o arraste inteiro vira um só passo.
            self.ui.tab_mut().undo.release();
            self.request_redraw();
            self.on_pointer_motion();
            return;
        }
        if button == BTN_LEFT && self.img_drag.take().is_some() {
            self.ui.tab_mut().undo.release();
            self.on_pointer_motion();
            return;
        }
        if button == BTN_LEFT && self.pointer_down {
            self.pointer_down = false;
            self.update_primary();
        }
    }

    fn on_scroll(&mut self, vertical: f64) {
        if vertical == 0.0 {
            return;
        }
        let (px, py) = self.phys();
        if let Some(g) = self.ui.glyphs.as_mut() {
            g.scroll(if vertical > 0.0 { 1 } else { -1 });
        } else if self.ui.list_open && self.ui.hits.panel.contains(px, py) {
            self.ui.list_move(if vertical > 0.0 { 1 } else { -1 });
        } else {
            let pixels = (vertical * 3.0 * self.ui.scale as f64) as f32;
            let (fs, tab) = self.ui.ed();
            tab.editor.action(fs, Action::Scroll { pixels });
        }
        self.request_redraw();
    }
}

// =====================================================================
// Handlers do smithay-client-toolkit.
// =====================================================================

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, new_factor: i32) {
        if new_factor != self.ui.scale && new_factor >= 1 {
            self.ui.scale = new_factor;
            let _ = self.window.set_buffer_scale(new_factor as u32);
            self.request_redraw();
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.needs_redraw {
            self.draw();
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        self.ui.csd = configure.decoration_mode == DecorationMode::Client;
        let (w, h) = match configure.new_size {
            (Some(w), Some(h)) => (w.get(), h.get()),
            _ => (self.ui.width, self.ui.height),
        };
        if (w, h) != (self.ui.width, self.ui.height) {
            self.ui.width = w;
            self.ui.height = h;
        }
        if !self.configured {
            self.configured = true;
            trace("primeiro configure");
            if let Some(tok) = self.pending_token.take() {
                self.activate(&tok);
            }
        }
        self.draw();
    }
}

impl ActivationHandler for App {
    type RequestUdata = ();
    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        self.activate(&token);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
            if let Some(ddm) = &self.ddm {
                self.data_device = Some(ddm.get_data_device(qh, &seat));
            }
            if let Some(psm) = &self.psm {
                self.primary_device = Some(psm.get_selection_device(qh, &seat));
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let kbd = self
                .seat_state
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app: &mut App, _, event| app.on_key(&event)),
                )
                .ok();
            self.keyboard = kbd;
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme::<App, ()>(qh, &seat, self.shm.wl_shm(), cursor_surface, ThemeSpec::default())
                .ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        if self.window.wl_surface() == surface {
            self.last_serial = serial;
            self.ui.focused = true;
            self.wake_cursor();
            self.request_redraw();
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &WlSurface, _: u32) {
        if self.window.wl_surface() == surface {
            self.ui.focused = false;
            self.pointer_down = false;
            self.flush();
            self.request_redraw();
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.last_serial = serial;
        self.on_key(&event);
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.on_key(&event);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, modifiers: Modifiers, _: RawModifiers, _: u32) {
        self.last_serial = serial;
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            if &ev.surface != self.window.wl_surface() {
                continue;
            }
            self.pointer_pos = ev.position;
            match ev.kind {
                PointerEventKind::Enter { serial } => {
                    self.last_serial = serial;
                    self.cursor_icon = CursorIcon::Default;
                    if let Some(p) = &self.pointer {
                        let _ = p.set_cursor(&self.conn, CursorIcon::Default);
                    }
                    self.on_pointer_motion();
                }
                PointerEventKind::Leave { .. } => {
                    self.ui.hover = (-1, -1);
                    self.request_redraw();
                }
                PointerEventKind::Motion { .. } => self.on_pointer_motion(),
                PointerEventKind::Press { button, serial, .. } => self.on_pointer_press(button, serial),
                PointerEventKind::Release { button, serial, .. } => {
                    self.last_serial = serial;
                    self.on_pointer_release(button);
                }
                PointerEventKind::Axis { vertical, .. } => self.on_scroll(vertical.absolute),
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &WlSurface) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let mime = offer.with_mime_types(|m| {
            ["text/uri-list", "text/plain;charset=utf-8", "text/plain"].iter().find(|x| m.iter().any(|y| y == *x)).map(|x| x.to_string())
        });
        offer.accept_mime_type(self.last_serial, mime);
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let pick = offer.with_mime_types(|m| {
            if m.iter().any(|x| x == "text/uri-list") {
                Some(("text/uri-list".to_string(), PasteKind::Uris))
            } else {
                ["text/plain;charset=utf-8", "text/plain"].iter().find(|x| m.iter().any(|y| y == *x)).map(|x| (x.to_string(), PasteKind::Text))
            }
        });
        let Some((mime, kind)) = pick else { return };
        if let Ok(pipe) = offer.receive(mime) {
            self.read_pipe(pipe, kind, Some(offer));
        }
    }
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _mime: String, fd: WritePipe) {
        if self.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            let text = self.clipboard_text.clone();
            std::thread::spawn(move || {
                let mut f = File::from(OwnedFd::from(fd));
                let _ = f.write_all(text.as_bytes());
            });
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.copy_source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl PrimarySelectionDeviceHandler for App {
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpPrimarySelectionDeviceV1) {}
}

impl PrimarySelectionSourceHandler for App {
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &ZwpPrimarySelectionSourceV1, _mime: String, fd: WritePipe) {
        if self.primary_source.as_ref().is_some_and(|s| s.inner() == source) {
            let text = self.primary_text.clone();
            std::thread::spawn(move || {
                let mut f = File::from(OwnedFd::from(fd));
                let _ = f.write_all(text.as_bytes());
            });
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &ZwpPrimarySelectionSourceV1) {
        if self.primary_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.primary_source = None;
        }
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);
