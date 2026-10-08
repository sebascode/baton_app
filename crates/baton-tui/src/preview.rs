//! Pantalla 1: vista previa del plan. Permite activar, ordenar y editar pasos antes de ejecutar.

use baton_core::events::{LastRunBanner, RunOutcome, StepInfo};
use baton_core::plan::{GateMode, Plan, Step, StepKind};
use baton_core::step_run::{gate_info, step_info};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::plan_prompt::{PlanOp, PlanPrompt, PlanRequest, PromptOutcome};
use crate::theme;
use crate::widgets::{self, frame, hsep, justify, pad, shortcuts_height, spans_width, truncate};

/// Etiqueta de tipo entre corchetes: `[check]`, `[gate auto]`...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub label: String,
}

impl Tag {
    pub fn new(label: impl Into<String>) -> Tag {
        Tag {
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewStep {
    /// Identificador del paso en el plan (lo que se le pasa al runner).
    pub id: String,
    pub name: String,
    /// Línea gris bajo el nombre.
    pub meta: String,
    pub tag: Tag,
    pub enabled: bool,
}

/// Lo que se decidió en la vista previa al pulsar enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// Ids de los pasos activos, en el orden en que quedaron en la lista.
    pub steps: Vec<String>,
    pub backup: bool,
    pub rollback: bool,
    pub dry_run: bool,
    /// Reanudar: saltar los pasos que terminaron bien en la última ejecución.
    pub resume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewAction {
    Run(RunRequest),
    /// Ver el historial de ejecuciones del plan.
    History,
    /// Ver el log de la última ejecución.
    LastLog,
    Edit(usize),
    AddGate(usize),
    /// Ver el pipeline del plan (pantalla 9).
    Pipeline,
    /// Pasar a otro plan del proyecto.
    SwitchPlan(String),
    /// Copiar, renombrar o eliminar un plan (ya confirmado en la caja).
    PlanOp(PlanRequest),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewState {
    pub plan: String,
    pub steps: Vec<PreviewStep>,
    pub cursor: usize,
    pub backup: bool,
    pub rollback: bool,
    pub dry_run: bool,
    /// Avisos (por ejemplo, por qué no se pudo ejecutar); se borran con la siguiente tecla.
    pub notice: Vec<String>,
    /// Los planes del proyecto, para poder cambiar de uno a otro (`p`). Con uno solo (o vacío,
    /// como en la demo) no hay a dónde cambiar y la tecla no se ofrece.
    pub plans: Vec<String>,
    /// Selector de planes abierto: posición del cursor.
    pub switcher: Option<usize>,
    /// Caja de copiar, renombrar o eliminar el plan elegido en el selector.
    pub prompt: Option<Box<PlanPrompt>>,
    /// Cómo terminó la última ejecución (la franja de estado de arriba); `None` si nunca se ejecutó.
    pub last_run: Option<LastRunBanner>,
    /// Texto con el que se filtra la lista (nombre, tipo o línea gris); vacío = sin filtro.
    pub filter: String,
    /// Se está escribiendo el filtro: las letras son texto, no atajos.
    pub filtering: bool,
}

/// Línea gris de un paso: su descripción o, si no la tiene, lo que hace.
fn step_meta(step: &Step) -> String {
    if let Some(d) = step.description.as_deref().filter(|d| !d.trim().is_empty()) {
        return d.to_string();
    }
    let src: Vec<&str> = step.source.iter().collect();
    match step.kind {
        StepKind::Compose | StepKind::Dockerfile | StepKind::Script | StepKind::Sql
            if !src.is_empty() =>
        {
            src.join(", ")
        }
        StepKind::Gate => step
            .gate
            .as_ref()
            .map(|g| gate_info(g).summary)
            .unwrap_or_default(),
        _ => step.command.clone().unwrap_or_default(),
    }
}

const SHORTCUTS: [(&str, &str); 6] = [
    ("espacio", "activar"),
    ("shift ↑↓", "mover"),
    ("e", "editar paso"),
    ("g", "añadir gate"),
    ("v", "pipeline"),
    ("enter", "ejecutar"),
];

/// Todos los pasos de un plan como los muestra el pipeline (para verlo antes de ejecutar).
/// `default_target` es el destino de los pasos que no declaran uno.
pub fn plan_step_infos(plan: &Plan, default_target: &str) -> Vec<StepInfo> {
    plan.steps
        .iter()
        .map(|s| step_info(s, s.target.as_deref().unwrap_or(default_target)))
        .collect()
}

impl PreviewState {
    /// Rehace la lista con el plan actualizado (por ejemplo, tras guardar en el editor), sin
    /// perder los toggles ni lo que el usuario activó o desactivó en los pasos que siguen existiendo.
    pub fn refresh_from_plan(&mut self, plan: &Plan) {
        let was: std::collections::HashMap<String, bool> = self
            .steps
            .iter()
            .map(|s| (s.id.clone(), s.enabled))
            .collect();
        let cursor_id = self.steps.get(self.cursor).map(|s| s.id.clone());
        let mut fresh = PreviewState::from_plan(plan);
        for s in &mut fresh.steps {
            if let Some(enabled) = was.get(&s.id) {
                s.enabled = *enabled;
            }
        }
        self.cursor = cursor_id
            .and_then(|id| fresh.steps.iter().position(|s| s.id == id))
            .unwrap_or(0);
        self.steps = fresh.steps;
    }

    /// La vista previa de un plan real: sus pasos (activos o no) y los toggles de `[options]`.
    pub fn from_plan(plan: &Plan) -> PreviewState {
        let steps = plan
            .steps
            .iter()
            .map(|s| PreviewStep {
                id: s.id.clone(),
                name: s.name.clone(),
                meta: step_meta(s),
                tag: Tag::new(match (s.kind, s.gate.as_ref()) {
                    (StepKind::Gate, Some(g)) if g.mode == GateMode::Manual => {
                        "gate manual".to_string()
                    }
                    (StepKind::Gate, _) => "gate auto".to_string(),
                    (kind, _) => kind.label().to_string(),
                }),
                enabled: s.enabled,
            })
            .collect();
        PreviewState {
            plan: plan.name.clone(),
            steps,
            cursor: 0,
            backup: plan.options.backup,
            rollback: plan.options.auto_rollback,
            dry_run: plan.options.dry_run,
            notice: Vec::new(),
            plans: Vec::new(),
            switcher: None,
            prompt: None,
            last_run: None,
            filter: String::new(),
            filtering: false,
        }
    }

    /// Al volver de una ejecución: lo que se mostraba como aviso o selector ya no aplica.
    pub fn refresh_after_run(&mut self) {
        self.notice.clear();
        self.switcher = None;
        self.prompt = None;
    }

    /// Deja el cursor en el paso con ese id (el que falló), si existe.
    pub fn select_step(&mut self, id: &str) {
        if let Some(pos) = self.steps.iter().position(|s| s.id == id) {
            self.cursor = pos;
        }
    }

    /// El selector de planes se ofrece si el proyecto tiene planes (en la demo, no): con uno solo
    /// sirve igual para renombrarlo o copiarlo.
    fn can_switch(&self) -> bool {
        !self.plans.is_empty()
    }

    /// Una lista nueva de planes (después de copiar, renombrar o eliminar): el cursor del selector
    /// se mantiene dentro de la lista.
    pub fn set_plans(&mut self, plans: Vec<String>) {
        self.plans = plans;
        self.prompt = None;
        if let Some(c) = self.switcher {
            self.switcher = Some(c.min(self.plans.len().saturating_sub(1)));
        }
    }

    /// El plan en el que se está pasó a llamarse así (se renombró).
    pub fn set_current_plan(&mut self, name: &str) {
        self.plan = name.to_string();
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Option<PreviewAction> {
        let prompt = self.prompt.as_mut()?;
        match prompt.handle_key(key) {
            PromptOutcome::Open => None,
            PromptOutcome::Cancel => {
                self.prompt = None;
                None
            }
            PromptOutcome::Submit(req) => {
                self.prompt = None;
                Some(PreviewAction::PlanOp(req))
            }
        }
    }

    fn open_prompt(&mut self, op: PlanOp) {
        let Some(name) = self.switcher.and_then(|c| self.plans.get(c)) else {
            return;
        };
        if op == PlanOp::Delete && *name == self.plan {
            self.notice = vec![
                "no se puede eliminar el plan abierto: cambia a otro plan primero (o usa baton delete)"
                    .to_string(),
            ];
            return;
        }
        self.prompt = Some(Box::new(PlanPrompt::new(op, name)));
    }

    /// Teclas del pie: `p` solo aparece si hay otros planes.
    fn shortcuts(&self) -> Vec<(&'static str, &'static str)> {
        let mut items = SHORTCUTS.to_vec();
        let at = items.len() - 1; // antes de "ejecutar"
        if self.can_switch() {
            items.insert(items.len() - 1, ("p", "planes"));
        }
        // lo de la última ejecución solo aparece si hay una
        if let Some(b) = &self.last_run {
            let mut extra = vec![("h", "historial"), ("l", "último log")];
            if b.can_resume {
                extra.push(("u", "reanudar"));
            }
            for (n, e) in extra.into_iter().enumerate() {
                items.insert(at + n, e);
            }
        }
        // con listas largas el filtro es la forma de llegar a un paso
        if self.steps.len() > 8 || !self.filter.is_empty() {
            items.insert(items.len() - 1, ("/", "filtrar"));
        }
        items.insert(items.len() - 1, ("?", "ayuda"));
        items
    }

    fn switcher_key(&mut self, key: KeyEvent) -> Option<PreviewAction> {
        let cursor = self.switcher?;
        let last = self.plans.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.switcher = Some(cursor.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.switcher = Some((cursor + 1).min(last)),
            KeyCode::Enter => {
                self.switcher = None;
                let chosen = self.plans.get(cursor)?;
                // elegir el plan en el que ya se está solo cierra el selector
                return (*chosen != self.plan).then(|| PreviewAction::SwitchPlan(chosen.clone()));
            }
            KeyCode::Char('c') => self.open_prompt(PlanOp::Copy),
            KeyCode::Char('r') => self.open_prompt(PlanOp::Rename),
            KeyCode::Char('d') => self.open_prompt(PlanOp::Delete),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => self.switcher = None,
            _ => {}
        }
        None
    }

    pub fn active_count(&self) -> usize {
        self.steps.iter().filter(|s| s.enabled).count()
    }

    pub fn request(&self) -> RunRequest {
        RunRequest {
            steps: self
                .steps
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.clone())
                .collect(),
            backup: self.backup,
            rollback: self.rollback,
            dry_run: self.dry_run,
            resume: false,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let shown = self.visible();
        if shown.is_empty() {
            return;
        }
        let at = shown.iter().position(|&i| i == self.cursor).unwrap_or(0);
        let to = at.saturating_add_signed(delta).min(shown.len() - 1);
        self.cursor = shown[to];
    }

    /// Posiciones (en `steps`) de los pasos que pasan el filtro, en orden.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.trim().to_lowercase();
        (0..self.steps.len())
            .filter(|&i| {
                let s = &self.steps[i];
                needle.is_empty()
                    || [s.name.as_str(), s.meta.as_str(), s.tag.label.as_str()]
                        .iter()
                        .any(|t| t.to_lowercase().contains(&needle))
            })
            .collect()
    }

    /// Tras cambiar el filtro, el cursor no puede quedar en un paso oculto.
    fn keep_cursor_visible(&mut self) {
        let shown = self.visible();
        if !shown.contains(&self.cursor)
            && let Some(&first) = shown.first()
        {
            self.cursor = first;
        }
    }

    fn filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            KeyCode::Enter => self.filtering = false,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
            }
            _ => {}
        }
        self.keep_cursor_visible();
    }

    /// Mueve el paso bajo el cursor una posición; el cursor lo acompaña.
    fn move_step(&mut self, delta: isize) {
        if !self.filter.is_empty() {
            self.notice = vec!["con un filtro activo no se mueven pasos: quítalo con esc".into()];
            return;
        }
        let target = self.cursor.saturating_add_signed(delta);
        if self.steps.is_empty() || target >= self.steps.len() || target == self.cursor {
            return;
        }
        self.steps.swap(self.cursor, target);
        self.cursor = target;
    }

    /// `?` abre la ayuda salvo mientras una caja o el selector de planes usan las teclas.
    pub fn accepts_help(&self) -> bool {
        self.prompt.is_none() && self.switcher.is_none() && !self.filtering
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PreviewAction> {
        self.notice.clear();
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        if self.switcher.is_some() {
            return self.switcher_key(key);
        }
        if self.filtering {
            self.filter_key(key);
            return None;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('/') if !self.steps.is_empty() => self.filtering = true,
            // con un filtro puesto, esc lo quita antes de salir
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
            }
            KeyCode::Up if shift => self.move_step(-1),
            KeyCode::Down if shift => self.move_step(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Home => self.cursor = self.visible().first().copied().unwrap_or(0),
            KeyCode::End => self.cursor = self.visible().last().copied().unwrap_or(0),
            KeyCode::Char(' ') => {
                if let Some(s) = self.steps.get_mut(self.cursor) {
                    s.enabled = !s.enabled;
                }
            }
            KeyCode::Char('b') => self.backup = !self.backup,
            KeyCode::Char('r') => self.rollback = !self.rollback,
            KeyCode::Char('d') => self.dry_run = !self.dry_run,
            // También con el plan vacío: es la forma de agregar el primer paso.
            KeyCode::Char('e') => return Some(PreviewAction::Edit(self.cursor)),
            KeyCode::Char('g') if !self.steps.is_empty() => {
                return Some(PreviewAction::AddGate(self.cursor));
            }
            KeyCode::Enter if self.active_count() > 0 => {
                return Some(PreviewAction::Run(self.request()));
            }
            KeyCode::Enter if self.steps.is_empty() => {
                self.notice = vec![
                    "este plan no tiene pasos todavía: pulsa e para agregar el primero".to_string(),
                ]
            }
            KeyCode::Enter => {
                self.notice = vec!["no hay pasos activos: activa alguno con espacio".to_string()]
            }
            KeyCode::Char('p') if self.can_switch() => {
                let at = self.plans.iter().position(|p| *p == self.plan).unwrap_or(0);
                self.switcher = Some(at);
            }
            KeyCode::Char('v') => return Some(PreviewAction::Pipeline),
            KeyCode::Char('h') => return Some(PreviewAction::History),
            KeyCode::Char('l') if self.last_run.is_some() => {
                return Some(PreviewAction::LastLog);
            }
            // reanudar: como enter, pero sin repetir lo que ya salió bien
            KeyCode::Char('u')
                if self.active_count() > 0
                    && self.last_run.as_ref().is_some_and(|b| b.can_resume) =>
            {
                let mut req = self.request();
                req.resume = true;
                return Some(PreviewAction::Run(req));
            }
            KeyCode::Char('q') | KeyCode::Esc => return Some(PreviewAction::Quit),
            _ => {}
        }
        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                format!("Revisar plan: {}", self.plan),
                theme::bold(),
            )],
            vec![Span::styled(
                format!(
                    "{} pasos · {} activos",
                    self.steps.len(),
                    self.active_count()
                ),
                theme::secondary(),
            )],
            theme::border(),
        );

