//! Análise de Markdown linha a linha, para renderização "ao vivo": o texto
//! continua sendo Markdown puro no arquivo, mas cada trecho recebe estilo
//! (negrito, itálico, código, riscado, cor…) e alguns marcadores são
//! escondidos para que decorações (ponto de lista, checkbox, barra de
//! citação, separador, grade de tabela) sejam desenhadas no lugar.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    Text,
    Heading(u8),
    Bullet,
    Numbered,
    Task(bool),
    Quote,
    Rule,
    /// `<br>` sozinho na linha: meio espaço vertical (Shift+Enter).
    Space,
    Fence,
    Code,
    Table,
    TableSep,
    /// Linha `▾ Título` (aberto) ou `▸ Título` (fechado); o conteúdo são as linhas recuadas abaixo.
    Toggle(bool),
    /// `:::` que abre um bloco de colunas.
    ColStart,
    /// `|||` separando colunas.
    ColSep,
    /// `:::` que fecha o bloco de colunas.
    ColEnd,
    /// `![alt](caminho)` sozinho na linha.
    Image,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Flags {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub mono: bool,
    pub strike: bool,
    pub underline: bool,
    pub marker: bool,
    pub hidden: bool,
    pub dim: bool,
    /// URL de um link: só aparece na linha em edição.
    pub url: bool,
    /// Marcador de cor `{#hex ` / `}`: nunca aparece.
    pub cmarker: bool,
    /// Cor 0xRRGGBB pedida com `{#f00 texto}` ou `{vermelho texto}`.
    pub color: Option<u32>,
    /// O `!` de `![alt](caminho)`: vira o espaço reservado da imagem em linha.
    pub img: bool,
}

pub struct LineInfo {
    pub block: Block,
    pub spans: Vec<(Range<usize>, Flags)>,
    /// Índices dos `>` (ocultos; barras desenhadas no lugar).
    pub quotes: Vec<usize>,
    /// Índice do marcador de lista (oculto; ponto desenhado no lugar).
    pub bullet: Option<usize>,
    /// Índice do `[` do checkbox e se está marcado.
    pub checkbox: Option<(usize, bool)>,
    /// Índices dos `|` (ocultos; linhas verticais desenhadas no lugar).
    pub pipes: Vec<usize>,
    /// Linha de cabeçalho de tabela (a que precede `|---|`).
    pub header: bool,
    /// Índice do `▸`/`▾` de um toggle.
    pub toggle_idx: Option<usize>,
    /// Escondida por um toggle fechado.
    pub folded: bool,
    /// Dentro de um bloco de colunas: (linha do `:::` inicial, índice da coluna).
    pub col: Option<(usize, u8)>,
    /// Caminho da imagem (`Block::Image`).
    pub image: Option<String>,
    /// Imagens em linha (`![alt](caminho =300x)`), na ordem do texto.
    pub images: Vec<InlineImage>,
    /// Linha termina com `\` (quebra dura do CommonMark): a próxima linha é
    /// da mesma "família" e fica mais perto (Shift+Enter).
    pub hard_break: bool,
}

pub const TOGGLE_OPEN: &str = "▾";
pub const TOGGLE_CLOSED: &str = "▸";
/// Marca do meio espaço (linha inteira).
pub const SPACE_MARK: &str = "<br>";
pub const MAX_COLS: u8 = 4;

impl LineInfo {
    /// Linha estrutural (`:::`, `|||`) ou dobrada: o cursor não deve parar nela.
    pub fn skip_cursor(&self) -> bool {
        self.folded || matches!(self.block, Block::ColStart | Block::ColSep | Block::ColEnd | Block::TableSep)
    }
}

