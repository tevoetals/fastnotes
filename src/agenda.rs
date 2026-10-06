//! Agenda: timers em tarefas (`- [ ] Escrever ⏱ 25m`), eventos com data
//! (`📅 2026-10-07 14:00`), avisos no PC (notificação do desktop, D-Bus
//! escrito à mão) e marcas de "já avisado" compartilhadas entre o app e o
//! bot do Telegram, para o mesmo aviso não sair duas vezes.
//!
//! O estado do timer fica no próprio texto, logo depois da duração, e só
//! aparece na linha em edição:
//!   `⏱ 25m ▶14:32:10`  rodando, termina às 14:32:10 (hora local)
//!   `⏱ 25m ⏸12:41`     pausado, faltam 12 min 41 s
//!   `⏱ 25m ⚠️`         acabou sem a tarefa ser marcada

use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TIMER: &str = "⏱";
pub const EVENT: &str = "📅";
pub const WARN: &str = "⚠️";
const RUN: char = '▶';
const PAUSE: char = '⏸';
/// Tarefa de pausa que o auto-pomodoro insere entre tarefas.
pub const REST_TASK: &str = "Descanse";
pub const REST_SECS: u32 = 10 * 60;
/// Trabalho seguido (em timers) antes de uma pausa.
pub const WORK_BEFORE_REST: u32 = 60 * 60;
/// Aviso antecipado de eventos com hora.
pub const EARLY_SECS: i64 = 10 * 60;
/// Um aviso atrasado (PC desligado, app fechado) ainda sai até este tempo depois.
pub const GRACE_SECS: i64 = 30 * 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TState {
    Idle,
    /// Rodando: hora local (segundos do dia) em que termina.
    Run(u32),
    /// Pausado: segundos restantes.
    Pause(u32),
}

#[derive(Clone, PartialEq, Debug)]
pub struct TimerMark {
    /// Byte do `⏱`.
    pub at: usize,
    /// Fim da duração (`⏱ 25m` = `at..dur_end`).
    pub dur_end: usize,
    pub secs: u32,
    pub state: TState,
    /// Estado gravado (`▶…`/`⏸…`, com o espaço antes); vazio se parado.
    pub state_range: Range<usize>,
    /// Acabou sem a tarefa ser marcada (`⚠️` depois do timer).
    pub warn: bool,
}

fn digits(s: &str) -> (u32, usize) {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (s[..n.min(6)].parse().unwrap_or(0), n)
}

/// `25m`, `25min`, `25`, `1h`, `1h30`, `1h30m`, `90s` → (segundos, bytes lidos).
pub fn parse_duration(s: &str) -> Option<(u32, usize)> {
    let (a, n) = digits(s);
    if n == 0 || n > 4 {
        return None;
    }
    let mut i = n;
    let r = &s[i..];
    let secs = if r.starts_with('h') {
        i += 1;
        let (b, m) = digits(&s[i..]);
        if (1..=2).contains(&m) {
            i += m;
            if s[i..].starts_with("min") {
                i += 3;
            } else if s[i..].starts_with('m') {
                i += 1;
            }
        }
        a * 3600 + b * 60
    } else if r.starts_with("min") {
        i += 3;
        a * 60
    } else if r.starts_with('m') {
        i += 1;
        a * 60
    } else if r.starts_with('s') {
        i += 1;
        a
    } else {
        a * 60
    };
    if s[i..].chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    (secs > 0).then_some((secs, i))
}

/// Até três números separados por `:` → (números, bytes lidos).
fn clock(s: &str) -> Option<(Vec<u32>, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let (v, n) = digits(&s[i..]);
        if n == 0 || n > 2 && !out.is_empty() {
            return None;
        }
        out.push(v);
        i += n;
        if out.len() < 3 && s[i..].starts_with(':') && s[i + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            i += 1;
        } else {
            break;
        }
    }
    (out.len() >= 2).then_some((out, i))
}

