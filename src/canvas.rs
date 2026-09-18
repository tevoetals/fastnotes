//! Desenho por software num buffer ARGB8888 (little-endian: B, G, R, A).

pub use cosmic_text::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect { x, y, w: (r - x).max(0), h: (b - y).max(0) }
    }
}

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::rgb(r, g, b)
}
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::rgba(r, g, b, a)
}

pub struct Canvas<'a> {
    buf: &'a mut [u8],
    pub w: i32,
    pub h: i32,
    clip: Rect,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u8], w: i32, h: i32) -> Canvas<'a> {
        Canvas { buf, w, h, clip: Rect::new(0, 0, w, h) }
    }

    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(&Rect::new(0, 0, self.w, self.h));
    }
    pub fn reset_clip(&mut self) {
        self.clip = Rect::new(0, 0, self.w, self.h);
    }

    pub fn fill(&mut self, c: Color) {
        // O buffer é XRGB (alfa ignorado): fundo preto vira um memset.
        if c.r() == 0 && c.g() == 0 && c.b() == 0 {
            self.buf.fill(0);
            return;
        }
        let px = [c.b(), c.g(), c.r(), 255];
        for chunk in self.buf.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    #[inline]
    fn blend_at(&mut self, idx: usize, c: Color, alpha: u32) {
        let d = &mut self.buf[idx..idx + 4];
        if alpha >= 255 {
            d[0] = c.b();
            d[1] = c.g();
            d[2] = c.r();
            d[3] = 255;
            return;
        }
        if alpha == 0 {
            return;
        }
        let inv = 255 - alpha;
        d[0] = ((c.b() as u32 * alpha + d[0] as u32 * inv) / 255) as u8;
        d[1] = ((c.g() as u32 * alpha + d[1] as u32 * inv) / 255) as u8;
        d[2] = ((c.r() as u32 * alpha + d[2] as u32 * inv) / 255) as u8;
        d[3] = 255;
    }

    /// Retângulo com mistura de alpha, recortado pelo clip.
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let r = Rect::new(x, y, w, h).intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let alpha = c.a() as u32;
        if alpha == 0 {
            return;
        }
        let stride = self.w as usize * 4;
        if alpha >= 255 {
            let px = [c.b(), c.g(), c.r(), 255];
            for yy in r.y..r.bottom() {
                let start = yy as usize * stride + r.x as usize * 4;
                for chunk in self.buf[start..start + r.w as usize * 4].chunks_exact_mut(4) {
                    chunk.copy_from_slice(&px);
                }
            }
        } else {
            for yy in r.y..r.bottom() {
                for xx in r.x..r.right() {
                    self.blend_at(yy as usize * stride + xx as usize * 4, c, alpha);
                }
            }
        }
    }

    /// Máscara de cobertura 8 bits (glifo) pintada com `c`: recorte calculado
    /// uma vez por glifo e laço interno sem chamadas.
    pub fn blit_mask(&mut self, x: i32, y: i32, w: u32, h: u32, mask: &[u8], c: Color) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 || mask.len() < (w * h) as usize {
            return;
        }
        let ca = c.a() as u32;
        if ca == 0 {
            return;
        }
        let (cr, cg, cb) = (c.r() as u32, c.g() as u32, c.b() as u32);
        let stride = self.w as usize * 4;
        for yy in r.y..r.bottom() {
            let mrow = ((yy - y) as u32 * w + (r.x - x) as u32) as usize;
            let drow = yy as usize * stride + r.x as usize * 4;
            let src = &mask[mrow..mrow + r.w as usize];
            let dst = &mut self.buf[drow..drow + r.w as usize * 4];
            for (m, d) in src.iter().zip(dst.chunks_exact_mut(4)) {
                let a = if ca == 255 { *m as u32 } else { (*m as u32 * ca) / 255 };
                if a == 0 {
                    continue;
                }
                if a >= 255 {
                    d[0] = cb as u8;
                    d[1] = cg as u8;
                    d[2] = cr as u8;
                    d[3] = 255;
                } else {
                    let inv = 255 - a;
                    d[0] = ((cb * a + d[0] as u32 * inv) / 255) as u8;
                    d[1] = ((cg * a + d[1] as u32 * inv) / 255) as u8;
                    d[2] = ((cr * a + d[2] as u32 * inv) / 255) as u8;
                    d[3] = 255;
                }
            }
        }
    }

    /// Bitmap RGBA (alfa não pré-multiplicado, p.ex. emoji colorido) misturado no canvas.
    pub fn blit_rgba(&mut self, x: i32, y: i32, w: u32, h: u32, rgba: &[u8]) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 || rgba.len() < (w * h * 4) as usize {
            return;
        }
        let stride = self.w as usize * 4;
        for yy in r.y..r.bottom() {
            let srow = (((yy - y) as u32 * w + (r.x - x) as u32) * 4) as usize;
            let drow = yy as usize * stride + r.x as usize * 4;
            let src = &rgba[srow..srow + r.w as usize * 4];
            let dst = &mut self.buf[drow..drow + r.w as usize * 4];
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                let a = s[3] as u32;
                if a == 0 {
                    continue;
                }
                if a >= 255 {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 255;
                } else {
                    let inv = 255 - a;
                    d[0] = ((s[2] as u32 * a + d[0] as u32 * inv) / 255) as u8;
                    d[1] = ((s[1] as u32 * a + d[1] as u32 * inv) / 255) as u8;
                    d[2] = ((s[0] as u32 * a + d[2] as u32 * inv) / 255) as u8;
                    d[3] = 255;
                }
            }
        }
    }

    /// Retângulo com cantos arredondados e antialiasing nos cantos.
    pub fn rounded_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, c: Color) {
        let r = radius.clamp(0, (w.min(h) / 2).max(0));
        if r == 0 {
            self.rect(x, y, w, h, c);
            return;
        }
        // Miolo: linhas inteiras sem cantos.
        self.rect(x + r, y, w - 2 * r, h, c);
        self.rect(x, y + r, r, h - 2 * r, c);
        self.rect(x + w - r, y + r, r, h - 2 * r, c);
        let alpha = c.a() as f32;
        let stride = self.w as usize * 4;
        let centers = [
            (x + r, y + r),
            (x + w - r - 1, y + r),
            (x + r, y + h - r - 1),
            (x + w - r - 1, y + h - r - 1),
        ];
        let corners = [
            Rect::new(x, y, r, r),
            Rect::new(x + w - r, y, r, r),
            Rect::new(x, y + h - r, r, r),
            Rect::new(x + w - r, y + h - r, r, r),
        ];
        for (corner, (cx, cy)) in corners.iter().zip(centers) {
            let cr = corner.intersect(&self.clip);
            for yy in cr.y..cr.bottom() {
                for xx in cr.x..cr.right() {
                    let dx = (xx - cx) as f32;
                    let dy = (yy - cy) as f32;
                    let d = (dx * dx + dy * dy).sqrt();
                    let cov = (r as f32 + 0.5 - d).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let a = (alpha * cov) as u32;
                        self.blend_at(yy as usize * stride + xx as usize * 4, c, a);
                    }
                }
            }
        }
    }

    /// Contorno arredondado de 1px (desenha o cheio e recorta o interior).
    pub fn rounded_outline(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, c: Color, fill: Color) {
        self.rounded_rect(x, y, w, h, radius, c);
        self.rounded_rect(x + 1, y + 1, w - 2, h - 2, (radius - 1).max(0), fill);
    }

    /// Copia um bitmap RGBA com mistura de alpha, recortado pelo clip.
    pub fn blit(&mut self, x: i32, y: i32, w: u32, h: u32, rgba: &[u8]) {
        let r = Rect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let stride = self.w as usize * 4;
        for yy in r.y..r.bottom() {
            let sy = (yy - y) as usize;
            for xx in r.x..r.right() {
                let sx = (xx - x) as usize;
                let i = (sy * w as usize + sx) * 4;
                let a = rgba[i + 3] as u32;
                if a == 0 {
                    continue;
                }
                let c = Color::rgb(rgba[i], rgba[i + 1], rgba[i + 2]);
                self.blend_at(yy as usize * stride + xx as usize * 4, c, a);
            }
        }
    }

    /// Triângulo preenchido (setas de toggle).
    pub fn triangle(&mut self, pts: [(i32, i32); 3], c: Color) {
        let ymin = pts.iter().map(|p| p.1).min().unwrap_or(0);
        let ymax = pts.iter().map(|p| p.1).max().unwrap_or(0);
        for y in ymin..=ymax {
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..3 {
                let (x0, y0) = pts[i];
                let (x1, y1) = pts[(i + 1) % 3];
                if (y >= y0.min(y1)) && (y <= y0.max(y1)) && y0 != y1 {
                    let t = (y - y0) as f32 / (y1 - y0) as f32;
                    xs.push(x0 as f32 + t * (x1 - x0) as f32);
                }
            }
            if xs.len() >= 2 {
                let a = xs.iter().cloned().fold(f32::INFINITY, f32::min).round() as i32;
                let b = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max).round() as i32;
                self.rect(a, y, (b - a).max(1), 1, c);
            }
        }
    }

    /// Linha grossa simples (para o "×").
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, c: Color) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = x0 as f32 + (x1 - x0) as f32 * t;
            let y = y0 as f32 + (y1 - y0) as f32 * t;
            self.rect(x.round() as i32 - thick / 2, y.round() as i32 - thick / 2, thick, thick, c);
        }
    }
}