pub fn analyze<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<LineInfo> {
    let lines: Vec<&str> = lines.into_iter().collect();
    let mut out: Vec<LineInfo> = Vec::with_capacity(lines.len());
    let mut in_code = false;
    for line in &lines {
        out.push(analyze_line(line, &mut in_code));
    }
    for i in 1..out.len() {
        if out[i].block == Block::TableSep && out[i - 1].block == Block::Table {
            out[i - 1].header = true;
        }
    }
    // Colunas: `:::` abre, `|||` separa, `:::` fecha.
    let mut start: Option<usize> = None;
    let mut col: u8 = 0;
    for i in 0..out.len() {
        if matches!(out[i].block, Block::Code | Block::Fence) {
            continue;
        }
        let t = lines[i].trim();
        let fence = col_fence(t).is_some();
        match (if fence { ":::" } else { t }, start) {
            (":::", None) => {
                out[i] = structural(lines[i], Block::ColStart);
                start = Some(i);
                col = 0;
            }
            (":::", Some(_)) => {
                out[i] = structural(lines[i], Block::ColEnd);
                start = None;
            }
            ("|||", Some(_)) => {
                out[i] = structural(lines[i], Block::ColSep);
                col = (col + 1).min(MAX_COLS - 1);
            }
            (_, Some(s)) => out[i].col = Some((s, col)),
            _ => {}
        }
    }
    // Toggles fechados escondem as linhas recuadas abaixo.
    let mut i = 0;
    while i < out.len() {
        if let Block::Toggle(open) = out[i].block {
            let indent = leading_ws(lines[i]);
            let mut j = i + 1;
            let mut last = i;
            while j < out.len() && (lines[j].trim().is_empty() || leading_ws(lines[j]) > indent) {
                if !lines[j].trim().is_empty() {
                    last = j;
                }
                j += 1;
            }
            if !open {
                for k in i + 1..=last {
                    out[k].folded = true;
                }
            }
            i = last.max(i) + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Linha `:::` de colunas, com larguras opcionais em porcentagem
/// (`::: 30 70`). Devolve as larguras (vazio = todas iguais).
pub fn col_fence(t: &str) -> Option<Vec<u16>> {
    let rest = t.trim().strip_prefix(":::")?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    if !rest.starts_with(' ') {
        return None;
    }
    rest.split_whitespace().map(|w| w.parse::<u16>().ok().filter(|&n| n > 0)).collect()
}

fn structural(line: &str, block: Block) -> LineInfo {
    let mut info = empty_info();
    info.block = block;
    let mut flags = vec![Flags::default(); line.len()];
    set(&mut flags, 0..line.len(), |f| f.hidden = true);
    info.spans = coalesce(&flags);
    info
}

fn empty_info() -> LineInfo {
    LineInfo {
        block: Block::Text,
        spans: Vec::new(),
        quotes: Vec::new(),
        bullet: None,
        checkbox: None,
        pipes: Vec::new(),
        header: false,
        toggle_idx: None,
        folded: false,
        col: None,
        image: None,
        images: Vec::new(),
        hard_break: false,
    }
}

/// Imagem em linha: intervalo do marcador no texto, caminho e largura pedida.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImage {
    pub start: usize,
    pub end: usize,
    pub path: String,
    pub width: Option<u32>,
}

/// Lê `![alt](caminho)` ou `![alt](caminho =300x)` no início de `s`.
/// Devolve (tamanho do marcador, alt, caminho, largura).
pub fn parse_image(s: &str) -> Option<(usize, &str, &str, Option<u32>)> {
    let inner = s.strip_prefix("![")?;
    let close = inner.find("](")?;
    let alt = &inner[..close];
    if alt.contains('[') || alt.contains('\n') {
        return None;
    }
    let rest = &inner[close + 2..];
    let pclose = rest.find(')')?;
    let target = &rest[..pclose];
    let (path, width) = match target.rfind(" =") {
        Some(k) => {
            let size = &target[k + 2..];
            let digits: String = size.chars().take_while(char::is_ascii_digit).collect();
            let w = digits.parse::<u32>().ok().filter(|_| size[digits.len()..].starts_with('x'));
            if w.is_some() { (&target[..k], w) } else { (target, None) }
        }
        None => (target, None),
    };
    let path = path.trim();
    if path.is_empty() || path.contains(' ') && !path.contains('/') {
        return None;
    }
    Some((2 + close + 2 + pclose + 1, alt, path, width))
}

/// Todas as imagens em linha de `line` (só as com caminho de imagem).
pub fn inline_images(line: &str) -> Vec<InlineImage> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            // pula trechos de código
            if let Some(j) = line[i + 1..].find('`') {
                i += j + 2;
                continue;
            }
        }
        if b[i] == b'!' && b.get(i + 1) == Some(&b'[') {
            if let Some((len, _alt, path, width)) = parse_image(&line[i..]) {
                out.push(InlineImage { start: i, end: i + len, path: path.to_string(), width });
                i += len;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `![alt](caminho)` ocupando a linha inteira → caminho.
fn image_path(rest: &str) -> Option<&str> {
    let t = rest.trim_end();
    let t = t.strip_suffix('\\').map(str::trim_end).unwrap_or(t);
    let (len, _, path, _) = parse_image(t)?;
    (len == t.len()).then_some(path)
}

/// Reescreve os blocos de tabela de um texto com colunas alinhadas.
pub fn format_tables_in_text(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let infos = analyze(lines.iter().copied());
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if matches!(infos[i].block, Block::Table | Block::TableSep) {
            let mut j = i;
            while j < lines.len() && matches!(infos[j].block, Block::Table | Block::TableSep) {
                j += 1;
            }
            out.extend(format_table(&lines[i..j]));
            i = j;
        } else {
            out.push(lines[i].to_string());
            i += 1;
        }
    }
    out.join("\n")
}

/// Analisa uma linha isolada (sem contexto de bloco de código).
#[cfg(test)]
pub fn analyze_one(line: &str) -> LineInfo {
    let mut in_code = false;
    analyze_line(line, &mut in_code)
}

fn analyze_line(line: &str, in_code: &mut bool) -> LineInfo {
    let len = line.len();
    let bytes = line.as_bytes();
    let mut flags = vec![Flags::default(); len];
    let mut info = empty_info();

    let mut pos = leading_ws(line);

    if line[pos..].starts_with("```") {
        info.block = Block::Fence;
        set(&mut flags, pos..len, |f| {
            f.marker = true;
            f.mono = true;
        });
        *in_code = !*in_code;
        info.spans = coalesce(&flags);
        return info;
    }
    if *in_code {
        info.block = Block::Code;
        set(&mut flags, 0..len, |f| f.mono = true);
        info.spans = coalesce(&flags);
        return info;
    }

    // Citações: um ou mais `>`.
    while pos < len && bytes[pos] == b'>' {
        info.quotes.push(pos);
        flags[pos].hidden = true;
        pos += 1;
        if bytes.get(pos) == Some(&b' ') {
            flags[pos].hidden = true;
            pos += 1;
        }
    }
    if !info.quotes.is_empty() {
        info.block = Block::Quote;
    }

    let rest = &line[pos..];
    let mut content = pos;
    // Quebra curta: número ímpar de `\` no fim da linha.
    let hard = rest.trim_end().bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 1;

    if rest.trim_end() == SPACE_MARK {
        info.block = Block::Space;
        set(&mut flags, pos..len, |f| f.marker = true);
        info.spans = coalesce(&flags);
        return info;
    }
    if is_rule(rest) {
        info.block = Block::Rule;
        info.hard_break = hard;
        set(&mut flags, pos..len, |f| f.hidden = true);
        info.spans = coalesce(&flags);
        return info;
    } else if let Some(path) = image_path(rest) {
        // Só imagem na linha: classificada como bloco, mas marcada pelo `inline`.
        info.block = Block::Image;
        info.image = Some(path.to_string());
    } else if rest.starts_with(TOGGLE_OPEN) || rest.starts_with(TOGGLE_CLOSED) {
        let open = rest.starts_with(TOGGLE_OPEN);
        info.block = Block::Toggle(open);
        info.toggle_idx = Some(pos);
        let m = TOGGLE_OPEN.len() + usize::from(rest.as_bytes().get(TOGGLE_OPEN.len()) == Some(&b' '));
        set(&mut flags, pos..pos + m, |f| f.hidden = true);
        content = pos + m;
    } else if let Some(level) = heading_level(rest) {
        info.block = Block::Heading(level);
        let m = level as usize + 1;
        set(&mut flags, pos..pos + m, |f| f.marker = true);
        content = pos + m;
    } else if let Some(checked) = task(rest) {
        info.block = Block::Task(checked);
        info.bullet = Some(pos);
        info.checkbox = Some((pos + 2, checked));
        set(&mut flags, pos..pos + 5, |f| f.hidden = true);
        content = pos + 5;
        if bytes.get(content) == Some(&b' ') {
            content += 1;
        }
        if checked {
            set(&mut flags, content..len, |f| {
                f.dim = true;
                f.strike = true;
            });
        }
    } else if is_bullet(rest) {
        info.block = Block::Bullet;
        info.bullet = Some(pos);
        flags[pos].hidden = true;
        content = pos + 2;
    } else if let Some(n) = numbered_len(rest) {
        // Números ficam sempre visíveis (não são "marcadores" ocultáveis).
        info.block = Block::Numbered;
        content = pos + n;
    } else if rest.starts_with('|') {
        if is_table_sep(rest) {
            info.block = Block::TableSep;
            set(&mut flags, pos..len, |f| {
                f.hidden = true;
                f.mono = true;
            });
            info.pipes = pipes(line, pos);
            info.spans = coalesce(&flags);
            return info;
        }
        info.block = Block::Table;
        set(&mut flags, 0..len, |f| f.mono = true);
        info.pipes = pipes(line, pos);
        for &p in &info.pipes {
            flags[p].hidden = true;
        }
    }

    let content = content.min(len);
    info.images = inline_images(line);
    inline(line, content..len, &mut flags);
    // Quebra dura: número ímpar de `\` no fim (o último é o marcador).
    if info.block != Block::Table && len > content {
        let trailing = bytes[content..].iter().rev().take_while(|&&c| c == b'\\').count();
        if trailing % 2 == 1 {
            info.hard_break = true;
            flags[len - 1] = Flags { marker: true, ..Flags::default() };
        }
    }
    info.spans = coalesce(&flags);
    info
}

/// Alinhamento declarado na linha separadora: `None` (só `---`), 0 esq, 1 centro, 2 dir.
pub fn sep_align(sep: &str) -> Vec<Option<u8>> {
    split_cells(sep)
        .iter()
        .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Some(1),
            (false, true) => Some(2),
            (true, false) => Some(0),
            _ => None,
        })
        .collect()
}

