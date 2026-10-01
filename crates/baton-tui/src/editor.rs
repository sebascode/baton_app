//! Pantalla 7: editor de pasos, con la lista a la izquierda y el formulario del paso a la derecha.
//! Desde aquí se abre el gate multi-check (pantalla 8).
//!
//! Los cambios viven en memoria; persistirlos en el plan llega con el hito d.

use baton_core::plan::{Plan, Sources, Step, StepKind};
use baton_core::units::{Dur, format_duration};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::forms::{Field, Form, Kind, label_width, render_fields};
use crate::gate_view::{GateAction, GateState};
use crate::theme;
use crate::widgets::{self, frame, hsep_range, justify, pad, truncate, vline_to_sep};

const LIST_W: u16 = 18;

/// Tipos de paso en el orden de la maqueta. `script` llega en v0.2: se ve pero no se elige.
const KINDS: [(&str, bool); 7] = [
    ("compose", false),
    ("dockerfile", false),
    ("script", true),
    ("comando", false),
    ("check", false),
    ("backup", false),
    ("gate", false),
];

// Posición de cada campo en el formulario de un paso.
const F_NAME: usize = 0;
const F_KIND: usize = 1;
const F_SOURCE: usize = 2;
const F_TARGET: usize = 3;
const F_COMMAND: usize = 4;
const F_TIMEOUT: usize = 6;
const F_RETRIES: usize = 7;
const F_GATE: usize = 8;
const F_ROLLBACK: usize = 9;
const F_BACKUP: usize = 10;

/// Datos con los que se crea un paso en el editor.
#[derive(Debug, Clone, Default)]
pub struct StepSpec {
    pub name: String,
    pub kind: String,
    pub source: String,
    /// Archivos que coinciden con `source`, si se sabe.
    pub source_count: Option<usize>,
    pub target: String,
    pub command: String,
    pub depends: Vec<u32>,
    pub timeout: String,
    pub retries: String,
    pub gate: Option<GateState>,
    pub rollback: String,
    pub backup_before: bool,
    /// Id del paso en el plan (vacío en un paso nuevo: se genera al guardar).
    pub step_id: String,
    /// El paso del plan del que viene, para conservar lo que el editor no muestra.
    pub origin: Option<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDraft {
    /// Identificador estable dentro del editor (las posiciones cambian al duplicar).
    pub id: u32,
    pub form: Form,
    pub depends: Vec<u32>,
    /// Candidato bajo el cursor en el campo de dependencias.
    pub dep_cursor: usize,
    pub gate: Option<GateState>,
    pub source_count: Option<usize>,
    /// Id del paso en el plan; vacío si es nuevo (se genera al guardar).
    pub step_id: String,
    pub origin: Option<Step>,
}

impl StepDraft {
    fn new(id: u32, spec: StepSpec, targets: &[String]) -> StepDraft {
        let mut target_refs: Vec<&str> = targets.iter().map(String::as_str).collect();
        // el destino del paso siempre debe poder elegirse, aunque no esté en la lista conocida
        if !spec.target.is_empty() && !target_refs.contains(&spec.target.as_str()) {
            target_refs.push(&spec.target);
        }
        let source = Field::text("origen", &spec.source);
        StepDraft {
            id,
            form: Form::new(vec![
                Field::text("nombre", &spec.name),
                Field::choice("tipo", &KINDS, &spec.kind),
                source,
                Field::drop("destino", &target_refs, &spec.target),
                Field::text("comando", &spec.command),
                Field::deps("depende de"),
                Field::text("timeout", &spec.timeout).width(8),
                Field::number("reintentos", &spec.retries).width(6).inline(),
                Field::card("gate para avanzar"),
                Field::text("rollback", &spec.rollback),
                Field::toggle("backup", "antes de este paso", spec.backup_before),
            ]),
            depends: spec.depends,
            dep_cursor: 0,
            gate: spec.gate,
            source_count: spec.source_count,
            step_id: spec.step_id,
            origin: spec.origin,
        }
    }

