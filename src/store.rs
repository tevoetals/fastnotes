//! Armazenamento: um arquivo `.md` UTF-8 puro por nota, mais um arquivo
//! `state` minúsculo (última nota, tamanho da janela, zoom).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct NoteMeta {
    pub path: PathBuf,
    pub title: String,
    pub preview: String,
    /// Texto em minúsculas usado pela busca (até 64 KiB).
    pub haystack: String,
    pub modified: SystemTime,
}

pub struct State {
    pub last: Option<String>,
    /// Abas abertas (nomes de arquivo; vazio = nota nova sem arquivo).
    pub tabs: Vec<String>,
    pub active: usize,
    pub width: u32,
    pub height: u32,
    pub zoom: i32,
}

pub struct Store {
    pub base_dir: PathBuf,
    pub notes_dir: PathBuf,
    pub trash_dir: PathBuf,
    pub images_dir: PathBuf,
    state_path: PathBuf,
}

impl Store {
    pub fn open() -> io::Result<Store> {
        let base = std::env::var_os("FASTNOTES_DIR").map(PathBuf::from).unwrap_or_else(|| {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| {
                    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                    home.join(".local").join("share")
                });
            data.join("fastnotes")
        });
        let notes_dir = base.join("notes");
        let trash_dir = base.join("trash");
        fs::create_dir_all(&notes_dir)?;
        fs::create_dir_all(&trash_dir)?;
        let images_dir = base.join("images");
        Ok(Store { state_path: base.join("state"), base_dir: base, notes_dir, trash_dir, images_dir })
    }

    /// Todas as notas, mais recente primeiro.
    pub fn list(&self) -> Vec<NoteMeta> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&self.notes_dir) else { return out };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let head = read_head(&path, 64 * 1024);
            out.push(NoteMeta {
                title: display_title(&head),
                preview: preview_of(&head),
                haystack: head.to_lowercase(),
                modified: meta.modified().unwrap_or(UNIX_EPOCH),
                path,
            });
        }
        out.sort_by(|a, b| b.modified.cmp(&a.modified));
        out
    }

    pub fn read(&self, path: &Path) -> io::Result<String> {
        let bytes = fs::read(path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Escrita atômica: grava em `.tmp` e renomeia por cima.
    pub fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        let tmp = path.with_extension("md.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    /// Move para a lixeira (`trash/`), nunca apaga de verdade.
    pub fn trash(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        let mut dest = self.trash_dir.join(&name);
        if dest.exists() {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("nota");
            dest = self.trash_dir.join(format!("{stem}-{}.md", stamp()));
        }
        fs::rename(path, dest)
    }

    pub fn new_path(&self) -> PathBuf {
        let base = stamp();
        let mut path = self.notes_dir.join(format!("{base}.md"));
        let mut n = 2;
        while path.exists() {
            path = self.notes_dir.join(format!("{base}-{n}.md"));
            n += 1;
        }
        path
    }

    pub fn load_state(&self) -> State {
        let mut st = State { last: None, tabs: Vec::new(), active: 0, width: 760, height: 540, zoom: 15 };
        if let Ok(s) = fs::read_to_string(&self.state_path) {
            for line in s.lines() {
                let Some((k, v)) = line.split_once('=') else { continue };
                let v = v.trim();
                match k.trim() {
                    "last" if !v.is_empty() => st.last = Some(v.to_string()),
                    "tabs" => st.tabs = v.split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
                    "active" => st.active = v.parse().unwrap_or(0),
                    "width" => st.width = v.parse().unwrap_or(st.width),
                    "height" => st.height = v.parse().unwrap_or(st.height),
                    "zoom" => st.zoom = v.parse().unwrap_or(st.zoom),
                    _ => {}
                }
            }
        }
        st.width = st.width.clamp(360, 8192);
        st.height = st.height.clamp(240, 8192);
        st
    }

    pub fn save_state(&self, st: &State) {
        let body = format!(
            "last={}\ntabs={}\nactive={}\nwidth={}\nheight={}\nzoom={}\n",
            st.last.as_deref().unwrap_or(""),
            st.tabs.join(","),
            st.active,
            st.width,
            st.height,
            st.zoom
        );
        let _ = fs::write(&self.state_path, body);
    }
}

fn read_head(path: &Path, max: u64) -> String {
    let mut buf = Vec::new();
    if let Ok(f) = fs::File::open(path) {
        let _ = f.take(max).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Primeira linha não vazia (sem `#` de markdown), cortada.
pub fn title_of(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    truncate(line, 60)
}

pub fn display_title(text: &str) -> String {
    let t = title_of(text);
    if t.is_empty() { "Sem título".to_string() } else { t }
}

/// Resto do texto (depois do título), com espaços colapsados.
pub fn preview_of(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    lines.next();
    let rest = lines.collect::<Vec<_>>().join(" ");
    let collapsed = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, 70)
}

fn truncate(s: &str, n: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(n).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}

// ---------- data/hora local (via libc, sem dependências pesadas) ----------

pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub yday: i32,
}

pub fn local_time(t: SystemTime) -> Option<LocalTime> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `secs` e `tm` são válidos; localtime_r é reentrante.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month: tm.tm_mon as u32 + 1,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        min: tm.tm_min as u32,
        sec: tm.tm_sec as u32,
        yday: tm.tm_yday,
    })
}

pub fn stamp() -> String {
    match local_time(SystemTime::now()) {
        Some(t) => format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            t.year, t.month, t.day, t.hour, t.min, t.sec
        ),
        None => "nota".to_string(),
    }
}

pub fn now_hm() -> String {
    local_time(SystemTime::now()).map(|t| format!("{:02}:{:02}", t.hour, t.min)).unwrap_or_default()
}

/// "14:32" se hoje, "17 set" se este ano, senão "17/09/2025".
pub fn fmt_date(t: SystemTime) -> String {
    const MESES: [&str; 12] =
        ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"];
    let (Some(d), Some(now)) = (local_time(t), local_time(SystemTime::now())) else {
        return String::new();
    };
    if d.year == now.year && d.yday == now.yday {
        format!("{:02}:{:02}", d.hour, d.min)
    } else if d.year == now.year {
        format!("{} {}", d.day, MESES[(d.month as usize - 1).min(11)])
    } else {
        format!("{:02}/{:02}/{}", d.day, d.month, d.year)
    }
}