/// Célula numérica (valor, moeda, percentual): alinhada à direita por padrão.
pub fn is_numeric_cell(cell: &str) -> bool {
    let t = strip_colors(cell);
    let t = t.trim().trim_matches(|c| matches!(c, '*' | '_' | '~' | '`'));
    let t = t.trim_start_matches(|c: char| matches!(c, '$' | '€' | '£' | '¥' | 'R' | '+' | '-' | '−' | '(')).trim_start();
    let t = t.trim_end_matches(|c: char| matches!(c, '%' | ')' | '$' | '€' | '£' | '¥')).trim_end();
    !t.is_empty() && t.chars().any(|c| c.is_ascii_digit()) && t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\'' | ':' | '/' | '-'))
}

fn set(flags: &mut [Flags], r: Range<usize>, f: impl Fn(&mut Flags)) {
    let end = r.end.min(flags.len());
    for x in &mut flags[r.start.min(end)..end] {
        f(x);
    }
}

fn leading_ws(s: &str) -> usize {
    s.bytes().take_while(|b| *b == b' ' || *b == b'\t').count()
}

fn is_rule(s: &str) -> bool {
    let t = s.trim();
    // `---\`: régua com quebra curta (Shift+Enter).
    let t = t.strip_suffix('\\').map(str::trim_end).unwrap_or(t);
    if t.len() < 3 {
        return false;
    }
    let c = t.as_bytes()[0];
    (c == b'-' || c == b'*' || c == b'_') && t.bytes().all(|b| b == c || b == b' ')
}