    pub fn name(&self) -> String {
        self.form.fields[F_NAME].value()
    }

    pub fn kind(&self) -> String {
        self.form.fields[F_KIND].value()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    Back,
    /// Probar solo este paso (posición en la lista).
    TestStep(usize),
    /// Escanear el origen del gate abierto.
    Rescan,
    Save,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    List,
    Field(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    pub plan: String,
    pub steps: Vec<StepDraft>,
    /// Paso seleccionado; `steps.len()` es la fila "+ nuevo paso".
    pub selected: usize,
    pub(crate) focus: Focus,
    pub targets: Vec<String>,
    next_id: u32,
    /// Abierto el gate multi-check del paso seleccionado.
    pub gate_open: bool,
    pub notice: Option<String>,
    /// Esperando confirmar que se quita el gate del paso seleccionado.
    pub(crate) confirm_gate_removal: bool,
    /// Resultado de la última prueba del paso.
    pub test: Option<(bool, String)>,
    /// Los cambios se pueden guardar en un plan real (con datos de demostración, no).
    pub can_save: bool,
    /// Destino de los pasos que no declaran uno.
    pub default_target: String,
}

impl EditorState {
    pub fn new(plan: &str, targets: &[&str], specs: Vec<StepSpec>) -> EditorState {
        let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
        let steps: Vec<StepDraft> = specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| StepDraft::new(i as u32 + 1, s, &targets))
            .collect();
        let next_id = steps.len() as u32 + 1;
        // Sin pasos no hay campo donde estar: se parte en la lista (en "+ nuevo paso"), si no la
        // primera tecla se perdería.
        let focus = if steps.is_empty() {
            Focus::List
        } else {
            Focus::Field(F_NAME)
        };
        EditorState {
            plan: plan.into(),
            steps,
            selected: 0,
            focus,
            targets,
            next_id,
            gate_open: false,
            notice: None,
            confirm_gate_removal: false,
            test: None,
            can_save: false,
            default_target: "local".into(),
        }
    }

    /// El editor de un plan real. `counts[i]` es cuántos archivos coinciden con el origen del
    /// paso `i`, si se sabe.
    pub fn from_plan(
        plan: &Plan,
        targets: &[String],
        default_target: &str,
        counts: &[Option<usize>],
    ) -> EditorState {
        let internal = |id: &str| {
            plan.steps
                .iter()
                .position(|s| s.id == id)
                .map(|p| p as u32 + 1)
        };
        let specs: Vec<StepSpec> = plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let source = s.source.iter().collect::<Vec<_>>().join(", ");
                StepSpec {
                    name: s.name.clone(),
                    kind: s.kind.label().into(),
                    gate: s
                        .gate
                        .as_ref()
                        .map(|g| GateState::from_gate(&s.name, g, &source)),
                    source,
                    source_count: counts.get(i).copied().flatten(),
                    target: s
                        .target
                        .clone()
                        .unwrap_or_else(|| default_target.to_string()),
                    command: s.command.clone().unwrap_or_default(),
                    depends: s.depends_on.iter().filter_map(|d| internal(d)).collect(),
                    timeout: s
                        .timeout
                        .map_or_else(String::new, |d| format_duration(d.as_duration())),
                    retries: s.retries.to_string(),
                    rollback: s.rollback.clone().unwrap_or_default(),
                    backup_before: s.backup_before,
                    step_id: s.id.clone(),
                    origin: Some(s.clone()),
                }
            })
            .collect();
        let names: Vec<&str> = targets.iter().map(String::as_str).collect();
        let mut e = EditorState::new(&plan.name, &names, specs);
        e.can_save = true;
        e.default_target = default_target.to_string();
        e
    }

