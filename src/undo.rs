//! Desfazer/refazer com os `Change` do cosmic-text, agrupando digitação
//! contínua (e fechando o grupo em espaço/enter: Ctrl+Z desfaz por palavra).

use cosmic_text::Change;
use std::time::{Duration, Instant};

/// Pausa entre teclas que fecha um grupo.
const GAP: Duration = Duration::from_millis(450);
/// Duração máxima de um grupo, mesmo digitando sem parar.
const MAX_GROUP: Duration = Duration::from_millis(1500);
const MAX_DEPTH: usize = 2000;

#[derive(Default)]
pub struct Undo {
    stack: Vec<Change>,
    redo: Vec<Change>,
    last_edit: Option<Instant>,
    group_start: Option<Instant>,
}

impl Undo {
    pub fn clear(&mut self) {
        *self = Undo::default();
    }

    /// Registra uma alteração já aplicada ao editor.
    pub fn record(&mut self, change: Change, boundary: bool) {
        if change.items.is_empty() {
            return;
        }
        let t = Instant::now();
        let merge = match (self.last_edit, self.group_start) {
            (Some(le), Some(gs)) => t - le <= GAP && t - gs <= MAX_GROUP && !self.stack.is_empty(),
            _ => false,
        };
        if merge {
            if let Some(last) = self.stack.last_mut() {
                last.items.extend(change.items);
            }
        } else {
            self.stack.push(change);
            self.group_start = Some(t);
            if self.stack.len() > MAX_DEPTH {
                self.stack.remove(0);
            }
        }
        self.redo.clear();
        if boundary {
            self.last_edit = None;
            self.group_start = None;
        } else {
            self.last_edit = Some(t);
        }
    }

    /// Devolve a alteração inversa a aplicar no editor.
    pub fn undo(&mut self) -> Option<Change> {
        let c = self.stack.pop()?;
        self.redo.push(c.clone());
        self.close_group();
        let mut r = c;
        r.reverse();
        Some(r)
    }

    /// Devolve a alteração a reaplicar no editor.
    pub fn redo(&mut self) -> Option<Change> {
        let c = self.redo.pop()?;
        self.stack.push(c.clone());
        self.close_group();
        Some(c)
    }

    fn close_group(&mut self) {
        self.last_edit = None;
        self.group_start = None;
    }
}