fn heading_level(s: &str) -> Option<u8> {
    let n = s.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&n) && s.as_bytes().get(n) == Some(&b' ') { Some(n as u8) } else { None }
}

fn task(s: &str) -> Option<bool> {
    let b = s.as_bytes();
    if b.len() >= 5 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' && b[2] == b'[' && b[4] == b']' {
        match b[3] {
            b' ' => Some(false),
            b'x' | b'X' => Some(true),
            _ => None,
        }
    } else {
        None
    }
}

fn is_bullet(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' '
}

fn numbered_len(s: &str) -> Option<usize> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > 6 {
        return None;
    }
    let b = s.as_bytes();
    if matches!(b.get(n), Some(b'.') | Some(b')')) && b.get(n + 1) == Some(&b' ') { Some(n + 2) } else { None }
}

fn is_table_sep(s: &str) -> bool {
    let t = s.trim().trim_matches('|');
    !t.is_empty()
        && t.split('|').all(|c| {
            let c = c.trim();
            !c.is_empty() && c.bytes().all(|b| b == b'-' || b == b':') && c.contains('-')
        })
}

fn pipes(line: &str, from: usize) -> Vec<usize> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'|' {
            out.push(i);
        }
        i += 1;
    }
    out
}

// ---------- inline ----------

fn inline(line: &str, range: Range<usize>, flags: &mut [Flags]) {
    let s = &line[range.clone()];
    let base = range.start;
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let mut colors: Vec<(usize, u32)> = Vec::new();

    let mark = |flags: &mut [Flags], r: Range<usize>| {
        for f in &mut flags[base + r.start..base + r.end.min(n)] {
            f.marker = true;
        }
    };

    while i < n {
        let c = b[i];
        let cur = Flags { bold, italic, strike, color: colors.last().map(|c| c.1), ..Flags::default() };

        if c == b'\\' && i + 1 < n {
            flags[base + i].marker = true;
            let l = char_len(b[i + 1]);
            apply(flags, base + i + 1..base + i + 1 + l, cur);
            i += 1 + l;
            continue;
        }
        if c == b'`' {
            if let Some(end) = find_byte(b, i + 1, b'`') {
                mark(flags, i..i + 1);
                for f in &mut flags[base + i + 1..base + end] {
                    f.code = true;
                    f.color = cur.color;
                }
                mark(flags, end..end + 1);
                i = end + 1;
                continue;
            }
        }
        if c == b'*' || c == b'_' {
            let run = b[i..].iter().take_while(|x| **x == c).count().min(3);
            // `_` no meio de uma palavra (snake_case) não é ênfase.
            let intraword = c == b'_'
                && i > 0
                && (b[i - 1] as char).is_alphanumeric()
                && b.get(i + run).is_some_and(|x| (*x as char).is_alphanumeric());
            if intraword {
                apply(flags, base + i..base + i + run, cur);
                i += run;
                continue;
            }
            let mut consumed = false;
            if run >= 2 {
                if bold {
                    bold = false;
                    consumed = true;
                } else if has_run_later(b, i + run, c, 2) {
                    bold = true;
                    consumed = true;
                }
            }
            if run == 1 || run == 3 {
                if italic {
                    italic = false;
                    consumed = true;
                } else if has_run_later(b, i + run, c, 1) {
                    italic = true;
                    consumed = true;
                }
            }
            if consumed {
                mark(flags, i..i + run);
                i += run;
                continue;
            }
            apply(flags, base + i..base + i + run, cur);
            i += run;
            continue;
        }
        if c == b'~' && b.get(i + 1) == Some(&b'~') {
            if strike {
                strike = false;
                mark(flags, i..i + 2);
                i += 2;
                continue;
            } else if has_run_later(b, i + 2, b'~', 2) {
                strike = true;
                mark(flags, i..i + 2);
                i += 2;
                continue;
            }
        }
        if c == b'!' && b.get(i + 1) == Some(&b'[') {
            if let Some((len, _, _, _)) = parse_image(&s[i..]) {
                mark(flags, i..i + len);
                flags[base + i].img = true;
                i += len;
                continue;
            }
        }
        if c == b'[' {
            if let Some(close) = find_matching(b, i, b'[', b']') {
                if b.get(close + 1) == Some(&b'(') {
                    if let Some(pclose) = find_matching(b, close + 1, b'(', b')') {
                        mark(flags, i..i + 1);
                        let mut t = cur;
                        t.underline = true;
                        apply(flags, base + i + 1..base + close, t);
                        mark(flags, close..close + 2);
                        for f in &mut flags[base + close + 2..base + pclose] {
                            f.url = true;
                        }
                        mark(flags, pclose..pclose + 1);
                        i = pclose + 1;
                        continue;
                    }
                }
            }
        }
        if c == b'{' {
            if let Some((color, content_start)) = parse_color_open(&s[i..]) {
                if let Some(close) = find_matching(b, i, b'{', b'}') {
                    for f in &mut flags[base + i..base + i + content_start] {
                        f.cmarker = true;
                    }
                    colors.push((close, color));
                    i += content_start;
                    continue;
                }
            }
        }
        if c == b'}' && colors.last().is_some_and(|(cl, _)| *cl == i) {
            colors.pop();
            flags[base + i].cmarker = true;
            i += 1;
            continue;
        }
        if s[i..].starts_with("http://") || s[i..].starts_with("https://") {
            let end = i + s[i..].find(|ch: char| ch.is_whitespace()).unwrap_or(n - i);
            let mut t = cur;
            t.underline = true;
            apply(flags, base + i..base + end, t);
            i = end;
            continue;
        }
        let l = char_len(c);
        apply(flags, base + i..base + i + l, cur);
        i += l;
    }
}