    /// Los pasos como quedaron en el editor, listos para guardar. Los campos que el editor no
    /// muestra (descripción, `enabled`, mensaje del gate...) se conservan del plan original.
    pub fn to_steps(&self) -> Result<Vec<Step>, Vec<String>> {
        let ids = self.final_ids();
        let mut errors = Vec::new();
        let mut out = Vec::new();
        for (pos, d) in self.steps.iter().enumerate() {
            match self.draft_to_step(d, &ids) {
                Ok(step) => out.push(step),
                Err(msgs) => errors.extend(
                    msgs.into_iter()
                        .map(|m| format!("paso {} ({}): {m}", pos + 1, d.name())),
                ),
            }
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    /// El texto de origen del paso seleccionado (lo que se escanea para inferir checks).
    pub fn current_source(&self) -> Option<String> {
        self.steps
            .get(self.selected)
            .map(|d| d.form.fields[F_SOURCE].value())
    }

    /// Ids finales de los pasos: los existentes se respetan y los nuevos salen del nombre, sin
    /// repetirse.
    fn final_ids(&self) -> Vec<String> {
        let mut used: std::collections::HashSet<String> = self
            .steps
            .iter()
            .filter(|d| !d.step_id.is_empty())
            .map(|d| d.step_id.clone())
            .collect();
        self.steps
            .iter()
            .map(|d| {
                if !d.step_id.is_empty() {
                    return d.step_id.clone();
                }
                let base = baton_core::slug::slug(&d.name(), "paso");
                let mut id = base.clone();
                let mut n = 2;
                while used.contains(&id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                used.insert(id.clone());
                id
            })
            .collect()
    }

    fn draft_to_step(&self, d: &StepDraft, ids: &[String]) -> Result<Step, Vec<String>> {
        let mut errors = Vec::new();
        let f = |i: usize| d.form.fields[i].value();
        let opt = |text: String| {
            let t = text.trim().to_string();
            (!t.is_empty()).then_some(t)
        };
        let origin = d.origin.as_ref();
        let pos = self
            .steps
            .iter()
            .position(|x| std::ptr::eq(x, d))
            .unwrap_or(0);

        let name = f(F_NAME).trim().to_string();
        if name.is_empty() {
            errors.push("el nombre no puede estar vacío".to_string());
        }
        let sources: Vec<String> = f(F_SOURCE)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // el destino por defecto no se escribe si el paso no lo declaraba
        let chosen = f(F_TARGET);
        let declared = origin.and_then(|o| o.target.clone());
        let target = if chosen
            == declared
                .clone()
                .unwrap_or_else(|| self.default_target.clone())
        {
            declared
        } else {
            opt(chosen)
        };

        let timeout = match f(F_TIMEOUT).trim() {
            "" => None,
            t => match t.parse::<Dur>() {
                Ok(v) => Some(v),
                Err(e) => {
                    errors.push(format!("timeout: {e}"));
                    None
                }
            },
        };
        let retries = match f(F_RETRIES).trim() {
            "" => 0,
            t => t.parse::<u32>().unwrap_or_else(|_| {
                errors.push("reintentos debe ser un número".to_string());
                0
            }),
        };
        let gate = match d.gate.as_ref() {
            Some(g) => match g.to_gate(origin.and_then(|o| o.gate.as_ref())) {
                Ok(g) => Some(g),
                Err(e) => {
                    errors.push(format!("gate: {e}"));
                    None
                }
            },
            None => None,
        };
        let depends_on: Vec<String> = d
            .depends
            .iter()
            .filter_map(|dep| self.steps.iter().position(|x| x.id == *dep))
            .filter(|p| *p < pos)
            .map(|p| ids[p].clone())
            .collect();

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Step {
            id: ids[pos].clone(),
            name,
            kind: kind_of(&f(F_KIND)),
            description: origin.and_then(|o| o.description.clone()),
            enabled: origin.is_none_or(|o| o.enabled),
            source: Sources(sources),
            target,
            command: opt(f(F_COMMAND)),
            depends_on,
            timeout,
            retries,
            gate,
            rollback: opt(f(F_ROLLBACK)),
            backup_before: d.form.fields[F_BACKUP].is_on(),
        })
    }

    /// Abre el editor en el paso `idx`.
    pub fn open_at(&mut self, idx: usize, gate: bool) {
        self.selected = idx.min(self.steps.len().saturating_sub(1));
        // Sin pasos no hay campo donde estar: se queda en la lista ("+ nuevo paso").
        self.focus = if self.steps.is_empty() {
            Focus::List
        } else {
            Focus::Field(F_NAME)
        };
        self.test = None;
        self.notice = None;
        self.gate_open = false;
        self.confirm_gate_removal = false;
        if gate {
            self.open_gate();
        }
    }

    fn current(&self) -> Option<&StepDraft> {
        self.steps.get(self.selected)
    }

    fn current_mut(&mut self) -> Option<&mut StepDraft> {
        self.steps.get_mut(self.selected)
    }

    pub fn gate_mut(&mut self) -> Option<&mut GateState> {
        self.current_mut().and_then(|s| s.gate.as_mut())
    }

    pub fn set_test_result(&mut self, ok: bool, message: &str) {
        self.test = Some((ok, message.to_string()));
    }

    fn open_gate(&mut self) {
        let Some(step) = self.steps.get_mut(self.selected) else {
            return;
        };
        if step.gate.is_none() {
            let source = step.form.fields[F_SOURCE].value();
            step.gate = Some(GateState::new(&step.name(), false, &source));
        }
        let removable = step.kind() != "gate";
        if let Some(g) = step.gate.as_mut() {
            g.step_name = step.form.fields[F_NAME].value();
            g.removable = removable;
            g.confirm_remove = false;
        }
        self.gate_open = true;
    }

    fn new_step(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let spec = StepSpec {
            name: "Nuevo paso".into(),
            kind: "comando".into(),
            target: self.targets.first().cloned().unwrap_or_default(),
            timeout: "5m".into(),
            retries: "0".into(),
            ..StepSpec::default()
        };
        self.steps.push(StepDraft::new(id, spec, &self.targets));
        self.selected = self.steps.len() - 1;
        self.focus = Focus::Field(F_NAME);
        self.test = None;
    }

    fn duplicate(&mut self) {
        let Some(orig) = self.current().cloned() else {
            return;
        };
        let mut copy = orig;
        copy.id = self.next_id;
        // la copia es un paso nuevo: su id sale de su nombre al guardar
        copy.step_id = String::new();
        self.next_id += 1;
        let name = format!("{} (copia)", copy.name());
        copy.form.fields[F_NAME].set_text(&name);
        self.steps.insert(self.selected + 1, copy);
        self.selected += 1;
        self.test = None;
        self.notice = Some("paso duplicado".into());
    }

    fn select(&mut self, idx: usize) {
        self.selected = idx.min(self.steps.len());
        self.test = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<EditorAction> {
        if self.gate_open {
            return self.handle_gate_key(key);
        }
        self.notice = None;
        if self.confirm_gate_removal {
            self.confirm_gate_removal = false;
            if matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y')) {
                self.remove_gate();
            }
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('s') => return Some(EditorAction::Save),
                KeyCode::Char('g') => {
                    self.open_gate();
                    return None;
                }
                KeyCode::Char('t') if self.selected < self.steps.len() => {
                    return Some(EditorAction::TestStep(self.selected));
                }
                KeyCode::Char('d') => {
                    self.duplicate();
                    return None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab => {
                self.cycle_focus(1);
                return None;
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                return None;
            }
            KeyCode::PageUp => {
                self.select(self.selected.saturating_sub(1));
                return None;
            }
            KeyCode::PageDown => {
                self.select((self.selected + 1).min(self.steps.len().saturating_sub(1)));
                return None;
            }
            _ => {}
        }

        match self.focus {
            Focus::List => self.handle_list_key(key),
            Focus::Field(i) => self.handle_field_key(i, key),
        }
    }

    fn handle_gate_key(&mut self, key: KeyEvent) -> Option<EditorAction> {
        let action = self.gate_mut()?.handle_key(key)?;
        match action {
            GateAction::Back => {
                self.gate_open = false;
                None
            }
            GateAction::Remove => {
                self.remove_gate();
                None
            }
            GateAction::Rescan => Some(EditorAction::Rescan),
        }
    }

    /// Quita el gate del paso seleccionado y vuelve al formulario.
    fn remove_gate(&mut self) {
        self.gate_open = false;
        self.confirm_gate_removal = false;
        if let Some(step) = self.steps.get_mut(self.selected) {
            step.gate = None;
            self.notice = Some("gate quitado".into());
        }
    }

    /// `x` o `supr` sobre la tarjeta del gate: pide confirmación si hay algo que quitar.
    fn ask_to_remove_gate(&mut self) {
        let Some(step) = self.current() else { return };
        self.notice = if step.gate.is_none() {
            Some("este paso no tiene gate".into())
        } else if step.kind() == "gate" {
            Some(
                "un paso de tipo gate necesita su gate: cambia el tipo del paso o elimínalo".into(),
            )
        } else {
            self.confirm_gate_removal = true;
            None
        };
    }

    fn field_count(&self) -> usize {
        self.current().map_or(0, |s| s.form.fields.len())
    }

    fn cycle_focus(&mut self, delta: isize) {
        // vuelta: lista, campos 0..n, lista, ...
        let n = self.field_count() as isize;
        if n == 0 {
            self.focus = Focus::List;
            return;
        }
        let pos = match self.focus {
            Focus::List => 0,
            Focus::Field(i) => i as isize + 1,
        };
        let next = (pos + delta).rem_euclid(n + 1);
        self.focus = if next == 0 {
            Focus::List
        } else {
            Focus::Field(next as usize - 1)
        };
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Option<EditorAction> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select(self.selected.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => {
                self.select((self.selected + 1).min(self.steps.len()))
            }
            KeyCode::Enter => {
                if self.selected >= self.steps.len() {
                    self.new_step();
                } else {
                    self.focus = Focus::Field(F_NAME);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return Some(EditorAction::Back),
            _ => {}
        }
        None
    }

    fn handle_field_key(&mut self, i: usize, key: KeyEvent) -> Option<EditorAction> {
        if self.selected >= self.steps.len() {
            self.focus = Focus::List;
            return None;
        }
        if key.code == KeyCode::Esc {
            return Some(EditorAction::Back);
        }
        let last = self.field_count() - 1;
        let step = &mut self.steps[self.selected];

        match step.form.fields[i].kind {
            Kind::Card => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => self.open_gate(),
                KeyCode::Delete | KeyCode::Char('x') => self.ask_to_remove_gate(),
                KeyCode::Up => self.focus = Focus::Field(i - 1),
                KeyCode::Down if i < last => self.focus = Focus::Field(i + 1),
                _ => {}
            },
            Kind::Deps => self.handle_deps_key(i, key),
            _ => {
                let before = step.form.fields[i].value();
                if step.form.fields[i].handle_key(&key) {
                    if i == F_SOURCE && step.form.fields[i].value() != before {
                        // el conteo de archivos ya no corresponde al texto editado
                        step.source_count = None;
                    }
                } else {
                    match key.code {
                        KeyCode::Up if i == 0 => self.focus = Focus::List,
                        KeyCode::Up => self.focus = Focus::Field(i - 1),
                        KeyCode::Down | KeyCode::Enter if i < last => {
                            self.focus = Focus::Field(i + 1)
                        }
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn handle_deps_key(&mut self, i: usize, key: KeyEvent) {
        let sel = self.selected;
        // solo pueden depender de pasos anteriores
        let candidates: Vec<u32> = self.steps[..sel].iter().map(|s| s.id).collect();
        let step = &mut self.steps[sel];
        match key.code {
            KeyCode::Left => step.dep_cursor = step.dep_cursor.saturating_sub(1),
            KeyCode::Right => {
                step.dep_cursor = (step.dep_cursor + 1).min(candidates.len().saturating_sub(1));
            }
            KeyCode::Char(' ') => {
                if let Some(id) = candidates.get(step.dep_cursor) {
                    match step.depends.iter().position(|d| d == id) {
                        Some(p) => {
                            step.depends.remove(p);
                        }
                        None => step.depends.push(*id),
                    }
                }
            }
            KeyCode::Up => self.focus = Focus::Field(i - 1),
            KeyCode::Down | KeyCode::Enter => self.focus = Focus::Field(i + 1),
            _ => {}
        }
    }

    // ---------------------------------------------------------------- dibujo

    fn shortcut_items(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm_gate_removal {
            return vec![("s", "sí"), ("n", "no")];
        }
        let mut items = vec![
            ("tab", "campo"),
            ("ctrl g", "configurar gate"),
            ("ctrl t", "probar paso"),
            ("ctrl d", "duplicar"),
            ("ctrl s", "guardar"),
            ("esc", "volver"),
        ];
        // con el foco en la tarjeta de un gate existente se ofrece quitarlo
        let on_card =
            self.focus == Focus::Field(F_GATE) && self.current().is_some_and(|s| s.gate.is_some());
        if on_card {
            items.insert(2, ("x", "quitar gate"));
        }
        items
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        if self.gate_open
            && let Some(g) = self.current().and_then(|s| s.gate.as_ref())
        {
            g.render(buf, area);
            return;
        }
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                format!("✎ Editar paso · plan {}", self.plan),
                theme::bold(),
            )],
            vec![Span::styled(
                format!(
                    "paso {} de {}",
                    (self.selected + 1).min(self.steps.len()),
                    self.steps.len()
                ),
                theme::secondary(),
            )],
            theme::border(),
        );

        let items = self.shortcut_items();
        let content_w = inner.width.saturating_sub(2);
        let notice_h = u16::from(self.notice.is_some());
        let prompt = self
            .confirm_gate_removal
            .then_some(crate::gate_view::REMOVE_PROMPT);
        let sc_h = widgets::prompt_bar_height(&items, content_w, prompt);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let notice_y = sc_y.saturating_sub(notice_h);
        let sep_low = notice_y.saturating_sub(1);
        if sep_low < inner.y + 8 {
            return;
        }

        let list = Rect::new(inner.x, inner.y, LIST_W, sep_low - inner.y);
        self.render_list(buf, list);
        let vx = inner.x + LIST_W;
        vline_to_sep(buf, vx, inner.y, sep_low, theme::border());
        let right = Rect::new(vx + 1, inner.y, inner.right() - vx - 1, sep_low - inner.y);
        self.render_form(buf, area, right);

        widgets::hsep(buf, area, sep_low, theme::border());
        // la unión de la línea vertical con el separador
        if let Some(c) = buf.cell_mut((vx, sep_low)) {
            c.set_symbol("┴");
        }
        if let Some(n) = &self.notice {
            Line::from(Span::styled(
                truncate(n, content_w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(inner.x + 1, notice_y, content_w, 1), buf);
        }
        widgets::render_prompt_bar(
            buf,
            Rect::new(inner.x + 1, sc_y, content_w, sc_h),
            prompt,
            &items,
        );
    }

    fn render_list(&self, buf: &mut Buffer, area: Rect) {
        buf.set_style(area, Style::new().bg(theme::PANEL_BG));
        let rows = self.steps.len() + 1;
        let visible = area.height as usize;
        let offset = (self.selected + 1)
            .saturating_sub(visible)
            .min(rows.saturating_sub(visible));
        let list_focus = self.focus == Focus::List;
        for (n, i) in (offset..rows).take(visible).enumerate() {
            let y = area.y + n as u16;
            let selected = i == self.selected;
            let row = Rect::new(area.x, y, area.width, 1);
            if selected {
                buf.set_style(row, Style::new().bg(theme::SELECTED_BG));
            }
            let marker = if selected {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![marker];
            if i == self.steps.len() {
                spans.push(Span::styled("+ nuevo paso", Style::new().fg(theme::INFO)));
            } else {
                let text = format!("{} {}", i + 1, self.steps[i].name());
                let style = if selected && list_focus {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                spans.push(Span::styled(
                    truncate(&text, area.width as usize - 2),
                    style,
                ));
            }
            Line::from(spans).render(row, buf);
        }
    }

    fn render_form(&self, buf: &mut Buffer, outer: Rect, right: Rect) {
        let Some(step) = self.current() else {
            let msg = if self.steps.is_empty() {
                "no hay pasos: elige + nuevo paso"
            } else {
                "elige un paso"
            };
            Line::from(Span::styled(msg, theme::muted()))
                .render(Rect::new(right.x + 2, right.y + 1, right.width, 1), buf);
            return;
        };
        let focus = match self.focus {
            Focus::Field(i) => Some(i),
            Focus::List => None,
        };
        let content = pad(Rect::new(right.x, right.y, right.width, right.height));
        let fields = &step.form.fields;
        let label_w = label_width(fields);
        let vx = right.x - 1;
        let bottom = right.bottom();

        // 1) campos principales, hasta timeout/reintentos
        let main = &fields[..=F_RETRIES];
        let mut y = content.y;
        for (i, f) in main.iter().enumerate() {
            if f.inline {
                continue; // se dibuja junto al anterior
            }
            if y >= bottom {
                break;
            }
            let mut spans = vec![f.label_span(label_w, focus == Some(i))];
            let used = label_w as u16;
            let width = content.width.saturating_sub(used);
            if matches!(f.kind, Kind::Deps) {
                spans.extend(self.deps_spans(step, width, focus == Some(i)));
            } else {
                let count = match (i, step.source_count) {
                    (F_SOURCE, Some(n)) => {
                        format!("{n} {}", if n == 1 { "archivo" } else { "archivos" })
                    }
                    _ => String::new(),
                };
                spans.extend(f.control_with_hint(width, focus == Some(i), &count));
            }
            // el campo inline que sigue (reintentos) va en la misma fila
            if let Some(next) = fields.get(i + 1).filter(|n| n.inline) {
                spans.push(Span::raw("   "));
                spans.push(next.label_span(0, focus == Some(i + 1)));
                spans.push(Span::raw(" "));
                spans.extend(next.control(6, focus == Some(i + 1)));
            }
            Line::from(spans).render(Rect::new(content.x, y, content.width, 1), buf);
            y += 1;
        }

        // 2) gate como tarjeta
        let sep1 = y;
        if sep1 + 6 < bottom {
            hsep_range(
                buf,
                right.x - 1,
                outer.right() - 1,
                sep1,
                ("├", "┤"),
                theme::border(),
            );
            Line::from(Span::styled("gate para avanzar", theme::secondary()))
                .render(Rect::new(content.x, sep1 + 1, content.width, 1), buf);
            self.render_gate_card(
                buf,
                Rect::new(content.x, sep1 + 2, content.width, 4),
                step,
                focus == Some(F_GATE),
            );

            // 3) recuperación
            let sep2 = sep1 + 6;
            hsep_range(
                buf,
                vx,
                outer.right() - 1,
                sep2,
                ("├", "┤"),
                theme::border(),
            );
            Line::from(Span::styled("recuperación · opcional", theme::secondary()))
                .render(Rect::new(content.x, sep2 + 1, content.width, 1), buf);
            let rec = &fields[F_ROLLBACK..];
            let rows = Rect::new(
                content.x,
                sep2 + 2,
                content.width,
                bottom.saturating_sub(sep2 + 2),
            );
            let used = render_fields(
                buf,
                rows,
                rec,
                focus.and_then(|f| f.checked_sub(F_ROLLBACK)),
                label_w,
            );
            if let Some((ok, msg)) = &self.test {
                let (sym, color) = if *ok {
                    ("✓", theme::OK)
                } else {
                    ("✗", theme::ERR)
                };
                let ty = sep2 + 2 + used;
                if ty < bottom {
                    Line::from(vec![
                        Span::styled(format!("{sym} prueba: "), Style::new().fg(color)),
                        Span::styled(msg.clone(), Style::new().fg(color)),
                    ])
                    .render(Rect::new(content.x, ty, content.width, 1), buf);
                }
            }
        }
    }

    fn deps_spans(&self, step: &StepDraft, width: u16, focused: bool) -> Vec<Span<'static>> {
        let bracket = Style::new().fg(if focused { theme::INFO } else { theme::MUTED });
        let inner = (width as usize).saturating_sub(4).max(1);
        let name_of = |id: u32| {
            self.steps
                .iter()
                .position(|s| s.id == id)
                .map(|p| format!("{} {}", p + 1, self.steps[p].name()))
        };
        let text = if focused {
            let candidates: Vec<u32> = self.steps[..self.selected].iter().map(|s| s.id).collect();
            match candidates.get(step.dep_cursor) {
                Some(id) => {
                    let mark = if step.depends.contains(id) {
                        "✓"
                    } else {
                        " "
                    };
                    format!(
                        "‹ [{mark}] {} ›  espacio para alternar",
                        name_of(*id).unwrap_or_default()
                    )
                }
                None => "no hay pasos anteriores".to_string(),
            }
        } else if step.depends.is_empty() {
            "ninguna".to_string()
        } else {
            step.depends
                .iter()
                .filter_map(|d| name_of(*d))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let t = truncate(&text, inner);
        let pad = inner.saturating_sub(unicode_width::UnicodeWidthStr::width(t.as_str()));
        let style = if step.depends.is_empty() && !focused {
            Style::new().bg(theme::PANEL_BG).fg(theme::MUTED)
        } else {
            Style::new().bg(theme::PANEL_BG)
        };
        vec![
            Span::styled("[ ", bracket),
            Span::styled(format!("{t}{}", " ".repeat(pad)), style),
            Span::styled(" ]", bracket),
        ]
    }

    fn render_gate_card(&self, buf: &mut Buffer, area: Rect, step: &StepDraft, focused: bool) {
        let border = if focused {
            Style::new().fg(theme::INFO)
        } else {
            theme::border()
        };
        Block::new()
            .borders(Borders::ALL)
            .border_style(border)
            .render(area, buf);
        let inner = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 2);
        let link = Span::styled("configurar ›", Style::new().fg(theme::INFO));
        match &step.gate {
            Some(g) => {
                let (first, second) = g.summary();
                let left = vec![
                    Span::styled("◆ ", Style::new().fg(theme::WARN)),
                    Span::styled(first, Style::new()),
                ];
                justify(left, vec![link], inner.width)
                    .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
                if !second.is_empty() {
                    Line::from(Span::styled(format!("  {second}"), theme::secondary()))
                        .render(Rect::new(inner.x, inner.y + 1, inner.width, 1), buf);
                }
            }
            None => {
                let left = vec![Span::styled("sin gate", theme::muted())];
                justify(
                    left,
                    vec![Span::styled("añadir ›", Style::new().fg(theme::INFO))],
                    inner.width,
                )
                .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
            }
        }
    }
}

fn kind_of(label: &str) -> StepKind {
    match label {
        "compose" => StepKind::Compose,
        "dockerfile" => StepKind::Dockerfile,
        "script" => StepKind::Script,
        "check" => StepKind::Check,
        "backup" => StepKind::Backup,
        "gate" => StepKind::Gate,
        _ => StepKind::Comando,
    }
}

#[cfg(test)]
mod empty_plan_tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn an_empty_plan_starts_on_the_new_step_row_and_the_first_enter_adds_one() {
        let mut e = EditorState::new("vacio", &["local"], Vec::new());
        assert_eq!(e.steps.len(), 0);
        // abrirlo desde la vista previa (`e`, `baton start`) no cambia eso
        e.open_at(0, false);
        e.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(e.steps.len(), 1, "la primera tecla no se pierde");
        assert_eq!(e.steps[0].form.fields[F_NAME].value(), "Nuevo paso");
    }
}