/// Timer numa linha de tarefa (o primeiro `⏱` seguido de uma duração).
pub fn parse_timer(line: &str) -> Option<TimerMark> {
    let at = line.find(TIMER)?;
    let mut i = at + TIMER.len();
    if line[i..].starts_with('\u{fe0f}') {
        i += '\u{fe0f}'.len_utf8();
    }
    i += line[i..].len() - line[i..].trim_start_matches(' ').len();
    let (secs, n) = parse_duration(&line[i..])?;
    let dur_end = i + n;
    let rest = &line[dur_end..];
    let ws = rest.len() - rest.trim_start_matches(' ').len();
    let r = &rest[ws..];
    let mut state = TState::Idle;
    let mut state_range = dur_end..dur_end;
    if let Some(t) = r.strip_prefix(RUN) {
        if let Some((v, n)) = clock(t) {
            let sod = v[0] * 3600 + v[1] * 60 + v.get(2).copied().unwrap_or(0);
            state = TState::Run(sod % 86_400);
            state_range = dur_end..dur_end + ws + RUN.len_utf8() + n;
        }
    } else if let Some(t) = r.strip_prefix(PAUSE) {
        let t2 = t.strip_prefix('\u{fe0f}').unwrap_or(t);
        if let Some((v, n)) = clock(t2) {
            let left = if v.len() == 3 { v[0] * 3600 + v[1] * 60 + v[2] } else { v[0] * 60 + v[1] };
            state = TState::Pause(left);
            state_range = dur_end..dur_end + ws + PAUSE.len_utf8() + (t.len() - t2.len()) + n;
        }
    }
    let warn = line[dur_end..].contains('⚠');
    Some(TimerMark { at, dur_end, secs, state, state_range, warn })
}

/// `25m`, `1h`, `1h30`, `90s`.
pub fn fmt_duration(secs: u32) -> String {
    match (secs / 3600, secs % 3600 / 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, _) => format!("{m}m"),
        (h, 0, _) => format!("{h}h"),
        (h, m, _) => format!("{h}h{m:02}"),
    }
}