fn apply(flags: &mut [Flags], r: Range<usize>, cur: Flags) {
    let end = r.end.min(flags.len());
    for f in &mut flags[r.start.min(end)..end] {
        f.bold |= cur.bold;
        f.italic |= cur.italic;
        f.strike |= cur.strike;
        f.underline |= cur.underline;
        if cur.color.is_some() {
            f.color = cur.color;
        }
    }
}

fn char_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn find_byte(b: &[u8], from: usize, c: u8) -> Option<usize> {
    b[from.min(b.len())..].iter().position(|x| *x == c).map(|p| p + from)
}

fn has_run_later(b: &[u8], from: usize, c: u8, need: usize) -> bool {
    let mut i = from.min(b.len());
    while i < b.len() {
        if b[i] == c {
            let run = b[i..].iter().take_while(|x| **x == c).count();
            if run >= need {
                return true;
            }
            i += run;
        } else {
            i += 1;
        }
    }
    false
}

fn find_matching(b: &[u8], open_at: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0;
    let mut i = open_at;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Cor de um marcador de abertura `{#f00 ` / `{vermelho `.
pub fn parse_color_name(open: &str) -> Option<u32> {
    parse_color_open(open).map(|(c, _)| c)
}

/// `{#f00 ` / `{#ff0000 ` / `{vermelho ` → (cor, bytes consumidos até o conteúdo).
fn parse_color_open(s: &str) -> Option<(u32, usize)> {
    let inner = s.strip_prefix('{')?;
    let name_len = inner.find(' ')?;
    let name = &inner[..name_len];
    let color = if let Some(hex) = name.strip_prefix('#') {
        match hex.len() {
            3 => {
                let v = u32::from_str_radix(hex, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                (r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11)
            }
            6 => u32::from_str_radix(hex, 16).ok()?,
            _ => return None,
        }
    } else {
        named_color(name)?
    };
    Some((color, 1 + name_len + 1))
}

fn named_color(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "red" | "vermelho" => 0xff5555,
        "green" | "verde" => 0x55e07a,
        "blue" | "azul" => 0x6ea8ff,
        "yellow" | "amarelo" => 0xffd93d,
        "orange" | "laranja" => 0xffa143,
        "purple" | "roxo" => 0xc58fff,
        "pink" | "rosa" => 0xff7ac6,
        "cyan" | "ciano" => 0x5fe3e3,
        "gray" | "grey" | "cinza" => 0x9a9a9a,
        "white" | "branco" => 0xffffff,
        _ => return None,
    })
}

/// Aplica uma cor de prévia a um intervalo de bytes de uma linha já analisada.
pub fn recolor(spans: &[(Range<usize>, Flags)], len: usize, range: Range<usize>, color: u32) -> Vec<(Range<usize>, Flags)> {
    let mut flags = vec![Flags::default(); len];
    for (r, f) in spans {
        for x in &mut flags[r.start.min(len)..r.end.min(len)] {
            *x = *f;
        }
    }
    for x in &mut flags[range.start.min(len)..range.end.min(len)] {
        if !(x.marker || x.cmarker || x.hidden) {
            x.color = Some(color);
        }
    }
    coalesce(&flags)
}

