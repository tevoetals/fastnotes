//! Desenho vetorial no estilo do Excalidraw, guardado na própria nota:
//!
//! ````text
//! ```desenho 320
//! ret 1 40 30 160 80 c=4dabf7 f=1f3b57 p=sol "Início"
//! seta 3 200,70 320,70 de=1 para=2
//! ```
//! ````
//!
//! A cerca diz a altura do desenho (em unidades: 1 unidade = 1 px no zoom
//! padrão). Cada linha é um elemento: tipo, id, geometria e só as
//! propriedades diferentes do padrão; o texto (rótulo) vem por último, entre
//! aspas. Fora do Fast Notes o bloco aparece como código, legível.
//!
//! Este módulo tem o modelo, a leitura/gravação, a geometria (contornos,
//! toques, alças, setas grudadas nas formas), a rasterização por software
//! (máscaras com antialiasing, sem GPU) e o editor (ferramentas e arrastes).
//! Texto e a barra de ferramentas são desenhados por `main.rs`.

use crate::canvas::{Canvas, Rect, rgba};

pub const FENCE: &str = "```desenho";
pub const DEFAULT_H: f32 = 320.0;
pub const MIN_H: f32 = 80.0;
/// Folga abaixo do elemento mais baixo quando o desenho cresce sozinho.
const GROW_PAD: f32 = 24.0;
/// Tolerância de toque (unidades) além da metade da espessura do traço.
const HIT_TOL: f32 = 6.0;
/// Distância em que a ponta de uma seta gruda numa forma.
const BIND_DIST: f32 = 12.0;
/// Folga entre a ponta da seta e a borda da forma.
const BIND_GAP: f32 = 5.0;
const HANDLE: f32 = 8.0;

// ---------------------------------------------------------------------------
// Modelo
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Rect,
    Diamond,
    Ellipse,
    Triangle,
    Line,
    Arrow,
    Free,
    Text,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Rect => "ret",
            Kind::Diamond => "los",
            Kind::Ellipse => "eli",
            Kind::Triangle => "tri",
            Kind::Line => "lin",
            Kind::Arrow => "seta",
            Kind::Free => "lap",
            Kind::Text => "txt",
        }
    }
    fn from_tag(s: &str) -> Option<Kind> {
        Some(match s {
            "ret" => Kind::Rect,
            "los" => Kind::Diamond,
            "eli" => Kind::Ellipse,
            "tri" => Kind::Triangle,
            "lin" => Kind::Line,
            "seta" => Kind::Arrow,
            "lap" => Kind::Free,
            "txt" => Kind::Text,
            _ => return None,
        })
    }
    /// Forma com caixa (x, y, w, h): retângulo, losango, elipse, triângulo.
    pub fn is_box(self) -> bool {
        matches!(self, Kind::Rect | Kind::Diamond | Kind::Ellipse | Kind::Triangle)
    }
    /// Forma feita de pontos: linha, seta, lápis.
    pub fn is_path(self) -> bool {
        matches!(self, Kind::Line | Kind::Arrow | Kind::Free)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fill {
    Hachure,
    Cross,
    Solid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dash {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Head {
    None,
    Arrow,
    Triangle,
    Bar,
    Dot,
}

impl Head {
    pub const ALL: [Head; 5] = [Head::None, Head::Arrow, Head::Triangle, Head::Bar, Head::Dot];
    fn tag(self) -> &'static str {
        match self {
            Head::None => "nada",
            Head::Arrow => "seta",
            Head::Triangle => "tri",
            Head::Bar => "barra",
            Head::Dot => "ponto",
        }
    }
    fn from_tag(s: &str) -> Head {
        match s {
            "seta" => Head::Arrow,
            "tri" => Head::Triangle,
            "barra" => Head::Bar,
            "ponto" => Head::Dot,
            _ => Head::None,
        }
    }
}

/// Paleta (tema escuro): cores do traço; o fundo usa as mesmas, mais suaves.
pub const COLORS: [u32; 8] = [0xffffff, 0xff6b6b, 0x51cf66, 0x4dabf7, 0xff922b, 0xffd43b, 0xcc5de8, 0x868e96];
pub const WIDTHS: [f32; 3] = [1.0, 2.0, 4.0];
pub const FONTS: [f32; 4] = [16.0, 20.0, 28.0, 36.0];
pub const OPACITIES: [u8; 4] = [100, 75, 50, 25];

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub stroke: u32,
    pub bg: Option<u32>,
    pub fill: Fill,
    pub width: f32,
    pub dash: Dash,
    pub round: bool,
    pub opacity: u8,
    pub font: f32,
    pub start: Head,
    pub end: Head,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            stroke: 0xffffff,
            bg: None,
            fill: Fill::Hachure,
            width: 2.0,
            dash: Dash::Solid,
            round: true,
            opacity: 100,
            font: 20.0,
            start: Head::None,
            end: Head::None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct El {
    pub id: u32,
    pub kind: Kind,
    /// Caixa (formas e texto: canto superior esquerdo; tamanho pode ser
    /// negativo enquanto o elemento é criado).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Pontos absolutos (linha, seta, lápis).
    pub pts: Vec<(f32, f32)>,
    /// Rótulo de forma/seta ou o conteúdo de um texto.
    pub text: String,
    pub st: Style,
    /// Formas a que as pontas (início, fim) da seta estão grudadas.
    pub bind: [Option<u32>; 2],
}

impl El {
    fn new(id: u32, kind: Kind, x: f32, y: f32, st: Style) -> El {
        El { id, kind, x, y, w: 0.0, h: 0.0, pts: Vec::new(), text: String::new(), st, bind: [None, None] }
    }

    /// Caixa normalizada (x0, y0, x1, y1).
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        if self.kind.is_path() {
            let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(x, y) in &self.pts {
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
            if self.pts.is_empty() {
                return (0.0, 0.0, 0.0, 0.0);
            }
            return b;
        }
        if self.kind == Kind::Text {
            let (w, h) = text_size(&self.text, self.st.font);
            return (self.x, self.y, self.x + w, self.y + h);
        }
        let (x0, x1) = if self.w < 0.0 { (self.x + self.w, self.x) } else { (self.x, self.x + self.w) };
        let (y0, y1) = if self.h < 0.0 { (self.y + self.h, self.y) } else { (self.y, self.y + self.h) };
        (x0, y0, x1, y1)
    }

    fn normalize(&mut self) {
        if self.kind.is_box() {
            let (x0, y0, x1, y1) = self.bounds();
            (self.x, self.y, self.w, self.h) = (x0, y0, x1 - x0, y1 - y0);
        }
    }

    fn translate(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        for p in &mut self.pts {
            p.0 += dx;
            p.1 += dy;
        }
    }
}

/// Tamanho aproximado de um texto (a medida exata é de `main.rs`; esta
/// serve para seleção e toques).
pub fn text_size(text: &str, font: f32) -> (f32, f32) {
    let lines: Vec<&str> = text.split('\n').collect();
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1);
    (cols as f32 * font * 0.56, lines.len().max(1) as f32 * font * 1.3)
}

#[derive(Clone, PartialEq, Debug)]
pub struct Doc {
    pub height: f32,
    pub els: Vec<El>,
}

impl Default for Doc {
    fn default() -> Doc {
        Doc { height: DEFAULT_H, els: Vec::new() }
    }
}

impl Doc {
    pub fn get(&self, id: u32) -> Option<&El> {
        self.els.iter().find(|e| e.id == id)
    }
    fn get_mut(&mut self, id: u32) -> Option<&mut El> {
        self.els.iter_mut().find(|e| e.id == id)
    }
    fn next_id(&self) -> u32 {
        self.els.iter().map(|e| e.id).max().unwrap_or(0) + 1
    }

    /// Cresce para caber o elemento mais baixo (nunca encolhe sozinho).
    fn grow(&mut self) {
        let bottom = self.els.iter().map(|e| e.bounds().3).fold(0.0, f32::max);
        if bottom + GROW_PAD > self.height {
            self.height = (bottom + GROW_PAD).round();
        }
    }

    /// Reposiciona as pontas grudadas de todas as setas e linhas.
    pub fn fix_bindings(&mut self) {
        let shapes: Vec<El> = self.els.iter().filter(|e| e.kind.is_box() || e.kind == Kind::Text).cloned().collect();
        for el in self.els.iter_mut().filter(|e| matches!(e.kind, Kind::Arrow | Kind::Line)) {
            for end in 0..2 {
                let Some(target) = el.bind[end] else { continue };
                let Some(shape) = shapes.iter().find(|s| s.id == target) else {
                    el.bind[end] = None;
                    continue;
                };
                let n = el.pts.len();
                if n < 2 {
                    continue;
                }
                let (i, j) = if end == 0 { (0, 1) } else { (n - 1, n - 2) };
                // A ponta vizinha também pode estar grudada: mira o centro dela.
                let other = if n == 2 {
                    match el.bind[1 - end].and_then(|o| shapes.iter().find(|s| s.id == o)) {
                        Some(o) => center(o),
                        None => el.pts[j],
                    }
                } else {
                    el.pts[j]
                };
                if let Some(p) = boundary_point(shape, other, BIND_GAP) {
                    el.pts[i] = p;
                }
            }
        }
    }

    // ---------- texto ----------

    pub fn parse(fence: &str, body: &[&str]) -> Doc {
        let height = fence.trim().strip_prefix(FENCE).and_then(|r| r.trim().parse::<f32>().ok()).filter(|h| *h >= MIN_H).unwrap_or(DEFAULT_H);
        let els = body.iter().filter_map(|l| parse_el(l)).collect();
        Doc { height, els }
    }

    pub fn fence(&self) -> String {
        format!("{FENCE} {}", self.height.round())
    }

    pub fn body(&self) -> Vec<String> {
        self.els.iter().map(write_el).collect()
    }
}

fn hex(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

fn num(v: f32) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r}") }
}