/// Contagem regressiva: `12:41` ou `1:02:03`.
pub fn fmt_left(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A linha com o timer em outro estado (e com ou sem `⚠️`).
pub fn with_state(line: &str, t: &TimerMark, state: TState, warn: bool) -> String {
    let mut out = line[..t.dur_end].to_string();
    match state {
        TState::Idle => {}
        TState::Run(sod) => out.push_str(&format!(" {RUN}{:02}:{:02}:{:02}", sod / 3600, sod % 3600 / 60, sod % 60)),
        TState::Pause(left) => out.push_str(&format!(" {PAUSE}{}", fmt_left(left as i64))),
    }
    let tail = line[t.state_range.end..].replace(WARN, "").replace('⚠', "");
    out.push_str(tail.trim_end());
    if warn {
        out.push(' ');
        out.push_str(WARN);
    }
    out
}

// ---------- hora local ----------

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn tm_of(t: i64) -> libc::tm {
    let secs = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `secs` e `tm` são válidos; localtime_r é reentrante.
    unsafe { libc::localtime_r(&secs, &mut tm) };
    tm
}

/// Data/hora local → segundos desde 1970 (horário de verão resolvido pela libc).
pub fn local_epoch(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = m as i32;
    tm.tm_sec = s as i32;
    tm.tm_isdst = -1;
    // SAFETY: `tm` é válido; mktime só o normaliza.
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

/// Segundos do dia (hora local) de um instante.
pub fn sec_of_day(t: i64) -> u32 {
    let tm = tm_of(t);
    (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as u32
}

/// Hora do dia gravada (`▶14:32:10`) → instante mais próximo de `now`
/// (até 12 h antes ou depois: um timer pode cruzar a meia-noite).
pub fn resolve_clock(sod: u32, now: i64) -> i64 {
    let tm = tm_of(now);
    let base = local_epoch(tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, 0, 0, 0).unwrap_or(now - sec_of_day(now) as i64);
    let mut t = base + sod as i64;
    if t - now > 43_200 {
        t -= 86_400;
    } else if now - t > 43_200 {
        t += 86_400;
    }
    t
}

/// Quando termina / quanto falta de um timer.
pub fn deadline(t: &TimerMark, now: i64) -> Option<i64> {
    match t.state {
        TState::Run(sod) => Some(resolve_clock(sod, now)),
        _ => None,
    }
}

// ---------- eventos ----------

#[derive(Clone, PartialEq, Debug)]
pub struct EventMark {
    pub at: usize,
    pub end: usize,
    pub when: i64,
    /// Sem hora: o aviso sai às 9h do dia, sem aviso antecipado.
    pub has_time: bool,
}

/// `📅 2026-10-07 14:00`, `📅 07/10/2026 14h`, `📅 07/10 14:30`, `📅 07/10` (9h).
pub fn parse_event(line: &str, now: i64) -> Option<EventMark> {
    let at = line.find(EVENT)?;
    let mut i = at + EVENT.len();
    i += line[i..].len() - line[i..].trim_start_matches(' ').len();
    let s = &line[i..];
    let (a, na) = digits(s);
    if na == 0 {
        return None;
    }
    let (y, mo, d, n) = if na == 4 && s[na..].starts_with('-') {
        let (m, nm) = digits(&s[na + 1..]);
        let p = na + 1 + nm;
        if nm == 0 || !s[p..].starts_with('-') {
            return None;
        }
        let (dd, nd) = digits(&s[p + 1..]);
        if nd == 0 {
            return None;
        }
        (a as i32, m, dd, p + 1 + nd)
    } else if na <= 2 && s[na..].starts_with('/') {
        let (m, nm) = digits(&s[na + 1..]);
        if nm == 0 || nm > 2 {
            return None;
        }
        let mut p = na + 1 + nm;
        let mut y = tm_of(now).tm_year + 1900;
        if s[p..].starts_with('/') {
            let (yy, ny) = digits(&s[p + 1..]);
            if ny == 2 || ny == 4 {
                y = if ny == 2 { 2000 + yy as i32 } else { yy as i32 };
                p += 1 + ny;
            }
        }
        (y, m, a, p)
    } else {
        return None;
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut end = i + n;
    let mut hm = None;
    let r = &line[end..];
    let ws = r.len() - r.trim_start_matches(' ').len();
    if ws > 0 || r.is_empty() {
        let t = &r[ws..];
        let (h, nh) = digits(t);
        if (1..=2).contains(&nh) && h < 24 {
            let after = &t[nh..];
            if let Some(m) = after.strip_prefix(':').or_else(|| after.strip_prefix('h')) {
                let (mi, nm) = digits(m);
                if nm == 2 && mi < 60 {
                    hm = Some((h, mi));
                    end += ws + nh + 1 + 2;
                } else if after.starts_with('h') && nm == 0 {
                    hm = Some((h, 0));
                    end += ws + nh + 1;
                }
            }
        }
    }
    let (h, mi) = hm.unwrap_or((9, 0));
    let when = local_epoch(y, mo, d, h, mi, 0)?;
    Some(EventMark { at, end, when, has_time: hm.is_some() })
}

/// "ter 07/10 14:00" (dia da semana, data e hora locais).
pub fn fmt_when(t: i64, has_time: bool) -> String {
    const DIAS: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
    let tm = tm_of(t);
    let d = format!("{} {:02}/{:02}", DIAS[tm.tm_wday.clamp(0, 6) as usize], tm.tm_mday, tm.tm_mon + 1);
    if has_time { format!("{d} {:02}:{:02}", tm.tm_hour, tm.tm_min) } else { d }
}

/// Texto de uma linha de tarefa/evento sem marcadores, timer nem data.
pub fn title_of_line(line: &str) -> String {
    let mut s = line.to_string();
    if let Some(t) = parse_timer(&s) {
        s.replace_range(t.at..t.state_range.end.max(t.dur_end), "");
    }
    if let Some(e) = parse_event(&s, now()) {
        s.replace_range(e.at..e.end, "");
    }
    let s = s.replace(WARN, "").replace('⚠', "");
    let mut t = s.trim_start().trim_start_matches(['>', ' ']);
    for p in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(p) {
            t = r;
            break;
        }
    }
    let t = t.trim_start_matches('#').trim();
    let t: String = t.replace("**", "").replace("~~", "").replace('`', "");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tarefa marcada (`- [x]`): não avisa mais.
pub fn is_done(line: &str) -> bool {
    let t = line.trim_start().trim_start_matches(['>', ' ']);
    t.starts_with("- [x]") || t.starts_with("- [X]") || t.starts_with("* [x]") || t.starts_with("* [X]")
}

pub fn is_rest(line: &str) -> bool {
    title_of_line(line).starts_with(REST_TASK)
}

// ---------- avisos: quem avisa primeiro marca ----------

fn claims_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FASTNOTES_DIR") {
        return PathBuf::from(d).join(".avisos");
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("fastnotes/avisos")
}

/// Marca o aviso `key` como dado. `false` se outro processo (app ou bot) já
/// o deu. Marcas com mais de 3 dias são apagadas.
pub fn claim(key: &str) -> bool {
    let dir = claims_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let old = SystemTime::now() - Duration::from_secs(3 * 86_400);
        for e in rd.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(format!("{h:016x}"))) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::AlreadyExists,
    }
}

// ---------- notificação do desktop (org.freedesktop.Notifications) ----------

/// Mostra uma notificação no PC (numa thread; falhas só vão para o log).
pub fn notify(summary: &str, body: &str) {
    eprintln!("aviso: {summary} — {body}");
    let (s, b) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = dbus_notify(&s, &b) {
            eprintln!("aviso no PC falhou: {e}");
        }
    });
}

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn pad(&mut self, a: usize) {
        while self.0.len() % a != 0 {
            self.0.push(0);
        }
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.pad(4);
        self.0.extend(v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend(s.as_bytes());
        self.0.push(0);
    }
    fn sig(&mut self, s: &str) {
        self.u8(s.len() as u8);
        self.0.extend(s.as_bytes());
        self.0.push(0);
    }
    /// Abre um array: devolve (posição do tamanho, início dos elementos).
    fn array(&mut self, align: usize) -> (usize, usize) {
        self.u32(0);
        let at = self.0.len() - 4;
        self.pad(align);
        (at, self.0.len())
    }
    fn close(&mut self, (at, start): (usize, usize)) {
        let n = (self.0.len() - start) as u32;
        self.0[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
}

fn method_call(serial: u32, path: &str, iface: &str, member: &str, dest: &str, sig: &str, body: &[u8]) -> Vec<u8> {
    let mut w = W::default();
    w.0.extend([b'l', 1, 0, 1]);
    w.u32(body.len() as u32);
    w.u32(serial);
    let a = w.array(8);
    let fields: [(u8, &str, &str); 5] = [(1, "o", path), (2, "s", iface), (3, "s", member), (6, "s", dest), (8, "g", sig)];
    for (code, ty, val) in fields {
        if val.is_empty() {
            continue;
        }
        w.pad(8);
        w.u8(code);
        w.sig(ty);
        if ty == "g" { w.sig(val) } else { w.str(val) }
    }
    w.close(a);
    w.pad(8);
    w.0.extend(body);
    w.0
}

fn connect_bus() -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;
    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| {
        let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
        format!("unix:path={rt}/bus")
    });
    for part in addr.split(';') {
        let Some(kv) = part.strip_prefix("unix:") else { continue };
        for kv in kv.split(',') {
            if let Some(p) = kv.strip_prefix("path=") {
                if let Ok(s) = UnixStream::connect(p) {
                    return Ok(s);
                }
            } else if let Some(p) = kv.strip_prefix("abstract=") {
                use std::os::linux::net::SocketAddrExt;
                let a = std::os::unix::net::SocketAddr::from_abstract_name(p.as_bytes())?;
                if let Ok(s) = UnixStream::connect_addr(&a) {
                    return Ok(s);
                }
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "sem barramento D-Bus da sessão"))
}

fn dbus_notify(summary: &str, body: &str) -> io::Result<()> {
    let mut s = connect_bus()?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    let uid: String = unsafe { libc::getuid() }.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    s.write_all(format!("\0AUTH EXTERNAL {uid}\r\n").as_bytes())?;
    let mut line = Vec::new();
    let mut c = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        if s.read(&mut c)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "D-Bus fechou na autenticação"));
        }
        line.push(c[0]);
    }
    if !line.starts_with(b"OK") {
        return Err(io::Error::other("D-Bus recusou a autenticação"));
    }
    s.write_all(b"BEGIN\r\n")?;
    s.write_all(&method_call(1, "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", "org.freedesktop.DBus", "", &[]))?;

    let mut b = W::default();
    b.str("Fast Notes");
    b.u32(0);
    b.str("io.github.tevoetals.fastnotes");
    b.str(summary);
    b.str(body);
    let a = b.array(4); // ações: nenhuma
    b.close(a);
    let a = b.array(8);
    for (k, ty, v) in [("desktop-entry", "s", "io.github.tevoetals.fastnotes"), ("sound-name", "s", "alarm-clock-elapsed"), ("urgency", "y", "")] {
        b.pad(8);
        b.str(k);
        b.sig(ty);
        if ty == "y" { b.u8(2) } else { b.str(v) }
    }
    b.close(a);
    b.pad(4);
    b.0.extend((-1i32).to_le_bytes());
    s.write_all(&method_call(
        2,
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
        "org.freedesktop.Notifications",
        "susssasa{sv}i",
        &b.0,
    ))?;

    // Espera as duas respostas (Hello e Notify) para não fechar antes da entrega.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let mut replies = 0;
    while replies < 2 {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend(&chunk[..n]);
        loop {
            if buf.len() < 16 {
                break;
            }
            let body_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
            let fields = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
            let total = (16 + fields).div_ceil(8) * 8 + body_len;
            if buf.len() < total {
                break;
            }
            match buf[1] {
                2 => replies += 1,
                3 => return Err(io::Error::other("o serviço de notificações respondeu com erro")),
                _ => {}
            }
            buf.drain(..total);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("25m"), Some((1500, 3)));
        assert_eq!(parse_duration("25min x"), Some((1500, 5)));
        assert_eq!(parse_duration("25"), Some((1500, 2)));
        assert_eq!(parse_duration("1h30"), Some((5400, 4)));
        assert_eq!(parse_duration("1h"), Some((3600, 2)));
        assert_eq!(parse_duration("90s"), Some((90, 3)));
        assert_eq!(parse_duration("25mx"), None);
        assert_eq!(parse_duration("0m"), None);
    }

    #[test]
    fn timer_states() {
        let t = parse_timer("- [ ] Ler ⏱ 25m").unwrap();
        assert_eq!((t.secs, t.state, t.warn), (1500, TState::Idle, false));
        let run = with_state("- [ ] Ler ⏱ 25m", &t, TState::Run(14 * 3600 + 32 * 60 + 10), false);
        assert_eq!(run, "- [ ] Ler ⏱ 25m ▶14:32:10");
        let t = parse_timer(&run).unwrap();
        assert_eq!(t.state, TState::Run(52330));
        assert_eq!(&run[t.state_range.clone()], " ▶14:32:10");
        let p = with_state(&run, &t, TState::Pause(761), false);
        assert_eq!(p, "- [ ] Ler ⏱ 25m ⏸12:41");
        let t = parse_timer(&p).unwrap();
        assert_eq!(t.state, TState::Pause(761));
        let w = with_state(&p, &t, TState::Idle, true);
        assert_eq!(w, "- [ ] Ler ⏱ 25m ⚠️");
        let t = parse_timer(&w).unwrap();
        assert!(t.warn);
        assert_eq!(with_state(&w, &t, TState::Idle, false), "- [ ] Ler ⏱ 25m");
        let t = parse_timer("- [ ] A ⏱ 1h ▶09:00:00 depois").unwrap();
        assert_eq!(with_state("- [ ] A ⏱ 1h ▶09:00:00 depois", &t, TState::Idle, false), "- [ ] A ⏱ 1h depois");
    }

    /// Mostra uma notificação de verdade: `cargo test -- --ignored notificacao`.
    #[test]
    #[ignore]
    fn notificacao_real() {
        dbus_notify("Fast Notes — teste do aviso", "Se você está lendo isto, os alarmes chegam ao PC.").unwrap();
    }

    #[test]
    fn events_and_titles() {
        let now = local_epoch(2026, 10, 6, 12, 0, 0).unwrap();
        let e = parse_event("- Reunião 📅 2026-10-07 14:00", now).unwrap();
        assert_eq!(e.when, local_epoch(2026, 10, 7, 14, 0, 0).unwrap());
        assert!(e.has_time);
        let e = parse_event("📅 07/10 14h30 dentista", now).unwrap();
        assert_eq!(e.when, local_epoch(2026, 10, 7, 14, 30, 0).unwrap());
        let e = parse_event("📅 8/10/26", now).unwrap();
        assert_eq!((e.when, e.has_time), (local_epoch(2026, 10, 8, 9, 0, 0).unwrap(), false));
        assert!(parse_event("📅 amanhã", now).is_none());
        assert_eq!(title_of_line("- [ ] **Reunião** 📅 2026-10-07 14:00"), "Reunião");
        assert_eq!(title_of_line("- [ ] Ler ⏱ 25m ▶14:32:10 ⚠️"), "Ler");
        assert!(is_rest("  - [ ] Descanse ⏱ 10m"));
        assert_eq!(resolve_clock(sec_of_day(now) + 60, now), now + 60);
    }
}
