//! fastnotes-telegram: um bot do Telegram que mostra e edita as notas deste PC.
//!
//! O bot conversa com **um único chat** (o seu), pareado por um código. As notas
//! são os mesmos arquivos `.md` do Fast Notes; o app percebe a mudança no disco
//! e recarrega a aba aberta. Toda versão substituída vai antes para a lixeira.
//!
//!   fastnotes-telegram setup <TOKEN>   guarda o token e mostra o link de pareamento
//!   fastnotes-telegram run             atende o bot (o serviço systemd roda isto)
//!   fastnotes-telegram enable|disable  liga/desliga o serviço de usuário
//!   fastnotes-telegram status          mostra bot, chat pareado e serviço

#[path = "../store.rs"]
#[allow(dead_code)]
mod store;

use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use store::Store;

/// Notas por página na lista.
const PAGE: usize = 8;
/// Acima disso a nota vai como arquivo `.md` (o limite do Telegram é 4096).
const INLINE_MAX: usize = 3500;
const BTN_NOTES: &str = "📒 Notas";
const BTN_NEW: &str = "➕ Nova nota";

// ---------------------------------------------------------------------------
// Configuração: ~/.config/fastnotes/telegram.conf (modo 0600)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Config {
    token: String,
    /// Chat pareado: o único que o bot atende.
    chat: Option<i64>,
    /// Código de pareamento pendente (`/start <código>`).
    pair: Option<String>,
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("fastnotes")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn config_path() -> PathBuf {
    std::env::var_os("FASTNOTES_TG_CONFIG").map(PathBuf::from).unwrap_or_else(|| config_dir().join("telegram.conf"))
}

fn load_config() -> Option<Config> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let mut c = Config::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "token" => c.token = v.to_string(),
            "chat" => c.chat = v.parse().ok(),
            "pair" if !v.is_empty() => c.pair = Some(v.to_string()),
            _ => {}
        }
    }
    (!c.token.is_empty()).then_some(c)
}

fn save_config(c: &Config) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    writeln!(f, "# Fast Notes ↔ Telegram. Guarde este arquivo: o token dá acesso ao bot.")?;
    writeln!(f, "token={}", c.token)?;
    writeln!(f, "chat={}", c.chat.map(|n| n.to_string()).unwrap_or_default())?;
    writeln!(f, "pair={}", c.pair.clone().unwrap_or_default())?;
    drop(f);
    std::fs::rename(tmp, path)
}