fn write_el(e: &El) -> String {
    let mut s = format!("{} {}", e.kind.tag(), e.id);
    if e.kind.is_path() {
        for &(x, y) in &e.pts {
            s.push_str(&format!(" {},{}", num(x), num(y)));
        }
    } else if e.kind == Kind::Text {
        s.push_str(&format!(" {} {}", num(e.x), num(e.y)));
    } else {
        s.push_str(&format!(" {} {} {} {}", num(e.x), num(e.y), num(e.w), num(e.h)));
    }
    let d = Style::default();
    let st = &e.st;
    if st.stroke != d.stroke {
        s.push_str(&format!(" c={:06x}", st.stroke));
    }
    let boxy = e.kind.is_box();
    if let Some(bg) = st.bg.filter(|_| boxy) {
        s.push_str(&format!(" f={bg:06x}"));
    }
    if st.fill != d.fill && boxy && st.bg.is_some() {
        s.push_str(match st.fill {
            Fill::Hachure => " p=hach",
            Fill::Cross => " p=cruz",
            Fill::Solid => " p=sol",
        });
    }
    if st.width != d.width && e.kind != Kind::Text {
        s.push_str(&format!(" e={}", num(st.width)));
    }
    if st.dash != d.dash && e.kind != Kind::Text {
        s.push_str(match st.dash {
            Dash::Solid => " t=cont",
            Dash::Dashed => " t=trac",
            Dash::Dotted => " t=pont",
        });
    }
    if st.round != d.round && matches!(e.kind, Kind::Rect | Kind::Diamond | Kind::Line | Kind::Arrow) {
        s.push_str(if st.round { " r=1" } else { " r=0" });
    }
    if st.opacity != d.opacity {
        s.push_str(&format!(" o={}", st.opacity));
    }
    if st.font != d.font && (e.kind == Kind::Text || !e.text.is_empty()) {
        s.push_str(&format!(" fs={}", num(st.font)));
    }
    if e.kind.is_path() {
        let def_end = if e.kind == Kind::Arrow { Head::Arrow } else { Head::None };
        if st.start != Head::None {
            s.push_str(&format!(" ini={}", st.start.tag()));
        }
        if st.end != def_end {
            s.push_str(&format!(" fim={}", st.end.tag()));
        }
    }
    if let Some(b) = e.bind[0] {
        s.push_str(&format!(" de={b}"));
    }
    if let Some(b) = e.bind[1] {
        s.push_str(&format!(" para={b}"));
    }
    if !e.text.is_empty() {
        let q: String = e.text.chars().flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' => vec!['\\', 'n'],
            c => vec![c],
        }).collect();
        s.push_str(&format!(" \"{q}\""));
    }
    s
}

fn parse_el(line: &str) -> Option<El> {
    let line = line.trim();
    // Texto entre aspas no fim da linha.
    let (head, text) = match line.find(" \"") {
        Some(i) => {
            let raw = line[i + 2..].strip_suffix('"').unwrap_or(&line[i + 2..]);
            let mut t = String::new();
            let mut it = raw.chars();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some('n') => t.push('\n'),
                        Some(o) => t.push(o),
                        None => {}
                    }
                } else {
                    t.push(c);
                }
            }
            (&line[..i], t)
        }
        None => (line, String::new()),
    };
    let mut toks = head.split_whitespace();
    let kind = Kind::from_tag(toks.next()?)?;
    let id: u32 = toks.next()?.parse().ok()?;
    let mut el = El::new(id, kind, 0.0, 0.0, Style::default());
    if kind == Kind::Arrow {
        el.st.end = Head::Arrow;
    }
    el.text = text;
    let mut nums = Vec::new();
    for t in toks {
        if let Some((k, v)) = t.split_once('=') {
            match k {
                "c" => el.st.stroke = hex(v).unwrap_or(el.st.stroke),
                "f" => el.st.bg = hex(v),
                "p" => {
                    el.st.fill = match v {
                        "cruz" => Fill::Cross,
                        "sol" => Fill::Solid,
                        _ => Fill::Hachure,
                    }
                }
                "e" => el.st.width = v.parse().unwrap_or(2.0f32).clamp(0.5, 16.0),
                "t" => {
                    el.st.dash = match v {
                        "trac" => Dash::Dashed,
                        "pont" => Dash::Dotted,
                        _ => Dash::Solid,
                    }
                }
                "r" => el.st.round = v != "0",
                "o" => el.st.opacity = v.parse().unwrap_or(100u8).clamp(5, 100),
                "fs" => el.st.font = v.parse().unwrap_or(20.0f32).clamp(6.0, 200.0),
                "ini" => el.st.start = Head::from_tag(v),
                "fim" => el.st.end = Head::from_tag(v),
                "de" => el.bind[0] = v.parse().ok(),
                "para" => el.bind[1] = v.parse().ok(),
                _ => {}
            }
        } else if let Some((a, b)) = t.split_once(',') {
            el.pts.push((a.parse().ok()?, b.parse().ok()?));
        } else {
            nums.push(t.parse::<f32>().ok()?);
        }
    }
    match kind {
        k if k.is_path() => {
            if el.pts.len() < 2 {
                return None;
            }
        }
        Kind::Text => {
            el.x = *nums.first()?;
            el.y = *nums.get(1)?;
        }
        _ => {
            if nums.len() < 4 {
                return None;
            }
            (el.x, el.y, el.w, el.h) = (nums[0], nums[1], nums[2], nums[3]);
        }
    }
    Some(el)
}

// ---------------------------------------------------------------------------
// Geometria
// ---------------------------------------------------------------------------

type P = (f32, f32);