/// Se `before` termina com um marcador de cor e `after` começa com `}`,
/// devolve o tamanho do marcador de abertura.
pub fn color_wrap_len(before: &str, after: &str) -> Option<usize> {
    if !after.starts_with('}') {
        return None;
    }
    let open = before.rfind('{')?;
    let (_, n) = parse_color_open(&before[open..])?;
    (open + n == before.len()).then_some(n)
}

fn coalesce(flags: &[Flags]) -> Vec<(Range<usize>, Flags)> {
    let mut out: Vec<(Range<usize>, Flags)> = Vec::new();
    for (i, f) in flags.iter().enumerate() {
        match out.last_mut() {
            Some((r, last)) if *last == *f && r.end == i => r.end = i + 1,
            _ => out.push((i..i + 1, *f)),
        }
    }
    out.retain(|(_, f)| *f != Flags::default());
    out
}

// ---------- tabelas ----------

fn char_width(c: char) -> usize {
    if (c as u32) >= 0x1100 && !c.is_ascii() && c as u32 != 0x2026 { 2 } else { 1 }
}

/// Largura visível em colunas de monoespaçado: ignora marcadores Markdown
/// (`**`, `*`, `` ` ``, `{cor `, URL de link…), que ficam ocultos na tela.
pub fn visible_width(s: &str) -> usize {
    let mut flags = vec![Flags::default(); s.len()];
    inline(s, 0..s.len(), &mut flags);
    s.char_indices()
        .filter(|(i, _)| {
            let f = flags[*i];
            !(f.marker || f.url || f.hidden || f.cmarker)
        })
        .map(|(_, c)| char_width(c))
        .sum()
}

fn width(s: &str) -> usize {
    visible_width(s)
}

/// Remove marcações de cor `{cor texto}` de um trecho, mantendo o texto.
pub fn strip_colors(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut closes: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if let Some((_, content)) = parse_color_open(&s[i..]) {
                if let Some(close) = find_matching(b, i, b'{', b'}') {
                    closes.push(close);
                    i += content;
                    continue;
                }
            }
        }
        if b[i] == b'}' && closes.last() == Some(&i) {
            closes.pop();
            i += 1;
            continue;
        }
        let l = char_len(b[i]);
        out.push_str(&s[i..i + l]);
        i += l;
    }
    out
}

/// Tabela vazia em Markdown com `cols` colunas e `rows` linhas de dados.
pub fn table_template(cols: usize, rows: usize) -> String {
    let cols = cols.max(1);
    let sep = format!("|{}", " --- |".repeat(cols));
    let row = format!("|{}", "   |".repeat(cols));
    let mut out = vec![row.clone(), sep];
    for _ in 0..rows.max(1) {
        out.push(row.clone());
    }
    out.join("\n")
}

// ---------- prefixos de bloco (atalhos de formatação) ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prefix {
    Heading(u8),
    Bullet,
    Numbered,
    Task,
    Quote,
    Toggle,
}

impl Prefix {
    pub fn text(self) -> String {
        match self {
            Prefix::Heading(n) => format!("{} ", "#".repeat(n.clamp(1, 6) as usize)),
            Prefix::Bullet => "- ".to_string(),
            Prefix::Numbered => "1. ".to_string(),
            Prefix::Task => "- [ ] ".to_string(),
            Prefix::Quote => "> ".to_string(),
            Prefix::Toggle => format!("{TOGGLE_OPEN} "),
        }
    }
}

/// (tamanho do recuo, tamanho do prefixo depois do recuo, tipo do prefixo).
pub fn line_prefix(line: &str) -> (usize, usize, Option<Prefix>) {
    let indent = leading_ws(line);
    let rest = &line[indent..];
    let b = rest.as_bytes();
    if let Some(n) = heading_level(rest) {
        return (indent, n as usize + 1, Some(Prefix::Heading(n)));
    }
    if rest.starts_with(TOGGLE_OPEN) || rest.starts_with(TOGGLE_CLOSED) {
        let n = TOGGLE_OPEN.len() + usize::from(b.get(TOGGLE_OPEN.len()) == Some(&b' '));
        return (indent, n, Some(Prefix::Toggle));
    }
    if task(rest).is_some() {
        let n = 5 + usize::from(b.get(5) == Some(&b' '));
        return (indent, n, Some(Prefix::Task));
    }
    if is_bullet(rest) {
        return (indent, 2, Some(Prefix::Bullet));
    }
    if let Some(n) = numbered_len(rest) {
        return (indent, n, Some(Prefix::Numbered));
    }
    if b.first() == Some(&b'>') {
        let n = 1 + usize::from(b.get(1) == Some(&b' '));
        return (indent, n, Some(Prefix::Quote));
    }
    (indent, 0, None)
}