fn random_code() -> String {
    let mut b = [0u8; 5];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = io::Read::read_exact(&mut f, &mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------------------
// API do Telegram (HTTP + JSON)
// ---------------------------------------------------------------------------

struct Api {
    agent: ureq::Agent,
    base: String,
    files: String,
}

impl Api {
    fn new(token: &str) -> Api {
        // `FASTNOTES_TG_API` aponta para um servidor falso nos testes locais.
        let root = std::env::var("FASTNOTES_TG_API").unwrap_or_else(|_| "https://api.telegram.org".to_string());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(75)))
            .http_status_as_error(false)
            .build()
            .into();
        Api { agent, base: format!("{root}/bot{token}"), files: format!("{root}/file/bot{token}") }
    }

    fn parse(text: &str) -> Result<Value, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("resposta inválida: {e}"))?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v["result"].clone())
        } else {
            Err(v["description"].as_str().unwrap_or("erro desconhecido").to_string())
        }
    }

    fn call(&self, method: &str, body: Value) -> Result<Value, String> {
        let body = body.to_string();
        // Uma conexão ociosa pode ter sido fechada pelo servidor: tenta de novo uma vez.
        let mut last = String::new();
        for _ in 0..2 {
            let text = self
                .agent
                .post(&format!("{}/{method}", self.base))
                .header("Content-Type", "application/json")
                .send(&body)
                .and_then(|mut r| r.body_mut().read_to_string());
            match text {
                Ok(t) => return Self::parse(&t),
                Err(e) => last = format!("rede: {e}"),
            }
        }
        Err(last)
    }

    /// `sendDocument` com multipart/form-data montado à mão.
    fn send_document(&self, chat: i64, name: &str, data: &[u8], caption: &str, markup: Option<Value>) -> Result<Value, String> {
        let boundary = format!("fastnotes{}", random_code());
        let mut body = Vec::new();
        let mut field = |k: &str, v: &str| {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        };
        field("chat_id", &chat.to_string());
        if !caption.is_empty() {
            field("caption", caption);
            field("parse_mode", "HTML");
        }
        if let Some(m) = markup {
            field("reply_markup", &m.to_string());
        }
        let safe: String = name.chars().map(|c| if c == '"' || c == '\r' || c == '\n' { '_' } else { c }).collect();
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"{safe}\"\r\nContent-Type: text/markdown; charset=utf-8\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut resp = self
            .agent
            .post(&format!("{}/sendDocument", self.base))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..])
            .map_err(|e| format!("rede: {e}"))?;
        let text = resp.body_mut().read_to_string().map_err(|e| format!("rede: {e}"))?;
        Self::parse(&text)
    }

    fn download(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let f = self.call("getFile", json!({ "file_id": file_id }))?;
        let path = f["file_path"].as_str().ok_or("arquivo sem caminho")?;
        let mut resp = self.agent.get(&format!("{}/{path}", self.files)).call().map_err(|e| format!("rede: {e}"))?;
        resp.body_mut().with_config().limit(20 << 20).read_to_vec().map_err(|e| format!("rede: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Texto: HTML do Telegram e reconstrução do Markdown
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Os apps do Telegram convertem `**negrito**`, `` `código` `` etc. em
/// formatação ao enviar, apagando os marcadores. Aqui eles voltam: cada
/// entidade vira de novo o Markdown equivalente (offsets em UTF-16).
fn entities_to_md(text: &str, entities: &[Value]) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    // (posição, ordem, texto): fechamentos (0) antes de aberturas (1) na mesma posição.
    let mut marks: Vec<(usize, u8, i64, String)> = Vec::new();
    for e in entities {
        let (Some(off), Some(len)) = (e["offset"].as_u64(), e["length"].as_u64()) else { continue };
        let (s, t) = (off as usize, (off + len) as usize);
        if t > units.len() || len == 0 {
            continue;
        }
        let (open, close) = match e["type"].as_str().unwrap_or("") {
            "bold" => ("**".to_string(), "**".to_string()),
            "italic" => ("*".to_string(), "*".to_string()),
            "strikethrough" => ("~~".to_string(), "~~".to_string()),
            "code" => ("`".to_string(), "`".to_string()),
            "pre" => (format!("```{}\n", e["language"].as_str().unwrap_or("")), "\n```".to_string()),
            "text_link" => ("[".to_string(), format!("]({})", e["url"].as_str().unwrap_or(""))),
            _ => continue,
        };
        // Abre primeiro a entidade mais longa; fecha primeiro a que começou depois.
        marks.push((s, 1, -(len as i64), open));
        marks.push((t, 0, -(s as i64), close));
    }
    if marks.is_empty() {
        return text.to_string();
    }
    marks.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let mut out = String::with_capacity(text.len() + marks.len() * 3);
    let mut pos = 0;
    for (at, _, _, m) in marks {
        if at > pos {
            out.push_str(&String::from_utf16_lossy(&units[pos..at]));
            pos = at;
        }
        out.push_str(&m);
    }
    out.push_str(&String::from_utf16_lossy(&units[pos..]));
    out
}

/// Id curto e estável de uma nota para os botões (callback_data ≤ 64 bytes).
fn note_key(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

fn cut(s: &str, n: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(n).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------------------
// Bot
// ---------------------------------------------------------------------------

enum Pending {
    Replace(PathBuf),
    Append(PathBuf),
    New,
}

struct Bot {
    api: Api,
    store: Store,
    cfg: Config,
    pending: Option<Pending>,
    /// Texto recebido sem contexto, esperando "nova nota" ou "acrescentar a…".
    stash: Option<String>,
}

fn main_keyboard() -> Value {
    json!({ "keyboard": [[{ "text": BTN_NOTES }, { "text": BTN_NEW }]], "resize_keyboard": true, "is_persistent": true })
}

impl Bot {
    fn chat(&self) -> i64 {
        self.cfg.chat.unwrap_or(0)
    }

    fn say(&self, html: &str, markup: Option<Value>) {
        let mut body = json!({ "chat_id": self.chat(), "text": html, "parse_mode": "HTML", "link_preview_options": { "is_disabled": true } });
        if let Some(m) = markup {
            body["reply_markup"] = m;
        }
        if let Err(e) = self.api.call("sendMessage", body) {
            eprintln!("sendMessage: {e}");
        }
    }

    fn find(&self, key: &str) -> Option<store::NoteMeta> {
        self.store.list().into_iter().find(|n| note_key(&n.path) == key)
    }

    fn title_of(path: &Path, store: &Store) -> String {
        store::display_title(&store.read(path).unwrap_or_default())
    }

    /// Lista paginada. `mode`: `o` abre a nota, `a` acrescenta o texto guardado.
    fn list_markup(&self, mode: char, page: usize) -> (String, Value) {
        let notes = self.store.list();
        let pages = notes.len().div_ceil(PAGE).max(1);
        let page = page.min(pages - 1);
        let mut rows: Vec<Value> = notes
            .iter()
            .skip(page * PAGE)
            .take(PAGE)
            .map(|n| {
                let label = format!("{} · {}", cut(&n.title, 40), store::fmt_date(n.modified));
                json!([{ "text": label, "callback_data": format!("{mode}:{}", note_key(&n.path)) }])
            })
            .collect();
        let mut nav = Vec::new();
        if page > 0 {
            nav.push(json!({ "text": "◀ Anteriores", "callback_data": format!("p:{mode}:{}", page - 1) }));
        }
        if page + 1 < pages {
            nav.push(json!({ "text": "Próximas ▶", "callback_data": format!("p:{mode}:{}", page + 1) }));
        }
        if !nav.is_empty() {
            rows.push(Value::Array(nav));
        }
        let head = match (mode, notes.is_empty()) {
            (_, true) => "Nenhuma nota ainda. Toque em ➕ Nova nota.".to_string(),
            ('a', _) => format!("Acrescentar o texto a qual nota? <i>(página {}/{pages})</i>", page + 1),
            _ => format!("📒 <b>{} notas</b> <i>(página {}/{pages})</i>", notes.len(), page + 1),
        };
        (head, json!({ "inline_keyboard": rows }))
    }

    fn send_list(&self, mode: char, page: usize) {
        let (head, markup) = self.list_markup(mode, page);
        self.say(&head, Some(markup));
    }

    fn note_buttons(key: &str) -> Value {
        json!({ "inline_keyboard": [
            [{ "text": "✏️ Substituir", "callback_data": format!("r:{key}") }, { "text": "➕ Acrescentar", "callback_data": format!("e:{key}") }],
            [{ "text": "📄 Arquivo .md", "callback_data": format!("f:{key}") }, { "text": "🗑 Lixeira", "callback_data": format!("d:{key}") }],
            [{ "text": "⬅ Notas", "callback_data": "p:o:0" }]
        ]})
    }

    fn send_note(&self, path: &Path) {
        let text = self.store.read(path).unwrap_or_default();
        let title = store::display_title(&text);
        let key = note_key(path);
        if text.chars().count() > INLINE_MAX {
            self.send_file(path, &format!("<b>{}</b>\nNota longa: vai como arquivo. Edite e mande o .md de volta.", esc(&title)), Some(Self::note_buttons(&key)));
            return;
        }
        let body = if text.trim().is_empty() { "(vazia)".to_string() } else { esc(&text) };
        self.say(&format!("<b>{}</b>\n<pre>{body}</pre>", esc(&title)), Some(Self::note_buttons(&key)));
    }

    fn send_file(&self, path: &Path, caption: &str, markup: Option<Value>) {
        let data = std::fs::read(path).unwrap_or_default();
        let title = store::title_of(&String::from_utf8_lossy(&data));
        let name = if title.is_empty() { "nota.md".to_string() } else { format!("{}.md", cut(&title, 40).trim_end_matches('…').replace('/', "-")) };
        if let Err(e) = self.api.send_document(self.chat(), &name, &data, caption, markup) {
            self.say(&format!("Não consegui enviar o arquivo: {}", esc(&e)), None);
        }
    }

    /// Copia a versão atual para a lixeira antes de mudar (nada se perde).
    fn backup(&self, path: &Path) {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("nota");
        let _ = std::fs::copy(path, self.store.trash_dir.join(format!("{stem}.antes-telegram-{}.md", store::stamp())));
    }

    fn write_note(&self, path: &Path, text: &str) -> bool {
        match self.store.write(path, text) {
            Ok(()) => true,
            Err(e) => {
                self.say(&format!("❌ Erro ao gravar: {}", esc(&e.to_string())), None);
                false
            }
        }
    }

    fn apply(&mut self, pending: Pending, text: String) {
        match pending {
            Pending::Replace(path) => {
                self.backup(&path);
                if self.write_note(&path, &text) {
                    self.say("✅ Nota substituída. A versão anterior está na lixeira do Fast Notes.", Some(main_keyboard()));
                    self.send_note(&path);
                }
            }
            Pending::Append(path) => self.append(&path, &text),
            Pending::New => self.create(&text),
        }
    }

    fn append(&self, path: &Path, text: &str) {
        let old = self.store.read(path).unwrap_or_default();
        let sep = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
        self.backup(path);
        if self.write_note(path, &format!("{old}{sep}{text}\n")) {
            self.say("✅ Texto acrescentado.", Some(main_keyboard()));
            self.send_note(path);
        }
    }

    fn create(&self, text: &str) {
        let path = self.store.new_path();
        if self.write_note(&path, &format!("{}\n", text.trim_end())) {
            self.say("✅ Nota criada.", Some(main_keyboard()));
            self.send_note(&path);
        }
    }

    fn prompt(&self, html: &str) {
        self.say(html, Some(json!({ "force_reply": true, "input_field_placeholder": "Texto da nota…" })));
    }

    fn welcome(&self) {
        self.say(
            "Olá! Este bot mostra e edita as notas do Fast Notes no seu PC.\n\n\
             • <b>📒 Notas</b> lista as notas; toque numa para vê-la.\n\
             • Na nota, <b>✏️ Substituir</b> troca o texto todo pelo que você mandar em seguida; \
             <b>➕ Acrescentar</b> junta ao fim.\n\
             • Para editar: toque no bloco de texto para copiá-lo, cole, mude e envie.\n\
             • Texto solto vira nota nova ou vai para uma nota existente.\n\
             • Nota longa? Mande um arquivo <code>.md</code>.\n\n\
             /cancelar desiste de uma edição em andamento.",
            Some(main_keyboard()),
        );
    }

    fn on_message(&mut self, msg: &Value) {
        let chat = msg["chat"]["id"].as_i64().unwrap_or(0);
        let raw = msg["text"].as_str().or(msg["caption"].as_str()).unwrap_or("");
        // Pareamento: o primeiro `/start <código>` correto vira o dono do bot.
        if self.cfg.chat.is_none() {
            let code = raw.strip_prefix("/start").map(str::trim);
            if let (Some(code), Some(want)) = (code, self.cfg.pair.clone()) {
                if code == want && msg["chat"]["type"].as_str() == Some("private") {
                    self.cfg.chat = Some(chat);
                    self.cfg.pair = None;
                    if let Err(e) = save_config(&self.cfg) {
                        eprintln!("não consegui salvar o pareamento: {e}");
                    }
                    eprintln!("pareado com o chat {chat}");
                    self.say("🔗 <b>Pareado!</b> Só este chat pode ver e editar as suas notas.", None);
                    self.welcome();
                }
            }
            return;
        }
        if Some(chat) != self.cfg.chat {
            return; // qualquer outro chat é ignorado em silêncio
        }
        let entities: Vec<Value> = msg["entities"].as_array().or(msg["caption_entities"].as_array()).cloned().unwrap_or_default();
        let mut text = entities_to_md(raw, &entities);
        let cmd = raw.trim();
        match cmd {
            "/start" | "/ajuda" | "/help" => return self.welcome(),
            "/notas" | BTN_NOTES => {
                self.pending = None;
                return self.send_list('o', 0);
            }
            "/nova" | BTN_NEW => {
                self.pending = Some(Pending::New);
                return self.prompt("Mande o texto da nota nova (ou um arquivo <code>.md</code>).");
            }
            "/cancelar" => {
                self.pending = None;
                self.stash = None;
                return self.say("Ok, nada foi alterado.", Some(main_keyboard()));
            }
            _ => {}
        }
        // Arquivo .md/.txt enviado: o conteúdo substitui o texto da mensagem.
        if let Some(doc) = msg.get("document").filter(|d| d.is_object()) {
            let name = doc["file_name"].as_str().unwrap_or("");
            let mime = doc["mime_type"].as_str().unwrap_or("");
            let ok = name.ends_with(".md") || name.ends_with(".txt") || name.ends_with(".markdown") || mime.starts_with("text/");
            if !ok {
                return self.say("Só aceito arquivos de texto (<code>.md</code> ou <code>.txt</code>).", None);
            }
            match self.api.download(doc["file_id"].as_str().unwrap_or("")) {
                Ok(bytes) => text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n"),
                Err(e) => return self.say(&format!("Não consegui baixar o arquivo: {}", esc(&e)), None),
            }
        }
        if text.trim().is_empty() {
            return;
        }
        if let Some(p) = self.pending.take() {
            return self.apply(p, text);
        }
        self.stash = Some(text);
        self.say(
            "O que faço com este texto?",
            Some(json!({ "inline_keyboard": [
                [{ "text": "➕ Nova nota", "callback_data": "n" }, { "text": "📎 Acrescentar a uma nota", "callback_data": "p:a:0" }],
                [{ "text": "Cancelar", "callback_data": "x" }]
            ]})),
        );
    }

    fn on_callback(&mut self, cq: &Value) {
        let _ = self.api.call("answerCallbackQuery", json!({ "callback_query_id": cq["id"] }));
        if cq["message"]["chat"]["id"].as_i64() != self.cfg.chat || self.cfg.chat.is_none() {
            return;
        }
        let data = cq["data"].as_str().unwrap_or("");
        let msg_id = cq["message"]["message_id"].clone();
        let (kind, arg) = data.split_once(':').unwrap_or((data, ""));
        match kind {
            "p" => {
                let (mode, page) = arg.split_once(':').unwrap_or(("o", "0"));
                let (head, markup) = self.list_markup(mode.chars().next().unwrap_or('o'), page.parse().unwrap_or(0));
                let edit = json!({ "chat_id": self.chat(), "message_id": msg_id, "text": head, "parse_mode": "HTML", "reply_markup": markup });
                // Mensagem com arquivo (legenda) não pode virar texto: manda outra.
                if self.api.call("editMessageText", edit).is_err() {
                    self.say(&head, Some(markup));
                }
            }
            "n" => match self.stash.take() {
                Some(t) => self.create(&t),
                None => self.say("Esse texto já foi usado. Mande de novo.", None),
            },
            "x" => {
                self.stash = None;
                self.pending = None;
                self.say("Ok, nada foi alterado.", Some(main_keyboard()));
            }
            _ => {
                let Some(note) = self.find(arg) else {
                    return self.say("Essa nota não existe mais. Toque em 📒 Notas.", Some(main_keyboard()));
                };
                let title = esc(&Self::title_of(&note.path, &self.store));
                match kind {
                    "o" => self.send_note(&note.path),
                    "a" => match self.stash.take() {
                        Some(t) => self.append(&note.path, &t),
                        None => self.say("Esse texto já foi usado. Mande de novo.", None),
                    },
                    "r" => {
                        self.pending = Some(Pending::Replace(note.path));
                        self.prompt(&format!("Mande o texto <b>completo</b> que substituirá «{title}». Dica: toque no bloco da nota para copiá-lo, cole e edite."));
                    }
                    "e" => {
                        self.pending = Some(Pending::Append(note.path));
                        self.prompt(&format!("Mande o texto a acrescentar ao fim de «{title}»."));
                    }
                    "f" => self.send_file(&note.path, &format!("<b>{title}</b>"), None),
                    "d" => self.say(
                        &format!("Mover «{title}» para a lixeira?"),
                        Some(json!({ "inline_keyboard": [[
                            { "text": "🗑 Sim, mover", "callback_data": format!("D:{arg}") },
                            { "text": "Não", "callback_data": "x" }
                        ]]})),
                    ),
                    "D" => match self.store.trash(&note.path) {
                        Ok(()) => self.say(&format!("🗑 «{title}» foi para a lixeira."), Some(main_keyboard())),
                        Err(e) => self.say(&format!("❌ {}", esc(&e.to_string())), None),
                    },
                    _ => {}
                }
            }
        }
    }

    fn run(&mut self) -> ! {
        let mut offset: i64 = 0;
        let mut backoff = 1;
        let _ = self.api.call(
            "setMyCommands",
            json!({ "commands": [
                { "command": "notas", "description": "Listar as notas" },
                { "command": "nova", "description": "Criar uma nota" },
                { "command": "cancelar", "description": "Desistir da edição em andamento" },
                { "command": "ajuda", "description": "Como usar" }
            ]}),
        );
        eprintln!("fastnotes-telegram: atendendo (notas em {})", self.store.notes_dir.display());
        loop {
            let body = json!({ "offset": offset, "timeout": 50, "allowed_updates": ["message", "callback_query"] });
            match self.api.call("getUpdates", body) {
                Ok(updates) => {
                    backoff = 1;
                    for u in updates.as_array().cloned().unwrap_or_default() {
                        offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                        if let Some(m) = u.get("message").filter(|m| m.is_object()) {
                            self.on_message(m);
                        } else if let Some(c) = u.get("callback_query").filter(|c| c.is_object()) {
                            self.on_callback(c);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("getUpdates: {e} (nova tentativa em {backoff} s)");
                    std::thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(60);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Linha de comando
// ---------------------------------------------------------------------------

const UNIT: &str = "fastnotes-telegram.service";

fn unit_path() -> PathBuf {
    config_dir().parent().map(Path::to_path_buf).unwrap_or_else(|| home().join(".config")).join("systemd/user").join(UNIT)
}

fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl").arg("--user").args(args).status().is_ok_and(|s| s.success())
}

fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut env = String::new();
    if let Some(dir) = std::env::var_os("FASTNOTES_DIR") {
        env = format!("Environment=FASTNOTES_DIR={}\n", dir.to_string_lossy());
    }
    let unit = format!(
        "[Unit]\nDescription=Fast Notes ↔ Telegram\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nExecStart={} run\n{env}Restart=always\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    );
    let path = unit_path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, unit).map_err(|e| e.to_string())?;
    if !(systemctl(&["daemon-reload"]) && systemctl(&["enable", "--now", UNIT]) && systemctl(&["restart", UNIT])) {
        return Err("systemctl falhou".into());
    }
    println!("✔ serviço ligado: {UNIT} (inicia com a sessão)");
    Ok(())
}

fn disable() {
    systemctl(&["disable", "--now", UNIT]);
    let _ = std::fs::remove_file(unit_path());
    systemctl(&["daemon-reload"]);
    println!("✔ serviço desligado e removido");
}

fn setup(token: &str) -> Result<(), String> {
    let api = Api::new(token);
    let me = api.call("getMe", json!({})).map_err(|e| format!("token recusado pelo Telegram: {e}"))?;
    let user = me["username"].as_str().unwrap_or("").to_string();
    let old = load_config();
    let keep_chat = old.as_ref().filter(|c| c.token == token).and_then(|c| c.chat);
    let pair = random_code();
    let cfg = Config { token: token.to_string(), chat: keep_chat, pair: if keep_chat.is_some() { None } else { Some(pair.clone()) } };
    save_config(&cfg).map_err(|e| e.to_string())?;
    println!("✔ bot @{user} configurado ({})", config_path().display());
    match keep_chat {
        Some(c) => println!("  já pareado com o chat {c}"),
        None => {
            println!("  Para parear, abra no celular ou no PC e toque em INICIAR:");
            println!("  https://t.me/{user}?start={pair}");
        }
    }
    Ok(())
}

fn status() {
    let Some(cfg) = load_config() else {
        println!("não configurado: rode `fastnotes-telegram setup <TOKEN>`");
        return;
    };
    let api = Api::new(&cfg.token);
    match api.call("getMe", json!({})) {
        Ok(me) => println!("bot: @{}", me["username"].as_str().unwrap_or("?")),
        Err(e) => println!("bot: erro ({e})"),
    }
    match (cfg.chat, &cfg.pair) {
        (Some(c), _) => println!("chat pareado: {c}"),
        (None, Some(p)) => println!("aguardando pareamento: /start {p}"),
        (None, None) => println!("sem chat pareado"),
    }
    let active = std::process::Command::new("systemctl")
        .args(["--user", "is-active", UNIT])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("serviço: {}", if active.is_empty() { "ausente" } else { &active });
}

/// Quem usa o Fast Notes pela loja (Flatpak) tem as notas dentro de
/// `~/.var/app/…`. Sem `FASTNOTES_DIR` e sem notas na pasta nativa, usa aquela.
fn use_flatpak_notes_if_only_there() {
    if std::env::var_os("FASTNOTES_DIR").is_some() {
        return;
    }
    let native = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("fastnotes/notes");
    let flat = home().join(".var/app/io.github.tevoetals.fastnotes/data/fastnotes");
    let has_notes = |d: &Path| std::fs::read_dir(d).is_ok_and(|mut r| r.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "md"))));
    if !has_notes(&native) && has_notes(&flat.join("notes")) {
        // SAFETY: ainda não há outras threads.
        unsafe { std::env::set_var("FASTNOTES_DIR", &flat) };
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("setup") => match args.get(1) {
            Some(t) => setup(t.trim()),
            None => Err("uso: fastnotes-telegram setup <TOKEN do BotFather>".into()),
        },
        Some("run") => {
            let Some(cfg) = load_config() else {
                eprintln!("não configurado: rode `fastnotes-telegram setup <TOKEN>`");
                std::process::exit(2);
            };
            use_flatpak_notes_if_only_there();
            let store = Store::open().unwrap_or_else(|e| {
                eprintln!("pasta de notas: {e}");
                std::process::exit(1);
            });
            let mut bot = Bot { api: Api::new(&cfg.token), store, cfg, pending: None, stash: None };
            bot.run()
        }
        Some("enable") => enable(),
        Some("disable") => {
            disable();
            Ok(())
        }
        Some("status") | None => {
            status();
            Ok(())
        }
        Some(other) => Err(format!("comando desconhecido: {other}\nuse: setup <TOKEN> · run · enable · disable · status")),
    };
    if let Err(e) = result {
        eprintln!("erro: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_back_to_markdown() {
        let e = |t: &str, o: u64, l: u64| json!({ "type": t, "offset": o, "length": l });
        assert_eq!(entities_to_md("olá mundo", &[e("bold", 4, 5)]), "olá **mundo**");
        assert_eq!(entities_to_md("a b", &[e("italic", 0, 3), e("bold", 2, 1)]), "*a **b***");
        assert_eq!(entities_to_md("🚀 x", &[e("code", 3, 1)]), "🚀 `x`");
        let link = json!({ "type": "text_link", "offset": 0, "length": 4, "url": "https://a.b" });
        assert_eq!(entities_to_md("site", &[link]), "[site](https://a.b)");
        assert_eq!(entities_to_md("fn x", &[json!({ "type": "pre", "offset": 0, "length": 4, "language": "rust" })]), "```rust\nfn x\n```");
        assert_eq!(entities_to_md("#tag", &[e("hashtag", 0, 4)]), "#tag");
    }

    #[test]
    fn keys_are_short_and_stable() {
        let k = note_key(Path::new("/x/20260918-101010.md"));
        assert_eq!(k.len(), 12);
        assert_eq!(k, note_key(Path::new("/y/20260918-101010.md")));
        assert!(format!("D:{k}").len() <= 64);
    }
}