fn center(e: &El) -> P {
    let (x0, y0, x1, y1) = e.bounds();
    ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
}

fn dist_seg(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - qx).powi(2) + (p.1 - qy).powi(2)).sqrt()
}

fn inside_poly(p: P, poly: &[P]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    inside
}

/// Contorno fechado de uma forma de caixa (pontos em unidades).
fn box_outline(e: &El) -> Vec<P> {
    let (x0, y0, x1, y1) = e.bounds();
    let (w, h) = (x1 - x0, y1 - y0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    match e.kind {
        Kind::Rect if e.st.round && w > 4.0 && h > 4.0 => {
            let r = (w.min(h) * 0.25).min(24.0);
            let mut v = Vec::new();
            let corners = [(x1 - r, y0 + r, -90.0f32), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)];
            for (ccx, ccy, a0) in corners {
                for k in 0..=6 {
                    let a = (a0 + 15.0 * k as f32).to_radians();
                    v.push((ccx + r * a.cos(), ccy + r * a.sin()));
                }
            }
            v
        }
        Kind::Rect => vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
        Kind::Diamond => vec![(cx, y0), (x1, cy), (cx, y1), (x0, cy)],
        Kind::Triangle => vec![(cx, y0), (x1, y1), (x0, y1)],
        Kind::Ellipse => {
            let n = 64;
            (0..n)
                .map(|k| {
                    let a = k as f32 / n as f32 * std::f32::consts::TAU;
                    (cx + w / 2.0 * a.cos(), cy + h / 2.0 * a.sin())
                })
                .collect()
        }
        _ => vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
    }
}

/// Curva suave (Catmull-Rom) passando pelos pontos.
fn smooth(pts: &[P]) -> Vec<P> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        for s in 0..10 {
            let t = s as f32 / 10.0;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| 0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3);
            out.push((f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    out.push(*pts.last().expect("3+ pontos"));
    out
}

/// Caminho desenhado de uma linha/seta (curva se `round` e 3+ pontos).
fn path_points(e: &El) -> Vec<P> {
    if e.kind != Kind::Free && e.st.round && e.pts.len() > 2 { smooth(&e.pts) } else { e.pts.clone() }
}

/// Ponto da borda de `shape` na reta de `from` até o centro dela, recuado `gap`.
fn boundary_point(shape: &El, from: P, gap: f32) -> Option<P> {
    let c = center(shape);
    let poly = if shape.kind == Kind::Text {
        let (x0, y0, x1, y1) = shape.bounds();
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    } else {
        box_outline(shape)
    };
    if inside_poly(from, &poly) {
        return None;
    }
    let mut best: Option<(f32, P)> = None;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if let Some(t) = seg_intersect(from, c, a, b) {
            let p = (from.0 + (c.0 - from.0) * t, from.1 + (c.1 - from.1) * t);
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, p));
            }
        }
    }
    let (_, p) = best?;
    let (dx, dy) = (from.0 - p.0, from.1 - p.1);
    let l = (dx * dx + dy * dy).sqrt().max(0.001);
    Some((p.0 + dx / l * gap, p.1 + dy / l * gap))
}