fn split_cells(row: &str) -> Vec<String> {
    let t = row.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push(c);
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// Reescreve um bloco de tabela com colunas alinhadas (fonte monoespaçada).
pub fn format_table(rows: &[&str]) -> Vec<String> {
    let parsed: Vec<(bool, Vec<String>)> = rows.iter().map(|r| (is_table_sep(r.trim()), split_cells(r))).collect();
    let ncols = parsed.iter().map(|(_, c)| c.len()).max().unwrap_or(1).max(1);
    let mut widths = vec![3usize; ncols];
    let mut align = vec![0u8; ncols]; // 0 esq, 1 centro, 2 dir
    for (sep, cells) in &parsed {
        for (i, c) in cells.iter().enumerate() {
            if *sep {
                let (l, r) = (c.starts_with(':'), c.ends_with(':'));
                align[i] = match (l, r) {
                    (true, true) => 1,
                    (false, true) => 2,
                    _ => 0,
                };
            } else {
                widths[i] = widths[i].max(width(c));
            }
        }
    }
    parsed
        .iter()
        .map(|(sep, cells)| {
            let mut line = String::from("|");
            for i in 0..ncols {
                let w = widths[i];
                if *sep {
                    let dashes = "-".repeat(w.max(3));
                    let cell = match align[i] {
                        1 => format!(":{}:", &dashes[..w.max(3) - 2]),
                        2 => format!("{}:", &dashes[..w.max(3) - 1]),
                        _ => dashes,
                    };
                    line.push(' ');
                    line.push_str(&cell);
                    line.push_str(" |");
                } else {
                    let c = cells.get(i).map(String::as_str).unwrap_or("");
                    let pad = w.saturating_sub(width(c));
                    let (l, r) = match align[i] {
                        1 => (pad / 2, pad - pad / 2),
                        2 => (pad, 0),
                        _ => (0, pad),
                    };
                    line.push(' ');
                    line.push_str(&" ".repeat(l));
                    line.push_str(c);
                    line.push_str(&" ".repeat(r));
                    line.push_str(" |");
                }
            }
            line
        })
        .collect()
}

pub fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Número de colunas de uma linha de tabela.
pub fn table_cols(line: &str) -> usize {
    split_cells(line).len().max(1)
}

/// Prefixo a repetir na linha seguinte ao dar Enter numa lista (`- `, `- [ ] `, `3. `),
/// e se a linha atual é um item vazio (que deve ser encerrado).
pub fn list_continuation(line: &str) -> Option<(String, bool)> {
    let indent = &line[..leading_ws(line)];
    let rest = &line[indent.len()..];
    if let Some(_) = task(rest) {
        let empty = rest[5..].trim().is_empty();
        return Some((format!("{indent}{} [ ] ", &rest[..1]), empty));
    }
    if is_bullet(rest) {
        let empty = rest[2..].trim().is_empty();
        return Some((format!("{indent}{} ", &rest[..1]), empty));
    }
    if let Some(n) = numbered_len(rest) {
        let num: u32 = rest[..n - 2].parse().unwrap_or(0);
        let empty = rest[n..].trim().is_empty();
        let sep = &rest[n - 2..n - 1];
        return Some((format!("{indent}{}{sep} ", num + 1), empty));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles() {
        let l = analyze_one("# Título **forte** e *leve* `x` ~~r~~ {#f00 cor}");
        assert_eq!(l.block, Block::Heading(1));
        assert!(l.spans.iter().any(|(_, f)| f.bold));
        assert!(l.spans.iter().any(|(_, f)| f.italic));
        assert!(l.spans.iter().any(|(_, f)| f.code));
        assert!(l.spans.iter().any(|(_, f)| f.strike));
        assert!(l.spans.iter().any(|(_, f)| f.color == Some(0xff0000)));
    }

    #[test]
    fn blocks() {
        assert_eq!(analyze_one("- [x] feito").block, Block::Task(true));
        assert_eq!(analyze_one("- item").block, Block::Bullet);
        assert_eq!(analyze_one("2) item").block, Block::Numbered);
        assert_eq!(analyze_one("---").block, Block::Rule);
        assert_eq!(analyze_one("---\\").block, Block::Rule);
        assert!(analyze_one("---\\").hard_break);
        assert!(analyze_one("# Título\\").hard_break);
        assert_eq!(analyze_one("<br>").block, Block::Space);
        assert_eq!(parse_image("![a](img/x.webp =240x) fim"), Some((22, "a", "img/x.webp", Some(240))));
        assert_eq!(parse_image("![](x.png)"), Some((10, "", "x.png", None)));
        let li = analyze_one("texto ![](a.png) e ![b](c.webp =50x) fim");
        assert_eq!(li.block, Block::Text);
        assert_eq!(li.images.len(), 2);
        assert_eq!(li.images[1].width, Some(50));
        assert!(li.spans.iter().any(|(r, f)| f.img && r.start == 6));
        assert_eq!(analyze_one("![](a.png =300x)").block, Block::Image);
        assert_eq!(analyze_one("  <br>  ").block, Block::Space);
        assert_eq!(analyze_one("<br>x").block, Block::Text);
        assert_eq!(analyze_one("> citação").block, Block::Quote);
        assert_eq!(analyze_one("| a | b |").block, Block::Table);
        assert!(analyze_one("linha\\").hard_break);
        assert!(!analyze_one("linha\\\\").hard_break);
        assert!(analyze_one("- item\\").hard_break);
        assert_eq!(sep_align("|:--|--:|:-:|---|"), vec![Some(0), Some(2), Some(1), None]);
        assert!(is_numeric_cell("1.234,50"));
        assert!(is_numeric_cell("R$ 12"));
        assert!(is_numeric_cell("-3%"));
        assert!(is_numeric_cell("**42**"));
        assert!(!is_numeric_cell("abc"));
        assert!(!is_numeric_cell(""));
        assert_eq!(analyze_one("|---|:-:|").block, Block::TableSep);
        let v = analyze(["```", "code *x*", "```"]);
        assert_eq!(v[1].block, Block::Code);
    }

    #[test]
    fn table_in_text() {
        let t = format_tables_in_text("x\n|a|b|\n|-|-|\n|c|d|\ny");
        assert_eq!(t, "x\n| a   | b   |\n| --- | --- |\n| c   | d   |\ny");
    }

    #[test]
    fn table() {
        let out = format_table(&["|a|bb|", "|-|-|", "|ccc|d|"]);
        assert_eq!(out[0], "| a   | bb  |");
        assert_eq!(out[1], "| --- | --- |");
        assert_eq!(out[2], "| ccc | d   |");
    }

    #[test]
    fn visible() {
        assert_eq!(visible_width("*Azul* x"), 6);
        assert_eq!(visible_width("**a** `b` {#f00 c} [d](http://x)"), 7);
        assert_eq!(color_wrap_len("x {#ff0000 ", "}y"), Some(9));
        assert_eq!(color_wrap_len("x {verde ", "} "), Some(7));
        assert_eq!(color_wrap_len("x ", "}"), None);
        assert_eq!(strip_colors("a {#f00 b} c {verde d {azul e}} f"), "a b c d e f");
        let l = analyze_one("snake_case_name e _isso_");
        assert!(l.spans.iter().filter(|(_, f)| f.italic).count() == 1);
    }

    #[test]
    fn prefixes() {
        assert_eq!(line_prefix("## t"), (0, 3, Some(Prefix::Heading(2))));
        assert_eq!(line_prefix("  - [ ] t"), (2, 6, Some(Prefix::Task)));
        assert_eq!(line_prefix("3. t"), (0, 3, Some(Prefix::Numbered)));
        assert_eq!(line_prefix("> t"), (0, 2, Some(Prefix::Quote)));
        assert_eq!(line_prefix("t"), (0, 0, None));
        assert_eq!(table_template(2, 1), "|   |   |\n| --- | --- |\n|   |   |");
    }

    #[test]
    fn new_blocks() {
        let v = analyze(["▾ aberto", "  filho", "▸ fechado", "  oculto", "", "  oculto2", "fora", ":::", "a", "|||", "b", ":::", "![x](img/a.webp)"]);
        assert_eq!(v[0].block, Block::Toggle(true));
        assert!(!v[1].folded);
        assert_eq!(v[2].block, Block::Toggle(false));
        assert!(v[3].folded && v[5].folded && !v[6].folded);
        assert_eq!(col_fence(":::"), Some(vec![]));
        assert_eq!(col_fence("::: 30 70"), Some(vec![30, 70]));
        assert_eq!(col_fence(":::x"), None);
        assert_eq!(col_fence("::: a"), None);
        let w = analyze([":::  25 75", "a", "|||", "b", ":::"].into_iter());
        assert_eq!(w[0].block, Block::ColStart);
        assert_eq!(w[1].col, Some((0, 0)));
        assert_eq!(w[3].col, Some((0, 1)));
        assert_eq!(w[4].block, Block::ColEnd);
        assert_eq!(v[7].block, Block::ColStart);
        assert_eq!(v[8].col, Some((7, 0)));
        assert_eq!(v[9].block, Block::ColSep);
        assert_eq!(v[10].col, Some((7, 1)));
        assert_eq!(v[11].block, Block::ColEnd);
        assert_eq!(v[12].image.as_deref(), Some("img/a.webp"));
    }

    #[test]
    fn continuation() {
        assert_eq!(list_continuation("- [ ] a"), Some(("- [ ] ".into(), false)));
        assert_eq!(list_continuation("  3. x"), Some(("  4. ".into(), false)));
        assert_eq!(list_continuation("- "), Some(("- ".into(), true)));
        assert_eq!(list_continuation("texto"), None);
    }
}