        let content_w = inner.width.saturating_sub(2);
        let shortcuts = self.shortcuts();
        let sc_h = shortcuts_height(&shortcuts, content_w).max(1);
        // de abajo hacia arriba: atajos, separador, toggles, separador, lista
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let sep_low = sc_y.saturating_sub(1);
        let toggles_y = sep_low.saturating_sub(1);
        let sep_high = toggles_y.saturating_sub(1);
        if sep_high <= inner.y {
            return;
        }

        // los avisos ocupan las últimas filas del área de la lista
        let notice_h = (self.notice.len().min(4) as u16).min(sep_high.saturating_sub(inner.y + 3));
        let list = Rect::new(
            inner.x,
            inner.y + 1, // una línea de aire arriba, como en la maqueta
            inner.width,
            sep_high.saturating_sub(inner.y + 1 + notice_h),
        );
        self.render_banner(buf, Rect::new(inner.x, inner.y, inner.width, 1));
        self.render_filter(buf, Rect::new(inner.x, inner.y, inner.width, 1));
        self.render_list(buf, list);
        for (n, line) in self.notice.iter().take(notice_h as usize).enumerate() {
            Line::from(Span::styled(
                truncate(line, content_w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(
                Rect::new(inner.x + 1, sep_high - notice_h + n as u16, content_w, 1),
                buf,
            );
        }

        hsep(buf, area, sep_high, theme::border());
        self.render_toggles(buf, Rect::new(inner.x, toggles_y, inner.width, 1));
        hsep(buf, area, sep_low, theme::border());
        widgets::render_shortcuts(
            buf,
            Rect::new(inner.x + 1, sc_y, content_w, sc_h),
            &shortcuts,
        );
        // la caja de copiar, renombrar o eliminar reemplaza al selector mientras está abierta
        if let Some(p) = &self.prompt {
            p.render(buf, list);
        } else if self.switcher.is_some() {
            self.render_switcher(buf, list);
        }
    }

    /// La franja de estado de arriba: cómo terminó la última ejecución, con ✓ o ✗.
    fn render_banner(&self, buf: &mut Buffer, area: Rect) {
        let Some(b) = &self.last_run else {
            return;
        };
        let (symbol, color) = match b.outcome {
            RunOutcome::Completed => ("✓", theme::OK),
            RunOutcome::CompletedWithWarnings | RunOutcome::Aborted => ("!", theme::WARN),
            RunOutcome::Failed => ("✗", theme::ERR),
        };
        let content = pad(area);
        let when = if b.ago.is_empty() {
            String::new()
        } else {
            format!(" · {}", b.ago)
        };
        Line::from(vec![
            Span::styled(format!("{symbol} "), Style::new().fg(color)),
            Span::styled("Última ejecución: ", theme::secondary()),
            Span::styled(b.detail.clone(), Style::new().fg(color)),
            Span::styled(when, theme::secondary()),
        ])
        .render(content, buf);
    }

    /// Lista de planes sobre la lista de pasos: `●` el actual, `›` el cursor.
    fn render_switcher(&self, buf: &mut Buffer, over: Rect) {
        let rows = self
            .plans
            .len()
            .min(over.height.saturating_sub(4) as usize)
            .max(1);
        let hint = "↑↓ · enter abrir · c copiar · r renombrar · d eliminar · esc";
        let widest = self
            .plans
            .iter()
            .map(|p| p.chars().count())
            .max()
            .unwrap_or(0)
            + 12;
        // el ancho que pide el hint (más los bordes) manda; nunca más que el área disponible
        let w = widest
            .max(hint.chars().count() + 4)
            .min(over.width.saturating_sub(2) as usize) as u16;
        let h = rows as u16 + 4;
        let area = Rect::new(
            over.x + over.width.saturating_sub(w) / 2,
            over.y + over.height.saturating_sub(h) / 2,
            w.min(over.width),
            h.min(over.height),
        );
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" Cambiar de plan ", theme::bold()));
        let inner = block.inner(area);
        block.render(area, buf);
        let cursor = self.switcher.unwrap_or(0);
        let offset = (cursor + 1).saturating_sub(rows);
        for (row, (i, name)) in self
            .plans
            .iter()
            .enumerate()
            .skip(offset)
            .take(rows)
            .enumerate()
        {
            let selected = i == cursor;
            let y = inner.y + row as u16;
            if selected {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    Style::new().bg(theme::SELECTED_BG),
                );
            }
            let marker = if selected {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            let current = if *name == self.plan {
                Span::styled(" ●", Style::new().fg(theme::OK))
            } else {
                Span::raw("  ")
            };
            Line::from(vec![
                marker,
                current,
                Span::raw(" "),
                Span::styled(
                    truncate(name, inner.width.saturating_sub(5) as usize),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ])
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
        }
        Line::from(Span::styled(hint, theme::muted())).render(
            Rect::new(
                inner.x + 1,
                inner.bottom().saturating_sub(1),
                inner.width,
                1,
            ),
            buf,
        );
    }

    fn render_list(&self, buf: &mut Buffer, list: Rect) {
        if self.steps.is_empty() {
            let lines = [
                "Este plan no tiene pasos todavía.",
                "Pulsa e para agregar el primero.",
            ];
            for (n, text) in lines.iter().enumerate() {
                let y = list.y + 1 + n as u16;
                if y < list.bottom() {
                    Line::from(Span::styled(*text, theme::muted())).render(
                        Rect::new(list.x + 2, y, list.width.saturating_sub(2), 1),
                        buf,
                    );
                }
            }
            return;
        }
        let shown = self.visible();
        if shown.is_empty() {
            Line::from(Span::styled(
                format!("Ningún paso coincide con «{}».", self.filter.trim()),
                theme::muted(),
            ))
            .render(
                Rect::new(list.x + 2, list.y + 1, list.width.saturating_sub(2), 1),
                buf,
            );
            return;
        }
        // Ventana sobre los pasos visibles. Cada paso ocupa 2 filas; si queda algo fuera por
        // arriba o por abajo se reserva una fila para decir cuántos, en vez de cortar en silencio.
        let at = shown.iter().position(|&i| i == self.cursor).unwrap_or(0);
        let mut capacity = (list.height / 2) as usize;
        let mut offset = (at + 1).saturating_sub(capacity);
        for _ in 0..2 {
            let above = u16::from(offset > 0);
            let below = u16::from(offset + capacity < shown.len());
            capacity = (list.height.saturating_sub(above + below) / 2) as usize;
            offset = (at + 1).saturating_sub(capacity);
        }
        if capacity == 0 {
            return;
        }
        let above = offset;
        let below = shown.len().saturating_sub(offset + capacity);
        let top = list.y + u16::from(above > 0);
        let digits = self.steps.len().to_string().len();
        let note = |n: usize, arrow: &str, y: u16, buf: &mut Buffer| {
            let noun = if n == 1 { "paso" } else { "pasos" };
            let place = if arrow == "↑" { "arriba" } else { "abajo" };
            Line::from(Span::styled(
                format!("{arrow} {n} {noun} más {place}"),
                theme::muted(),
            ))
            .render(
                Rect::new(list.x + 2, y, list.width.saturating_sub(2), 1),
                buf,
            );
        };
        if above > 0 {
            note(above, "↑", list.y, buf);
        }
        if below > 0 {
            note(below, "↓", top + (capacity as u16) * 2, buf);
        }

        for (row, &i) in shown.iter().skip(offset).take(capacity).enumerate() {
            let step = &self.steps[i];
            let y = top + (row as u16) * 2;
            let rect = Rect::new(list.x, y, list.width, 2);
            let selected = i == self.cursor;
            if selected {
                buf.set_style(rect, Style::new().bg(theme::SELECTED_BG));
            }
            let content = pad(rect);
            let (title, meta) = step_lines(step, i, selected, digits, content.width);
            title.render(Rect::new(content.x, y, content.width, 1), buf);
            meta.render(Rect::new(content.x, y + 1, content.width, 1), buf);
        }
    }

    /// El filtro, a la derecha de la franja de arriba: lo escrito y cuántos pasos pasan.
    fn render_filter(&self, buf: &mut Buffer, area: Rect) {
        if !self.filtering && self.filter.is_empty() {
            return;
        }
        let cursor = if self.filtering { "▌" } else { "" };
        let text = format!(
            "/ {}{cursor} · {} de {}",
            self.filter,
            self.visible().len(),
            self.steps.len()
        );
        let w = (text.chars().count() as u16 + 2).min(area.width);
        let x = area.right().saturating_sub(w + 1);
        let style = Style::new().fg(theme::INFO);
        buf.set_style(Rect::new(x, area.y, w, 1), Style::new());
        Line::from(Span::styled(format!(" {text}"), style)).render(Rect::new(x, area.y, w, 1), buf);
    }

    fn render_toggles(&self, buf: &mut Buffer, area: Rect) {
        buf.set_style(area, Style::new().bg(theme::PANEL_BG));
        let item = |on: bool, text: &str| {
            let (mark, color) = if on {
                ("(●)", theme::OK)
            } else {
                ("( )", theme::MUTED)
            };
            vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::raw(format!(" {text}")),
            ]
        };
        let mut spans = Vec::new();
        for (n, (on, text)) in [
            (self.backup, "Backup antes de ejecutar"),
            (self.rollback, "Rollback automático"),
            (self.dry_run, "Dry-run"),
        ]
        .into_iter()
        .enumerate()
        {
            if n > 0 {
                spans.push(Span::raw("   "));
            }
            spans.extend(item(on, text));
        }
        Line::from(spans).render(pad(area), buf);
    }
}