/// Parâmetro t (0..1 em p→q) do cruzamento dos segmentos p-q e a-b.
fn seg_intersect(p: P, q: P, a: P, b: P) -> Option<f32> {
    let r = (q.0 - p.0, q.1 - p.1);
    let s = (b.0 - a.0, b.1 - a.1);
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-6 {
        return None;
    }
    let t = ((a.0 - p.0) * s.1 - (a.1 - p.1) * s.0) / den;
    let u = ((a.0 - p.0) * r.1 - (a.1 - p.1) * r.0) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// O ponto `p` (unidades) toca o elemento?
pub fn hit(e: &El, p: P) -> bool {
    let tol = HIT_TOL + e.st.width / 2.0;
    match e.kind {
        Kind::Text => {
            let (x0, y0, x1, y1) = e.bounds();
            p.0 >= x0 - 4.0 && p.0 <= x1 + 4.0 && p.1 >= y0 - 4.0 && p.1 <= y1 + 4.0
        }
        k if k.is_path() => {
            let pts = path_points(e);
            pts.windows(2).any(|w| dist_seg(p, w[0], w[1]) <= tol) || (pts.len() == 1 && dist_seg(p, pts[0], pts[0]) <= tol)
        }
        _ => {
            let poly = box_outline(e);
            if (e.st.bg.is_some() || !e.text.is_empty()) && inside_poly(p, &poly) {
                return true;
            }
            let n = poly.len();
            (0..n).any(|i| dist_seg(p, poly[i], poly[(i + 1) % n]) <= tol)
        }
    }
}

/// Elemento de cima sob o ponto.
pub fn topmost(doc: &Doc, p: P) -> Option<u32> {
    doc.els.iter().rev().find(|e| hit(e, p)).map(|e| e.id)
}

/// Como `topmost`, mas o miolo de formas sem fundo também conta (duplo
/// clique dentro de uma forma edita o rótulo dela).
fn container_at(doc: &Doc, p: P) -> Option<u32> {
    topmost(doc, p).or_else(|| doc.els.iter().rev().find(|e| e.kind.is_box() && inside_poly(p, &box_outline(e))).map(|e| e.id))
}

/// Alças de um elemento selecionado: (índice, ponto). Caixas: 0..8 nos cantos
/// e lados (sentido horário a partir do canto superior esquerdo); caminhos:
/// um por ponto (lápis: nenhuma).
pub fn handles(e: &El) -> Vec<(usize, P)> {
    match e.kind {
        Kind::Free => Vec::new(),
        k if k.is_path() => e.pts.iter().copied().enumerate().collect(),
        _ => {
            let (x0, y0, x1, y1) = e.bounds();
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let pad = if e.kind == Kind::Text { 4.0 } else { 0.0 };
            let (x0, y0, x1, y1) = (x0 - pad, y0 - pad, x1 + pad, y1 + pad);
            let all = vec![(0, (x0, y0)), (1, (cx, y0)), (2, (x1, y0)), (3, (x1, cy)), (4, (x1, y1)), (5, (cx, y1)), (6, (x0, y1)), (7, (x0, cy))];
            if e.kind == Kind::Text { all.into_iter().filter(|(i, _)| i % 2 == 0).collect() } else { all }
        }
    }
}

// ---------------------------------------------------------------------------
// Rasterização: máscaras de cobertura (u8) com antialiasing
// ---------------------------------------------------------------------------

/// Máscara de cobertura numa região da tela.
struct Mask {
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    a: Vec<u8>,
}

impl Mask {
    fn new(r: Rect) -> Mask {
        Mask { x0: r.x, y0: r.y, w: r.w.max(0), h: r.h.max(0), a: vec![0; (r.w.max(0) * r.h.max(0)) as usize] }
    }

    /// Segmento grosso (cápsula) de raio `rad` com borda suave.
    fn seg(&mut self, a: P, b: P, rad: f32) {
        let pad = rad + 1.0;
        let xa = ((a.0.min(b.0) - pad).floor() as i32).max(self.x0);
        let xb = ((a.0.max(b.0) + pad).ceil() as i32).min(self.x0 + self.w - 1);
        let ya = ((a.1.min(b.1) - pad).floor() as i32).max(self.y0);
        let yb = ((a.1.max(b.1) + pad).ceil() as i32).min(self.y0 + self.h - 1);
        for y in ya..=yb {
            let row = ((y - self.y0) * self.w) as usize;
            for x in xa..=xb {
                let d = dist_seg((x as f32 + 0.5, y as f32 + 0.5), a, b);
                let cov = (rad + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let i = row + (x - self.x0) as usize;
                    self.a[i] = self.a[i].max((cov * 255.0) as u8);
                }
            }
        }
    }

    fn polyline(&mut self, pts: &[P], rad: f32, closed: bool) {
        for w in pts.windows(2) {
            self.seg(w[0], w[1], rad);
        }
        if closed && pts.len() > 2 {
            self.seg(pts[pts.len() - 1], pts[0], rad);
        }
        if pts.len() == 1 {
            self.seg(pts[0], pts[0], rad);
        }
    }

    /// Polígono cheio (par-ímpar), com `pattern` decidindo os pixels internos.
    fn fill(&mut self, poly: &[P], pattern: impl Fn(i32, i32) -> bool) {
        let n = poly.len();
        if n < 3 {
            return;
        }
        let ya = (poly.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor() as i32).max(self.y0);
        let yb = (poly.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i32).min(self.y0 + self.h - 1);
        let mut xs = Vec::new();
        for y in ya..=yb {
            let fy = y as f32 + 0.5;
            xs.clear();
            for i in 0..n {
                let (a, b) = (poly[i], poly[(i + 1) % n]);
                if (a.1 <= fy) != (b.1 <= fy) {
                    xs.push(a.0 + (fy - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
            xs.sort_by(f32::total_cmp);
            let row = ((y - self.y0) * self.w) as usize;
            for pair in xs.chunks_exact(2) {
                let xa = (pair[0].round() as i32).max(self.x0);
                let xb = (pair[1].round() as i32).min(self.x0 + self.w);
                for x in xa..xb {
                    if pattern(x, y) {
                        self.a[row + (x - self.x0) as usize] = 255;
                    }
                }
            }
        }
    }

    fn draw(&self, canvas: &mut Canvas, color: u32, alpha: u8) {
        canvas.blit_mask(self.x0, self.y0, self.w as u32, self.h as u32, &self.a, rgba((color >> 16) as u8, (color >> 8) as u8, color as u8, alpha));
    }
}

/// Divide um caminho em traços (tracejado/pontilhado).
fn dashes(pts: &[P], on: f32, off: f32) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut draw = true;
    let mut left = on;
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut seg = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        while seg > 0.0 {
            let step = left.min(seg);
            let t = step / seg;
            let m = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            if draw {
                if cur.is_empty() {
                    cur.push(a);
                }
                cur.push(m);
            }
            seg -= step;
            left -= step;
            a = m;
            if left <= 0.0 {
                if draw {
                    out.push(std::mem::take(&mut cur));
                }
                draw = !draw;
                left = if draw { on } else { off };
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Texto a desenhar por `main.rs` (coordenadas de tela).
pub struct TextItem {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: u32,
    pub alpha: u8,
    pub text: String,
    /// Centralizado em (x, y) em vez de começar no canto (x, y).
    pub center: bool,
    pub id: u32,
}

/// Desenha o documento com origem (ox, oy) na tela e escala `k` (px por unidade).
/// Os textos voltam em `texts` para `main.rs` desenhar.
pub fn render(doc: &Doc, canvas: &mut Canvas, ox: f32, oy: f32, k: f32, texts: &mut Vec<TextItem>, skip_text: Option<u32>) {
    for e in &doc.els {
        render_el(e, canvas, ox, oy, k, texts, skip_text);
    }
}

pub fn render_el(e: &El, canvas: &mut Canvas, ox: f32, oy: f32, k: f32, texts: &mut Vec<TextItem>, skip_text: Option<u32>) {
    let to = |p: P| (ox + p.0 * k, oy + p.1 * k);
    let alpha = (e.st.opacity as u32 * 255 / 100) as u8;
    let rad = (e.st.width * k / 2.0).max(0.6);
    let (bx0, by0, bx1, by1) = e.bounds();
    let pad = (e.st.width * 4.0 + 16.0) * k;
    let area = Rect::new((ox + bx0 * k - pad) as i32, (oy + by0 * k - pad) as i32, ((bx1 - bx0) * k + 2.0 * pad) as i32 + 1, ((by1 - by0) * k + 2.0 * pad) as i32 + 1).intersect(&canvas.clip());
    if e.kind != Kind::Text && (area.w <= 0 || area.h <= 0) {
        return;
    }
    let stroke_path = |m: &mut Mask, pts: &[P], closed: bool| {
        let mut pts: Vec<P> = pts.iter().map(|&p| to(p)).collect();
        if closed && pts.len() > 2 {
            pts.push(pts[0]);
        }
        match e.st.dash {
            Dash::Solid => m.polyline(&pts, rad, false),
            Dash::Dashed => {
                for d in dashes(&pts, rad * 2.0 * 4.0 + 4.0 * k, rad * 2.0 * 3.0 + 4.0 * k) {
                    m.polyline(&d, rad, false);
                }
            }
            Dash::Dotted => {
                for d in dashes(&pts, 0.01, rad * 2.0 * 2.5 + 2.0 * k) {
                    m.seg(d[0], d[0], rad);
                }
            }
        }
    };
    match e.kind {
        Kind::Text => {}
        k2 if k2.is_box() => {
            let poly = box_outline(e);
            if let Some(bg) = e.st.bg {
                let mut m = Mask::new(area);
                let sp = (8.0 * k).max(4.0) as i32;
                let t = (1.5 * k).max(1.0) as i32;
                let sc: Vec<P> = poly.iter().map(|&p| to(p)).collect();
                match e.st.fill {
                    Fill::Solid => m.fill(&sc, |_, _| true),
                    Fill::Hachure => m.fill(&sc, |x, y| (x + y).rem_euclid(sp) < t),
                    Fill::Cross => m.fill(&sc, |x, y| (x + y).rem_euclid(sp) < t || (x - y).rem_euclid(sp) < t),
                }
                // Fundo sólido mais suave, para traço e rótulo continuarem legíveis.
                let a = if e.st.fill == Fill::Solid { (alpha as u32 * 45 / 100) as u8 } else { alpha };
                m.draw(canvas, bg, a);
            }
            let mut m = Mask::new(area);
            stroke_path(&mut m, &poly, true);
            m.draw(canvas, e.st.stroke, alpha);
        }
        _ => {
            let pts = path_points(e);
            let mut m = Mask::new(area);
            stroke_path(&mut m, &pts, false);
            // Pontas.
            let n = pts.len();
            if n >= 2 {
                let tip = |m: &mut Mask, head: Head, at: P, from: P| {
                    let (a, b) = (to(at), to(from));
                    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
                    let l = (dx * dx + dy * dy).sqrt().max(0.001);
                    let (ux, uy) = (dx / l, dy / l);
                    let size = (e.st.width * 3.0 + 10.0) * k;
                    let wing = |ang: f32| {
                        let (c, s) = (ang.cos(), ang.sin());
                        (a.0 - size * (ux * c - uy * s), a.1 - size * (ux * s + uy * c))
                    };
                    match head {
                        Head::None => {}
                        Head::Arrow => {
                            m.seg(a, wing(0.45), rad);
                            m.seg(a, wing(-0.45), rad);
                        }
                        Head::Triangle => {
                            let tri = [a, wing(0.4), wing(-0.4)];
                            m.fill(&tri, |_, _| true);
                            m.polyline(&tri, rad, true);
                        }
                        Head::Bar => {
                            let h = size * 0.6;
                            m.seg((a.0 - uy * h, a.1 + ux * h), (a.0 + uy * h, a.1 - ux * h), rad);
                        }
                        Head::Dot => {
                            let r = (e.st.width * 1.5 + 3.0) * k;
                            m.seg(a, a, r);
                        }
                    }
                };
                // Direção da ponta: um pouco antes do fim (curvas).
                let back = |idx: usize, dir: isize| -> P {
                    let p = pts[idx];
                    let mut j = idx as isize;
                    loop {
                        j += dir;
                        if j < 0 || j as usize >= n {
                            return pts[(idx as isize + dir).clamp(0, n as isize - 1) as usize];
                        }
                        let q = pts[j as usize];
                        if ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt() > 8.0 {
                            return q;
                        }
                    }
                };
                tip(&mut m, e.st.end, pts[n - 1], back(n - 1, -1));
                tip(&mut m, e.st.start, pts[0], back(0, 1));
            }
            m.draw(canvas, e.st.stroke, alpha);
        }
    }
    if !e.text.is_empty() && skip_text != Some(e.id) {
        let (x, y, center) = match e.kind {
            Kind::Text => (e.x, e.y, false),
            k2 if k2.is_path() => {
                let pts = path_points(e);
                let mid = pts.get(pts.len() / 2).copied().unwrap_or((e.x, e.y));
                (mid.0, mid.1, true)
            }
            _ => {
                let c = center_of(e);
                (c.0, c.1, true)
            }
        };
        let s = to((x, y));
        texts.push(TextItem { x: s.0, y: s.1, size: e.st.font * k, color: e.st.stroke, alpha, text: e.text.clone(), center, id: e.id });
    }
}

fn center_of(e: &El) -> P {
    center(e)
}

/// Ícone de uma ferramenta, desenhado no retângulo `r` da tela.
pub fn icon(canvas: &mut Canvas, tool: Tool, r: Rect, color: u32) {
    let (x0, y0) = (r.x as f32 + r.w as f32 * 0.25, r.y as f32 + r.h as f32 * 0.25);
    let s = r.w.min(r.h) as f32 * 0.5;
    let p = |u: f32, v: f32| (x0 + u * s, y0 + v * s);
    let mut m = Mask::new(r);
    let rad = (s / 18.0).max(0.7);
    match tool {
        Tool::Select => {
            let tri = [p(0.1, 0.0), p(0.85, 0.55), p(0.45, 0.6), p(0.3, 1.0)];
            m.fill(&tri, |_, _| true);
        }
        Tool::Rect => m.polyline(&[p(0.0, 0.15), p(1.0, 0.15), p(1.0, 0.85), p(0.0, 0.85), p(0.0, 0.15)], rad, false),
        Tool::Diamond => m.polyline(&[p(0.5, 0.0), p(1.0, 0.5), p(0.5, 1.0), p(0.0, 0.5), p(0.5, 0.0)], rad, false),
        Tool::Ellipse => {
            let pts: Vec<P> = (0..=32).map(|i| {
                let a = i as f32 / 32.0 * std::f32::consts::TAU;
                p(0.5 + 0.5 * a.cos(), 0.5 + 0.42 * a.sin())
            }).collect();
            m.polyline(&pts, rad, false);
        }
        Tool::Triangle => m.polyline(&[p(0.5, 0.05), p(1.0, 0.95), p(0.0, 0.95), p(0.5, 0.05)], rad, false),
        Tool::Arrow => {
            m.seg(p(0.0, 1.0), p(1.0, 0.0), rad);
            m.seg(p(1.0, 0.0), p(0.55, 0.05), rad);
            m.seg(p(1.0, 0.0), p(0.95, 0.45), rad);
        }
        Tool::Line => m.seg(p(0.0, 1.0), p(1.0, 0.0), rad),
        Tool::Free => {
            let pts: Vec<P> = (0..=24).map(|i| {
                let t = i as f32 / 24.0;
                p(t, 0.5 + 0.35 * (t * 9.0).sin() * (1.0 - t * 0.4))
            }).collect();
            m.polyline(&pts, rad, false);
        }
        Tool::Text => {
            m.seg(p(0.1, 0.05), p(0.9, 0.05), rad * 1.2);
            m.seg(p(0.5, 0.05), p(0.5, 1.0), rad * 1.2);
        }
        Tool::Lock => {
            let body = [p(0.12, 0.45), p(0.88, 0.45), p(0.88, 1.0), p(0.12, 1.0)];
            m.fill(&body, |_, _| true);
            let arc: Vec<P> = (0..=16).map(|i| {
                let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                p(0.5 + 0.26 * a.cos(), 0.42 + 0.36 * a.sin())
            }).collect();
            m.polyline(&arc, rad, false);
        }
        Tool::Eraser => {
            let b = [p(0.35, 0.05), p(1.0, 0.55), p(0.65, 0.95), p(0.0, 0.45)];
            m.polyline(&[b[0], b[1], b[2], b[3], b[0]], rad, false);
            m.seg(p(0.18, 0.25), p(0.82, 0.75), rad);
        }
    }
    m.draw(canvas, color, 255);
}

/// Amostra de estilo para os botões de propriedade (espessura, traço, pontas…).
pub fn sample(canvas: &mut Canvas, r: Rect, color: u32, st: &Style, kind: Kind) {
    let k = r.h as f32 / 28.0;
    let el = match kind {
        k2 if k2.is_box() => {
            let mut e = El::new(0, kind, 7.0, 7.0, *st);
            e.w = r.w as f32 / k - 14.0;
            e.h = 14.0;
            e
        }
        _ => {
            let mut e = El::new(0, kind, 0.0, 0.0, *st);
            e.pts = vec![(6.0, 14.0), (r.w as f32 / k - 6.0, 14.0)];
            e
        }
    };
    let mut el = el;
    el.st.stroke = color;
    el.st.opacity = st.opacity;
    let mut t = Vec::new();
    render_el(&el, canvas, r.x as f32, r.y as f32, k, &mut t, None);
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Rect,
    Diamond,
    Ellipse,
    Triangle,
    Arrow,
    Line,
    Free,
    Text,
    Eraser,
    /// Só o ícone do cadeado (Q) na barra; não é uma ferramenta.
    Lock,
}

impl Tool {
    /// Ordem da barra, com o atalho de letra e de número (como no Excalidraw).
    pub const BAR: [(Tool, &'static str, char, char); 10] = [
        (Tool::Select, "Seleção", 'v', '1'),
        (Tool::Rect, "Retângulo", 'r', '2'),
        (Tool::Diamond, "Losango", 'd', '3'),
        (Tool::Ellipse, "Elipse", 'o', '4'),
        (Tool::Arrow, "Seta", 'a', '5'),
        (Tool::Line, "Linha", 'l', '6'),
        (Tool::Free, "Lápis", 'p', '7'),
        (Tool::Text, "Texto", 't', '8'),
        (Tool::Triangle, "Triângulo", 'g', '9'),
        (Tool::Eraser, "Borracha", 'e', '0'),
    ];
    fn kind(self) -> Option<Kind> {
        Some(match self {
            Tool::Rect => Kind::Rect,
            Tool::Diamond => Kind::Diamond,
            Tool::Ellipse => Kind::Ellipse,
            Tool::Triangle => Kind::Triangle,
            Tool::Arrow => Kind::Arrow,
            Tool::Line => Kind::Line,
            Tool::Free => Kind::Free,
            Tool::Text => Kind::Text,
            _ => return None,
        })
    }
}

enum Drag {
    None,
    /// Criando o elemento `id` a partir do ponto inicial.
    Create { id: u32, start: P },
    /// Movendo a seleção; cópias originais para mover sem acumular erro.
    Move { start: P, orig: Vec<El>, moved: bool },
    /// Redimensionando pela alça `h`.
    Resize { id: u32, h: usize, orig: El },
    /// Movendo o ponto `i` de um caminho.
    Point { id: u32, i: usize },
    /// Caixa de seleção.
    Marquee { a: P, b: P, add: Vec<u32> },
    Erase,
    /// Altura do desenho (borda de baixo).
    Height { start: f32, h0: f32 },
}

/// Resultado de um evento para `main.rs`.
#[derive(Default, Clone, Copy)]
pub struct Out {
    /// Redesenhar.
    pub redraw: bool,
    /// O documento mudou de verdade: gravar na nota (uma alteração desfazível).
    pub commit: bool,
}

pub struct Editor {
    /// Linha da cerca ```desenho na página.
    pub line: usize,
    pub doc: Doc,
    pub tool: Tool,
    /// Q: a ferramenta continua ativa depois de desenhar.
    pub lock: bool,
    pub sel: Vec<u32>,
    /// Estilo dos próximos elementos (e o mostrado na barra sem seleção).
    pub style: Style,
    /// Texto sendo digitado (id do elemento).
    pub typing: Option<u32>,
    drag: Drag,
    /// Ponteiro (unidades), para o cursor e a prévia da borracha.
    pub hover: P,
}

impl Editor {
    pub fn new(line: usize, doc: Doc) -> Editor {
        Editor { line, doc, tool: Tool::Select, lock: false, sel: Vec::new(), style: Style::default(), typing: None, drag: Drag::None, hover: (0.0, 0.0) }
    }

    pub fn dragging(&self) -> bool {
        !matches!(self.drag, Drag::None)
    }

    pub fn set_tool(&mut self, t: Tool) -> Out {
        let mut out = self.finish_typing();
        self.tool = t;
        if t != Tool::Select {
            self.sel.clear();
        }
        out.redraw = true;
        out
    }

    /// Estilo mostrado na barra: o do primeiro selecionado, senão o atual.
    pub fn shown_style(&self) -> Style {
        self.sel.first().and_then(|id| self.doc.get(*id)).map(|e| e.st).unwrap_or(self.style)
    }

    /// Tipo para decidir quais propriedades aparecem na barra.
    pub fn shown_kinds(&self) -> Vec<Kind> {
        if self.sel.is_empty() {
            return self.tool.kind().into_iter().collect();
        }
        self.sel.iter().filter_map(|id| self.doc.get(*id)).map(|e| e.kind).collect()
    }

    /// Muda uma propriedade na seleção (e no estilo dos próximos).
    pub fn apply_style(&mut self, f: impl Fn(&mut Style)) -> Out {
        f(&mut self.style);
        let mut changed = false;
        for id in self.sel.clone() {
            if let Some(e) = self.doc.get_mut(id) {
                let before = e.st;
                f(&mut e.st);
                changed |= e.st != before;
            }
        }
        if changed {
            self.doc.fix_bindings();
        }
        Out { redraw: true, commit: changed }
    }

    pub fn press(&mut self, p: P, shift: bool, clicks: u32) -> Out {
        self.hover = p;
        let mut out = Out { redraw: true, commit: false };
        // Borda de baixo: altura do desenho.
        if (p.1 - self.doc.height).abs() <= 6.0 && self.typing.is_none() {
            self.drag = Drag::Height { start: p.1, h0: self.doc.height };
            return out;
        }
        if let Some(id) = self.typing {
            // Clique fora do texto em edição: termina.
            if !self.doc.get(id).is_some_and(|e| hit(e, p)) {
                out = self.finish_typing();
                out.redraw = true;
            } else {
                return out;
            }
        }
        match self.tool {
            Tool::Select => {
                // Duplo clique: rótulo da forma/seta, ou texto novo no vazio.
                if clicks >= 2 {
                    if let Some(id) = container_at(&self.doc, p) {
                        let e = self.doc.get(id).expect("existe");
                        if e.kind != Kind::Free {
                            self.sel = vec![id];
                            self.typing = Some(id);
                            return out;
                        }
                    } else {
                        return self.new_text(p);
                    }
                }
                // Alça do único selecionado.
                if let [id] = self.sel[..] {
                    if let Some(e) = self.doc.get(id) {
                        if let Some((h, _)) = handles(e).into_iter().find(|(_, q)| (q.0 - p.0).abs() <= HANDLE && (q.1 - p.1).abs() <= HANDLE) {
                            self.drag = if e.kind.is_path() { Drag::Point { id, i: h } } else { Drag::Resize { id, h, orig: e.clone() } };
                            return out;
                        }
                    }
                }
                match topmost(&self.doc, p) {
                    Some(id) => {
                        if shift {
                            if let Some(i) = self.sel.iter().position(|s| *s == id) {
                                self.sel.remove(i);
                                return out;
                            }
                            self.sel.push(id);
                        } else if !self.sel.contains(&id) {
                            self.sel = vec![id];
                        }
                        let orig = self.sel.iter().filter_map(|i| self.doc.get(*i)).cloned().collect();
                        self.drag = Drag::Move { start: p, orig, moved: false };
                    }
                    None => {
                        let add = if shift { self.sel.clone() } else { Vec::new() };
                        self.sel = add.clone();
                        self.drag = Drag::Marquee { a: p, b: p, add };
                    }
                }
            }
            Tool::Eraser => {
                self.drag = Drag::Erase;
                out.commit |= self.erase_at(p);
            }
            Tool::Text => {
                return self.new_text(p);
            }
            t => {
                let kind = t.kind().expect("ferramenta de forma");
                let id = self.doc.next_id();
                let mut st = self.style;
                if kind == Kind::Arrow && st.end == Head::None && st.start == Head::None {
                    st.end = Head::Arrow;
                }
                if kind == Kind::Free {
                    st.round = false;
                }
                let mut e = El::new(id, kind, p.0, p.1, st);
                if kind.is_path() {
                    e.pts = vec![p, p];
                    if kind != Kind::Free {
                        e.bind[0] = self.bind_target(p, None);
                    }
                }
                self.doc.els.push(e);
                self.sel = vec![id];
                self.drag = Drag::Create { id, start: p };
            }
        }
        out
    }

    fn new_text(&mut self, p: P) -> Out {
        let id = self.doc.next_id();
        let mut e = El::new(id, Kind::Text, p.0, p.1 - self.style.font * 0.65, self.style);
        e.st.bg = None;
        self.doc.els.push(e);
        self.sel = vec![id];
        self.typing = Some(id);
        Out { redraw: true, commit: false }
    }

    /// Forma (não seta/linha) onde a ponta em `p` grudaria.
    fn bind_target(&self, p: P, except: Option<u32>) -> Option<u32> {
        self.doc
            .els
            .iter()
            .rev()
            .filter(|e| (e.kind.is_box() || e.kind == Kind::Text) && Some(e.id) != except)
            .find(|e| {
                let (x0, y0, x1, y1) = e.bounds();
                p.0 >= x0 - BIND_DIST && p.0 <= x1 + BIND_DIST && p.1 >= y0 - BIND_DIST && p.1 <= y1 + BIND_DIST
            })
            .map(|e| e.id)
    }

    pub fn motion(&mut self, p: P, shift: bool) -> Out {
        self.hover = p;
        let mut out = Out { redraw: self.tool == Tool::Eraser || !matches!(self.drag, Drag::None), commit: false };
        match &mut self.drag {
            Drag::None => {}
            Drag::Height { start, h0 } => {
                let bottom = self.doc.els.iter().map(|e| e.bounds().3).fold(0.0, f32::max);
                self.doc.height = (*h0 + p.1 - *start).max(MIN_H).max(bottom + 8.0).round();
            }
            Drag::Create { id, start } => {
                let (id, start) = (*id, *start);
                let target = self.bind_target(p, None);
                let Some(e) = self.doc.get_mut(id) else { return out };
                match e.kind {
                    Kind::Free => {
                        if e.pts.last().is_none_or(|q| (q.0 - p.0).abs() + (q.1 - p.1).abs() >= 1.5) {
                            e.pts.push(p);
                        }
                    }
                    Kind::Line | Kind::Arrow => {
                        let mut q = p;
                        if shift {
                            q = snap45(start, p);
                        }
                        let n = e.pts.len();
                        e.pts[n - 1] = q;
                        e.bind[1] = if target != e.bind[0] || e.bind[0].is_none() { target } else { None };
                    }
                    _ => {
                        let (mut w, mut h) = (p.0 - start.0, p.1 - start.1);
                        if shift {
                            let s = w.abs().max(h.abs());
                            w = s * w.signum();
                            h = s * h.signum();
                        }
                        e.w = w;
                        e.h = h;
                    }
                }
                self.doc.fix_bindings();
            }
            Drag::Move { start, orig, moved } => {
                let (mut dx, mut dy) = (p.0 - start.0, p.1 - start.1);
                if shift {
                    if dx.abs() > dy.abs() { dy = 0.0 } else { dx = 0.0 }
                }
                if dx.abs() + dy.abs() > 0.5 {
                    *moved = true;
                }
                let ids: Vec<u32> = orig.iter().map(|e| e.id).collect();
                for o in orig.iter() {
                    if let Some(e) = self.doc.els.iter_mut().find(|e| e.id == o.id) {
                        *e = o.clone();
                        e.translate(dx, dy);
                        // Seta movida sozinha (sem as formas): solta das formas.
                        if e.kind.is_path() {
                            for b in &mut e.bind {
                                if b.is_some_and(|t| !ids.contains(&t)) && *moved {
                                    *b = None;
                                }
                            }
                        }
                    }
                }
                self.doc.fix_bindings();
            }
            Drag::Resize { id, h, orig } => {
                let (id, h) = (*id, *h);
                let o = orig.clone();
                let Some(e) = self.doc.get_mut(id) else { return out };
                let (mut x0, mut y0, mut x1, mut y1) = o.bounds();
                match h {
                    0 => (x0, y0) = p,
                    1 => y0 = p.1,
                    2 => (x1, y0) = (p.0, p.1),
                    3 => x1 = p.0,
                    4 => (x1, y1) = p,
                    5 => y1 = p.1,
                    6 => (x0, y1) = (p.0, p.1),
                    _ => x0 = p.0,
                }
                if shift && h % 2 == 0 && o.w.abs() > 0.1 {
                    // Proporção mantida pelos cantos.
                    let ratio = o.h.abs() / o.w.abs().max(0.1);
                    let w = x1 - x0;
                    let hh = w.abs() * ratio * if y1 >= y0 { 1.0 } else { -1.0 };
                    if h == 0 || h == 2 { y0 = y1 - hh } else { y1 = y0 + hh }
                }
                if e.kind == Kind::Text {
                    // Texto: os cantos mudam o tamanho da fonte.
                    let (_, oh) = text_size(&o.text, o.st.font);
                    let nh = (y1 - y0).abs().max(6.0);
                    e.st.font = (o.st.font * nh / oh.max(1.0)).clamp(8.0, 160.0).round();
                    e.x = x0.min(x1);
                    e.y = y0.min(y1);
                } else {
                    (e.x, e.y, e.w, e.h) = (x0, y0, x1 - x0, y1 - y0);
                }
                self.doc.fix_bindings();
            }
            Drag::Point { id, i } => {
                let (id, i) = (*id, *i);
                let target = self.bind_target(p, Some(id));
                let Some(e) = self.doc.get_mut(id) else { return out };
                let n = e.pts.len();
                let mut q = p;
                if shift && n >= 2 {
                    let anchor = if i == 0 { e.pts[1] } else { e.pts[i - 1] };
                    q = snap45(anchor, p);
                }
                e.pts[i] = q;
                if i == 0 {
                    e.bind[0] = target;
                } else if i == n - 1 {
                    e.bind[1] = target;
                }
                self.doc.fix_bindings();
            }
            Drag::Marquee { a, b, add } => {
                *b = p;
                let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
                let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
                let mut sel = add.clone();
                for e in &self.doc.els {
                    let (ex0, ey0, ex1, ey1) = e.bounds();
                    if ex0 >= x0 && ex1 <= x1 && ey0 >= y0 && ey1 <= y1 && !sel.contains(&e.id) {
                        sel.push(e.id);
                    }
                }
                self.sel = sel;
            }
            Drag::Erase => out.commit |= self.erase_at(p),
        }
        out
    }

    pub fn release(&mut self, _p: P) -> Out {
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        let mut out = Out { redraw: true, commit: false };
        match drag {
            Drag::Create { id, .. } => {
                let keep = self.doc.get(id).is_some_and(|e| {
                    let (x0, y0, x1, y1) = e.bounds();
                    (x1 - x0) + (y1 - y0) >= 3.0 || e.kind == Kind::Free
                });
                if keep {
                    if let Some(e) = self.doc.get_mut(id) {
                        e.normalize();
                        if e.kind == Kind::Free && e.pts.len() > 2 {
                            e.pts = simplify(&e.pts, 0.8);
                        }
                    }
                    self.doc.fix_bindings();
                    out.commit = true;
                    if !self.lock {
                        self.tool = Tool::Select;
                    } else {
                        self.sel.clear();
                    }
                } else {
                    self.doc.els.retain(|e| e.id != id);
                    self.sel.clear();
                }
            }
            Drag::Move { moved, .. } => out.commit = moved,
            Drag::Resize { id, .. } => {
                if let Some(e) = self.doc.get_mut(id) {
                    e.normalize();
                }
                self.doc.fix_bindings();
                out.commit = true;
            }
            Drag::Point { .. } | Drag::Height { .. } => out.commit = true,
            Drag::Erase => out.commit = true,
            Drag::Marquee { .. } | Drag::None => {}
        }
        if out.commit {
            self.doc.grow();
        }
        out
    }

    fn erase_at(&mut self, p: P) -> bool {
        match topmost(&self.doc, p) {
            Some(id) => {
                self.remove(&[id]);
                true
            }
            None => false,
        }
    }

    fn remove(&mut self, ids: &[u32]) {
        self.doc.els.retain(|e| !ids.contains(&e.id));
        for e in &mut self.doc.els {
            for b in &mut e.bind {
                if b.is_some_and(|t| ids.contains(&t)) {
                    *b = None;
                }
            }
        }
        self.sel.retain(|s| !ids.contains(s));
    }

    pub fn delete_selection(&mut self) -> Out {
        if self.sel.is_empty() {
            return Out::default();
        }
        let ids = self.sel.clone();
        self.remove(&ids);
        Out { redraw: true, commit: true }
    }

    pub fn select_all(&mut self) -> Out {
        self.tool = Tool::Select;
        self.sel = self.doc.els.iter().map(|e| e.id).collect();
        Out { redraw: true, commit: false }
    }

    pub fn duplicate(&mut self) -> Out {
        if self.sel.is_empty() {
            return Out::default();
        }
        let mut next = self.doc.next_id();
        let mut map = Vec::new();
        let mut new = Vec::new();
        for id in &self.sel {
            if let Some(e) = self.doc.get(*id) {
                let mut c = e.clone();
                map.push((c.id, next));
                c.id = next;
                next += 1;
                c.translate(16.0, 16.0);
                new.push(c);
            }
        }
        // Setas copiadas junto com as formas continuam grudadas nas cópias.
        for c in &mut new {
            for b in &mut c.bind {
                *b = b.and_then(|t| map.iter().find(|(o, _)| *o == t).map(|(_, n)| *n));
            }
        }
        self.sel = new.iter().map(|e| e.id).collect();
        self.doc.els.extend(new);
        self.doc.fix_bindings();
        self.doc.grow();
        Out { redraw: true, commit: true }
    }

    /// Camadas: `front` para frente (ou para trás); `all` até o fim.
    pub fn reorder(&mut self, front: bool, all: bool) -> Out {
        if self.sel.is_empty() {
            return Out::default();
        }
        let before: Vec<u32> = self.doc.els.iter().map(|e| e.id).collect();
        let (mut picked, mut rest): (Vec<El>, Vec<El>) = self.doc.els.drain(..).partition(|e| self.sel.contains(&e.id));
        if all {
            self.doc.els = if front { rest.into_iter().chain(picked).collect() } else { picked.into_iter().chain(rest).collect() };
        } else {
            // Um passo: cada selecionado troca com o vizinho não selecionado.
            let mut all_els: Vec<El> = before.iter().filter_map(|id| picked.iter().position(|e| e.id == *id).map(|i| picked[i].clone()).or_else(|| rest.iter().position(|e| e.id == *id).map(|i| rest[i].clone()))).collect();
            let n = all_els.len();
            let sel = self.sel.clone();
            if front {
                for i in (0..n.saturating_sub(1)).rev() {
                    if sel.contains(&all_els[i].id) && !sel.contains(&all_els[i + 1].id) {
                        all_els.swap(i, i + 1);
                    }
                }
            } else {
                for i in 1..n {
                    if sel.contains(&all_els[i].id) && !sel.contains(&all_els[i - 1].id) {
                        all_els.swap(i, i - 1);
                    }
                }
            }
            picked.clear();
            rest.clear();
            self.doc.els = all_els;
        }
        let after: Vec<u32> = self.doc.els.iter().map(|e| e.id).collect();
        Out { redraw: true, commit: after != before }
    }

    pub fn nudge(&mut self, dx: f32, dy: f32) -> Out {
        if self.sel.is_empty() {
            return Out::default();
        }
        for id in self.sel.clone() {
            if let Some(e) = self.doc.get_mut(id) {
                e.translate(dx, dy);
            }
        }
        self.doc.fix_bindings();
        self.doc.grow();
        Out { redraw: true, commit: true }
    }

    /// Esc: termina o texto, limpa a seleção ou (devolve `false`) sai do desenho.
    pub fn escape(&mut self) -> (bool, Out) {
        if self.typing.is_some() {
            return (true, self.finish_typing());
        }
        if self.dragging() {
            self.drag = Drag::None;
            return (true, Out { redraw: true, commit: false });
        }
        if !self.sel.is_empty() || self.tool != Tool::Select {
            self.sel.clear();
            self.tool = Tool::Select;
            return (true, Out { redraw: true, commit: false });
        }
        (false, Out::default())
    }

    // ---------- texto ----------

    pub fn type_str(&mut self, s: &str) -> Out {
        let Some(id) = self.typing else { return Out::default() };
        if let Some(e) = self.doc.get_mut(id) {
            e.text.push_str(s);
        }
        Out { redraw: true, commit: false }
    }

    pub fn backspace(&mut self) -> Out {
        let Some(id) = self.typing else { return Out::default() };
        if let Some(e) = self.doc.get_mut(id) {
            e.text.pop();
        }
        Out { redraw: true, commit: false }
    }

    /// Termina a digitação: texto vazio some; senão grava.
    pub fn finish_typing(&mut self) -> Out {
        let Some(id) = self.typing.take() else { return Out::default() };
        if self.doc.get(id).is_some_and(|e| e.kind == Kind::Text && e.text.trim().is_empty()) {
            self.remove(&[id]);
            return Out { redraw: true, commit: false };
        }
        if let Some(e) = self.doc.get_mut(id) {
            e.text = e.text.trim_end_matches('\n').to_string();
        }
        self.doc.fix_bindings();
        self.doc.grow();
        Out { redraw: true, commit: true }
    }

    /// Caixa da seleção de marquee (para desenhar), se houver.
    pub fn marquee(&self) -> Option<(P, P)> {
        match &self.drag {
            Drag::Marquee { a, b, .. } => Some((*a, *b)),
            _ => None,
        }
    }
}

fn snap45(a: P, p: P) -> P {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let l = (dx * dx + dy * dy).sqrt();
    let ang = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
    (a.0 + l * ang.cos(), a.1 + l * ang.sin())
}

/// Simplificação de Ramer–Douglas–Peucker (traço do lápis com menos pontos).
fn simplify(pts: &[P], eps: f32) -> Vec<P> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (mut idx, mut dmax) = (0, 0.0);
    for (i, &p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = dist_seg(p, a, b);
        if d > dmax {
            (idx, dmax) = (i, d);
        }
    }
    if dmax > eps {
        let mut l = simplify(&pts[..=idx], eps);
        let r = simplify(&pts[idx..], eps);
        l.pop();
        l.extend(r);
        l
    } else {
        vec![a, b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let body = [
            r#"ret 1 40 30 160 80 c=4dabf7 f=1f3b57 p=sol "Início \"x\"""#,
            "eli 2 300 30 120 80 t=trac",
            "seta 3 200,70 300,70 de=1 para=2",
            "lap 4 10,10 12,14 20,18",
            r#"txt 5 50 200 fs=28 "Olá\nmundo""#,
            "tri 6 10 10 40 40 o=50",
        ];
        let doc = Doc::parse("```desenho 400", &body);
        assert_eq!(doc.height, 400.0);
        assert_eq!(doc.els.len(), 6);
        assert_eq!(doc.els[0].text, "Início \"x\"");
        assert_eq!(doc.els[2].st.end, Head::Arrow);
        assert_eq!(doc.els[2].bind, [Some(1), Some(2)]);
        assert_eq!(doc.els[4].text, "Olá\nmundo");
        let again = Doc::parse(&doc.fence(), &doc.body().iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(doc, again);
        assert_eq!(doc.body()[2], "seta 3 200,70 300,70 de=1 para=2");
    }

    #[test]
    fn bindings_follow_shapes() {
        let mut doc = Doc::parse("```desenho", &["ret 1 0 0 100 100", "ret 2 300 0 100 100", "seta 3 100,50 300,50 de=1 para=2"]);
        doc.fix_bindings();
        let a = &doc.els[2];
        assert!((a.pts[0].0 - (100.0 + BIND_GAP)).abs() < 0.5, "{:?}", a.pts);
        assert!((a.pts[1].0 - (300.0 - BIND_GAP)).abs() < 0.5, "{:?}", a.pts);
        doc.els[1].translate(0.0, 200.0);
        doc.fix_bindings();
        let a = &doc.els[2];
        assert!(a.pts[1].1 > 150.0, "seta segue a forma: {:?}", a.pts);
    }

    #[test]
    fn editing_flow() {
        let mut ed = Editor::new(0, Doc::default());
        ed.set_tool(Tool::Rect);
        ed.press((10.0, 10.0), false, 1);
        ed.motion((110.0, 60.0), false);
        let out = ed.release((110.0, 60.0));
        assert!(out.commit);
        assert_eq!(ed.tool, Tool::Select);
        assert_eq!(ed.doc.els[0].bounds(), (10.0, 10.0, 110.0, 60.0));
        // Seta do retângulo até o vazio, depois uma elipse onde ela termina.
        ed.set_tool(Tool::Arrow);
        ed.press((112.0, 35.0), false, 1);
        ed.motion((250.0, 35.0), false);
        ed.release((250.0, 35.0));
        assert_eq!(ed.doc.els[1].bind[0], Some(1));
        // Mover o retângulo arrasta a ponta da seta.
        ed.set_tool(Tool::Select);
        ed.press((50.0, 10.0), false, 1);
        ed.motion((50.0, 110.0), false);
        assert!(ed.release((50.0, 110.0)).commit);
        assert!(ed.doc.els[1].pts[0].1 > 60.0);
        // Apagar.
        ed.sel = vec![1];
        ed.delete_selection();
        assert_eq!(ed.doc.els.len(), 1);
        assert_eq!(ed.doc.els[0].bind[0], None);
        assert!(simplify(&[(0.0, 0.0), (1.0, 0.01), (2.0, 0.0)], 0.5).len() == 2);
    }
}