/// Las dos líneas de un paso: título con etiqueta a la derecha y meta en gris.
fn step_lines(
    step: &PreviewStep,
    index: usize,
    selected: bool,
    digits: usize,
    width: u16,
) -> (Line<'static>, Line<'static>) {
    let dim = Style::new().fg(theme::MUTED);
    let (base, name_style, tag_style, check) = if step.enabled {
        (
            Style::new(),
            Style::new(),
            Style::new().fg(theme::tag_color(&step.tag.label)),
            Span::raw("[x]"),
        )
    } else {
        (
            dim,
            dim.add_modifier(Modifier::CROSSED_OUT),
            dim,
            Span::styled("[ ]", dim),
        )
    };

    let marker = if selected {
        Span::styled("›", Style::new().fg(theme::INFO))
    } else {
        Span::raw(" ")
    };
    let mut left = vec![
        marker,
        Span::raw(" "),
        Span::styled("⋮⋮", dim),
        Span::raw(" "),
        check,
        Span::raw(" "),
        Span::styled(format!("{:>digits$}", index + 1), base),
        Span::raw("  "),
    ];
    let prefix_w = spans_width(&left);

    // "(opcional)" y similares van atenuados dentro del nombre.
    match step.name.split_once(" (") {
        Some((head, rest)) if step.enabled => {
            left.push(Span::styled(head.to_string(), name_style));
            left.push(Span::styled(format!(" ({rest}"), theme::secondary()));
        }
        _ => left.push(Span::styled(step.name.clone(), name_style)),
    }

    let right = vec![Span::styled(format!("[{}]", step.tag.label), tag_style)];
    let title = justify(left, right, width);

    let meta_style = if step.enabled {
        theme::secondary()
    } else {
        dim
    };
    let mut meta = vec![Span::raw(" ".repeat(prefix_w))];
    meta.push(Span::styled(step.meta.clone(), meta_style));
    let meta = Line::from(widgets::truncate_spans(meta, width as usize));
    (title, meta)
}
