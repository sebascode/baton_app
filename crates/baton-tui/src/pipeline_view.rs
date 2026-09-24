//! Pantalla 9: pipeline. Un riel horizontal con todos los pasos y gates (el avance de un vistazo)
//! y, debajo, un timeline vertical agrupado por destino, con los checks de cada gate.
//!
//! Se dibuja solo a partir de [`RunState`], así que sirve antes de ejecutar (todo pendiente),
//! durante la ejecución (con los intentos en vivo), en el fallo y en el resumen.

use std::collections::BTreeSet;

use baton_core::events::{CheckInfo, CheckState, StepStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::run::{Phase, RunState, StepRow};
use crate::run_view::{prompt_text, prompt_width};
use crate::theme;
use crate::widgets::{
    self, fmt_clock, fmt_duration, frame, hsep, pad, shortcuts_height, spans_width, truncate,
    truncate_spans,
};

/// Estado de la interfaz de la vista: qué nodo está seleccionado y qué gates están abiertos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeUi {
    /// Nodo seleccionado cuando no se está siguiendo la ejecución.
    pub cursor: usize,
    /// Seguir el nodo activo mientras corre el plan.
    pub follow: bool,
    /// Pasos cuyo gate se muestra abierto (además de los que están en curso o fallaron).
    pub expanded: BTreeSet<usize>,
}

impl Default for PipeUi {
    fn default() -> Self {
        PipeUi {
            cursor: 0,
            follow: true,
            expanded: BTreeSet::new(),
        }
    }
}

/// Un elemento seleccionable del pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Step(usize),
    Gate(usize),
}

impl Node {
    pub fn step(self) -> usize {
        match self {
            Node::Step(i) | Node::Gate(i) => i,
        }
    }
}

/// Un paso de tipo `gate` (sin acción propia) se dibuja solo como gate.
fn is_gate_step(row: &StepRow) -> bool {
    row.info.kind == "gate" && row.info.gate.is_some()
}

pub fn nodes(rows: &[StepRow]) -> Vec<Node> {
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if is_gate_step(r) {
            out.push(Node::Gate(i));
        } else {
            out.push(Node::Step(i));
            if r.info.gate.is_some() {
                out.push(Node::Gate(i));
            }
        }
    }
    out
}

/// El nodo donde está la acción ahora: el gate si el paso lo está esperando, si no el paso.
fn active_node(state: &RunState, nodes: &[Node]) -> usize {
    // el más avanzado que esté en curso (un paso puede seguir "corriendo" mientras su gate
    // ya espera), y si no hay ninguno, el último que tuvo actividad
    let target_step = state
        .rows
        .iter()
        .rposition(|r| {
            matches!(
                r.info.status,
                StepStatus::Running | StepStatus::Gate | StepStatus::Failed
            )
        })
        .or_else(|| {
            state
                .rows
                .iter()
                .rposition(|r| r.info.status != StepStatus::Pending)
        });
    let Some(s) = target_step else { return 0 };
    let row = &state.rows[s];
    let gate_now = row.info.status == StepStatus::Gate && row.info.gate.is_some();
    let want = if gate_now || is_gate_step(row) {
        Node::Gate(s)
    } else {
        Node::Step(s)
    };
    nodes.iter().position(|n| *n == want).unwrap_or(0)
}

/// El nodo seleccionado de verdad: el activo si se sigue la ejecución, si no el elegido a mano.
pub fn effective_cursor(state: &RunState, nodes: &[Node]) -> usize {
    let last = nodes.len().saturating_sub(1);
    if state.pipe.follow && !state.preview {
        active_node(state, nodes).min(last)
    } else {
        state.pipe.cursor.min(last)
    }
}

impl RunState {
    pub(crate) fn pipe_move(&mut self, delta: isize) {
        let ns = nodes(&self.rows);
        let cur = effective_cursor(self, &ns);
        self.pipe.cursor = cur
            .saturating_add_signed(delta)
            .min(ns.len().saturating_sub(1));
        self.pipe.follow = false;
    }

    pub(crate) fn pipe_toggle(&mut self) {
        let ns = nodes(&self.rows);
        let cur = effective_cursor(self, &ns);
        if let Some(n) = ns.get(cur) {
            let step = n.step();
            if !self.pipe.expanded.remove(&step) {
                self.pipe.expanded.insert(step);
            }
        }
    }
}

// ------------------------------------------------------------------ colores

fn check_symbol(c: &CheckInfo) -> (&'static str, Color) {
    if c.is_new {
        return ("+", theme::WARN);
    }
    match c.state {
        CheckState::Passed => ("✓", theme::OK),
        CheckState::Running => ("◐", theme::INFO),
        CheckState::Failed => ("✗", theme::ERR),
        CheckState::Warning => ("!", theme::WARN),
        CheckState::Pending => ("○", theme::MUTED),
        CheckState::Skipped => ("»", theme::MUTED),
    }
}

/// Color y texto de estado de un gate. Un paso de tipo `gate` usa el estado del propio paso.
fn gate_state(row: &StepRow) -> (Color, String) {
    let any = |s: CheckState| {
        row.info
            .gate
            .as_ref()
            .is_some_and(|g| g.checks.iter().any(|c| c.state == s))
    };
    let warn = any(CheckState::Warning);
    let standalone = is_gate_step(row);
    match row.info.status {
        // un paso puede fallar antes de llegar a su gate: entonces el gate ni corrió
        StepStatus::Failed if standalone || any(CheckState::Failed) => (theme::ERR, "falló".into()),
        StepStatus::Failed => (theme::MUTED, "no llegó a correr".into()),
        StepStatus::Gate => {
            let text = match row.gate {
                Some((a, o)) => format!("intento {a}/{o}"),
                None => "esperando".into(),
            };
            (theme::WARN, text)
        }
        StepStatus::Done if warn => (theme::WARN, "con advertencias".into()),
        StepStatus::Done => (theme::OK, "ok".into()),
        StepStatus::Skipped => (theme::MUTED, "omitido".into()),
        StepStatus::Running if standalone => (theme::WARN, "en curso".into()),
        StepStatus::Running | StepStatus::Pending => (theme::MUTED, "pendiente".into()),
    }
}

fn spine_color(status: StepStatus) -> Color {
    match status {
        StepStatus::Done => theme::OK,
        StepStatus::Running | StepStatus::Gate => theme::INFO,
        StepStatus::Failed => theme::ERR,
        StepStatus::Pending | StepStatus::Skipped => theme::MUTED,
    }
}

/// ¿Se muestran los checks de este gate? Abierto a mano, o porque está activo o falló.
fn gate_open(state: &RunState, i: usize, cursor_on_it: bool) -> bool {
    let row = &state.rows[i];
    let failed_check = row
        .info
        .gate
        .as_ref()
        .is_some_and(|g| g.checks.iter().any(|c| c.state == CheckState::Failed));
    state.pipe.expanded.contains(&i)
        || cursor_on_it
        || row.info.status == StepStatus::Gate
        || (row.info.status == StepStatus::Failed && (is_gate_step(row) || failed_check))
}

// -------------------------------------------------------------------- riel

struct RailNode {
    symbol: &'static str,
    color: Color,
    step: usize,
    label: String,
}

fn rail_nodes(state: &RunState, ns: &[Node]) -> Vec<RailNode> {
    ns.iter()
        .map(|n| match *n {
            Node::Step(i) => {
                let st = state.rows[i].info.status;
                RailNode {
                    symbol: theme::status_symbol(st),
                    color: theme::status_color(st),
                    step: i,
                    label: (i + 1).to_string(),
                }
            }
            Node::Gate(i) => {
                let (color, _) = gate_state(&state.rows[i]);
                RailNode {
                    symbol: "◆",
                    color,
                    step: i,
                    // el gate de un paso se rotula "g"; un paso de tipo gate lleva su número
                    label: if is_gate_step(&state.rows[i]) {
                        (i + 1).to_string()
                    } else {
                        "g".into()
                    },
                }
            }
        })
        .collect()
}

/// Las tres líneas del riel: nodos, números y carriles (destinos).
fn rail_lines(state: &RunState, ns: &[Node], cursor: usize, width: usize) -> [Line<'static>; 3] {
    let rail = rail_nodes(state, ns);
    let n = rail.len();
    if n == 0 || width < 3 {
        return [Line::default(), Line::default(), Line::default()];
    }

    // conector más largo que quepa; si ni así, se muestra una ventana alrededor del cursor
    let span_w = |c: usize, k: usize| k * (1 + c) - c;
    let c = [5usize, 4, 3, 2, 1, 0]
        .into_iter()
        .find(|c| span_w(*c, n) <= width)
        .unwrap_or(0);
    let (start, end) = if span_w(c, n) <= width {
        (0, n)
    } else {
        let mut s = cursor;
        let mut e = cursor + 1;
        loop {
            let grew_l = s > 0 && span_w(c, e - s + 1) <= width.saturating_sub(2);
            if grew_l {
                s -= 1;
            }
            let grew_r = e < n && span_w(c, e - s + 1) <= width.saturating_sub(2);
            if grew_r {
                e += 1;
            }
            if !grew_l && !grew_r {
                break;
            }
        }
        (s, e)
    };
    let hidden_left = start > 0;
    let hidden_right = end < n;
    let x0 = usize::from(hidden_left);

    let mut nodes_row: Vec<Span<'static>> = Vec::new();
    let mut nums_row: Vec<Span<'static>> = Vec::new();
    if hidden_left {
        nodes_row.push(Span::styled("‹", theme::muted()));
        nums_row.push(Span::raw(" "));
    }
    for (k, node) in rail.iter().enumerate().take(end).skip(start) {
        let mut style = Style::new().fg(node.color);
        if k == cursor {
            style = style.bg(theme::SELECTED_BG).add_modifier(Modifier::BOLD);
        }
        nodes_row.push(Span::styled(node.symbol, style));
        let mut label = node.label.clone();
        if label.chars().count() > c {
            label.clear(); // no cabe sin pegarse al vecino
        }
        let num_style = if k == cursor {
            theme::bold()
        } else {
            theme::secondary()
        };
        nums_row.push(Span::styled(
            format!("{:<w$}", label, w = 1 + if k + 1 < end { c } else { 0 }),
            num_style,
        ));
        if k + 1 < end && c > 0 {
            // el conector toma el color del avance: verde mientras el nodo de la izquierda esté listo
            let left_done = matches!(
                state.rows[node.step].info.status,
                StepStatus::Done | StepStatus::Skipped
            ) && node.color != theme::WARN;
            let color = if left_done { theme::OK } else { theme::MUTED };
            nodes_row.push(Span::styled("━".repeat(c), Style::new().fg(color)));
        }
    }
    if hidden_right {
        nodes_row.push(Span::styled("›", theme::muted()));
    }

    // carriles: agrupa nodos consecutivos con el mismo destino
    let mut lane: Vec<char> = vec![' '; span_w(c, end - start) + 1];
    let mut lane_label: Vec<bool> = vec![false; lane.len()];
    let target_of = |k: usize| state.rows[rail[k].step].info.target.clone();
    let mut k = start;
    while k < end {
        let target = target_of(k);
        let mut e = k;
        while e + 1 < end && target_of(e + 1) == target {
            e += 1;
        }
        let xs = (k - start) * (1 + c);
        let xe = (e - start) * (1 + c);
        if !target.is_empty() {
            let next_start = if e + 1 < end {
                (e + 1 - start) * (1 + c)
            } else {
                lane.len()
            };
            let room = next_start.saturating_sub(xs + 1).max(1);
            let label = truncate(&target, room.max(xe - xs + 1));
            let label: Vec<char> = label.chars().collect();
            for (j, ch) in label.iter().enumerate() {
                if xs + j < lane.len() {
                    lane[xs + j] = *ch;
                    lane_label[xs + j] = true;
                }
            }
            let after = xs + label.len();
            if xe > after {
                lane[after + 1..xe].fill('─');
                lane[xe] = '┘';
            }
        }
        k = e + 1;
    }
    let mut lane_spans: Vec<Span<'static>> = vec![Span::raw(" ".repeat(x0))];
    let mut i = 0;
    while i < lane.len() {
        let is_label = lane_label[i];
        let mut j = i;
        while j < lane.len() && lane_label[j] == is_label {
            j += 1;
        }
        let text: String = lane[i..j].iter().collect();
        lane_spans.push(Span::styled(
            text,
            if is_label {
                theme::secondary()
            } else {
                theme::muted()
            },
        ));
        i = j;
    }

    let mut nums = vec![Span::raw(" ".repeat(x0))];
    nums.extend(nums_row);
    [
        Line::from(nodes_row),
        Line::from(nums),
        Line::from(lane_spans),
    ]
}

// ---------------------------------------------------------------- timeline

struct TLine {
    line: Line<'static>,
    /// Índice del nodo al que pertenece (para seleccionarlo y mantenerlo a la vista).
    node: Option<usize>,
}

/// Une `left` y `right` con puntos guía entre ambos, en `width` columnas.
fn leader(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let width = width as usize;
    let rw = spans_width(&right);
    let left = truncate_spans(left, width.saturating_sub(rw + 3));
    let gap = width.saturating_sub(spans_width(&left) + rw);
    let fill = if gap >= 3 {
        format!(" {} ", "·".repeat(gap - 2))
    } else {
        " ".repeat(gap)
    };
    let mut spans = left;
    spans.push(Span::styled(fill, theme::muted()));
    spans.extend(right);
    Line::from(spans)
}

fn step_right(row: &StepRow) -> Vec<Span<'static>> {
    let (time, style) = match row.info.status {
        StepStatus::Done => {
            let mut t = row.elapsed.map(fmt_duration).unwrap_or_default();
            if row.retries > 0 {
                t = format!("↻{} {t}", row.retries);
            }
            let color = if row.retries > 0 {
                theme::WARN
            } else {
                theme::SECONDARY
            };
            (t, Style::new().fg(color))
        }
        StepStatus::Running => ("en curso".to_string(), Style::new().fg(theme::INFO)),
        StepStatus::Gate => ("en gate".to_string(), Style::new().fg(theme::WARN)),
        StepStatus::Failed => ("falló".to_string(), Style::new().fg(theme::ERR)),
        StepStatus::Skipped => ("omitido".to_string(), theme::muted()),
        StepStatus::Pending => (String::new(), Style::new()),
    };
    vec![
        Span::styled(
            format!("{:<11}", row.info.kind),
            Style::new().fg(theme::tag_color(&row.info.kind)),
        ),
        Span::styled(format!(" {time:>8}"), style),
    ]
}

fn build_timeline(state: &RunState, ns: &[Node], cursor: usize, width: u16) -> Vec<TLine> {
    let mut out: Vec<TLine> = Vec::new();
    let mut lane: Option<String> = None;
    let mut prev_status: Option<StepStatus> = None;
    let failure = match &state.phase {
        Phase::Failed(f) => Some(f),
        _ => None,
    };

    let mut idx = 0;
    while idx < ns.len() {
        let node = ns[idx];
        let i = node.step();
        let row = &state.rows[i];

        // cambio de carril: cierra el anterior y abre el nuevo
        if lane.as_deref() != Some(row.info.target.as_str())
            && matches!(node, Node::Step(_) | Node::Gate(_))
        {
            let is_new_step = idx == 0 || ns[idx - 1].step() != i;
            if is_new_step {
                if lane.is_some() {
                    out.push(TLine {
                        line: Line::from(Span::styled(" ╵", theme::muted())),
                        node: None,
                    });
                }
                if !row.info.target.is_empty() {
                    out.push(TLine {
                        line: Line::from(vec![
                            Span::styled("▸ ", theme::muted()),
                            Span::styled(row.info.target.clone(), theme::bold()),
                        ]),
                        node: None,
                    });
                }
                lane = Some(row.info.target.clone());
                prev_status = None;
            }
        } else if let Some(prev) = prev_status {
            // mismo carril: conector entre pasos (no entre un paso y su propio gate)
            if matches!(node, Node::Step(_)) || is_gate_step(row) {
                out.push(TLine {
                    line: Line::from(Span::styled(" │", Style::new().fg(spine_color(prev)))),
                    node: None,
                });
            }
        }

        let selected = idx == cursor;
        let marker = if selected {
            Span::styled("›", Style::new().fg(theme::INFO))
        } else {
            Span::raw(" ")
        };
        let bg = |l: Line<'static>| {
            if selected {
                l.style(Style::new().bg(theme::SELECTED_BG))
            } else {
                l
            }
        };

        match node {
            Node::Step(_) => {
                let st = row.info.status;
                let left = vec![
                    marker,
                    Span::styled(
                        theme::status_symbol(st),
                        Style::new().fg(theme::status_color(st)),
                    ),
                    Span::raw(" "),
                    Span::styled(row.info.name.clone(), name_style(st)),
                ];
                out.push(TLine {
                    line: bg(leader(left, step_right(row), width)),
                    node: Some(idx),
                });
                if let (Some(f), true) = (
                    failure,
                    state.current == Some(i) && st == StepStatus::Failed,
                ) {
                    out.push(TLine {
                        line: Line::from(vec![
                            Span::styled(" │", Style::new().fg(theme::ERR)),
                            Span::styled(format!("  ✗ {}", f.message), Style::new().fg(theme::ERR)),
                        ]),
                        node: Some(idx),
                    });
                }
                prev_status = Some(st);
            }
            Node::Gate(_) => {
                let (color, state_text) = gate_state(row);
                let gate = row
                    .info
                    .gate
                    .as_ref()
                    .expect("un nodo de gate siempre tiene gate");
                let open = gate_open(state, i, selected);
                let title = if is_gate_step(row) {
                    format!("{} · {}", row.info.name, gate.summary)
                } else {
                    format!("gate {}", gate.summary)
                };
                let count = if !open && !gate.checks.is_empty() {
                    let n = gate.checks.len();
                    format!(" · {n} {}", if n == 1 { "check" } else { "checks" })
                } else {
                    String::new()
                };
                let left = vec![
                    marker,
                    Span::styled("◆", Style::new().fg(color)),
                    Span::raw(" "),
                    Span::styled(title, Style::new()),
                    Span::styled(count, theme::secondary()),
                ];
                let right = vec![Span::styled(state_text, Style::new().fg(color))];
                out.push(TLine {
                    line: bg(leader(left, right, width)),
                    node: Some(idx),
                });
                if open {
                    let label_w = gate
                        .checks
                        .iter()
                        .map(|c| c.label.chars().count())
                        .max()
                        .unwrap_or(0)
                        + 3;
                    let spine = Style::new().fg(spine_color(row.info.status));
                    for c in &gate.checks {
                        let (sym, col) = check_symbol(c);
                        let label = if c.critical {
                            format!("{} ★", c.label)
                        } else {
                            c.label.clone()
                        };
                        let detail = if c.is_new {
                            "nuevo, sin activar".to_string()
                        } else {
                            c.detail.clone()
                        };
                        let mut spans = vec![
                            Span::styled(" │", spine),
                            Span::raw("  "),
                            Span::styled(sym, Style::new().fg(col)),
                            Span::raw(" "),
                            Span::styled(
                                format!("{label:<label_w$}"),
                                if c.is_new {
                                    Style::new().fg(theme::WARN)
                                } else {
                                    Style::new()
                                },
                            ),
                            Span::styled(format!("{:<12}", c.kind), theme::secondary()),
                            Span::styled(
                                detail,
                                if c.is_new {
                                    Style::new().fg(theme::WARN)
                                } else {
                                    theme::secondary()
                                },
                            ),
                        ];
                        spans = truncate_spans(spans, width as usize);
                        out.push(TLine {
                            line: Line::from(spans),
                            node: Some(idx),
                        });
                    }
                }
                // un paso de tipo gate cierra su bloque aquí; uno con gate adjunto ya fijó su estado
                if is_gate_step(row) {
                    prev_status = Some(row.info.status);
                }
            }
        }
        idx += 1;
    }
    if lane.is_some() {
        out.push(TLine {
            line: Line::from(Span::styled(" ╵", theme::muted())),
            node: None,
        });
    }
    out
}

fn name_style(st: StepStatus) -> Style {
    match st {
        StepStatus::Pending | StepStatus::Skipped => theme::muted(),
        _ => Style::new(),
    }
}

// ----------------------------------------------------------------- pantalla

fn status_left(state: &RunState) -> String {
    if state.preview {
        let gates = state.rows.iter().filter(|r| r.info.gate.is_some()).count();
        let mut targets: Vec<&str> = Vec::new();
        for r in &state.rows {
            if !r.info.target.is_empty() && !targets.contains(&r.info.target.as_str()) {
                targets.push(&r.info.target);
            }
        }
        return format!(
            "{} pasos · {gates} gates · {} destinos",
            state.rows.len(),
            targets.len()
        );
    }
    let n = state
        .current
        .map(|c| c + 1)
        .unwrap_or_else(|| state.finished_steps().min(state.total_steps()));
    let name = state
        .current
        .and_then(|c| state.rows.get(c))
        .map_or("", |r| r.info.name.as_str());
    if name.is_empty() {
        format!(
            "{} de {} pasos",
            state.finished_steps(),
            state.total_steps()
        )
    } else {
        format!("Paso {n} de {} · {name}", state.total_steps())
    }
}

fn shortcut_items(state: &RunState) -> Vec<(&'static str, &'static str)> {
    if state.quit_confirm {
        return vec![("s", "sí"), ("n", "no")];
    }
    if state.ask.is_some() {
        return vec![("enter", "continuar"), ("n", "detener")];
    }
    if state.preview {
        return vec![("↑↓", "nodo"), ("espacio", "abrir gate"), ("v", "volver")];
    }
    match state.phase {
        Phase::Running => vec![
            ("↑↓", "nodo"),
            ("espacio", "abrir gate"),
            ("v", "log"),
            ("p", if state.paused { "reanudar" } else { "pausar" }),
            ("r", "rollback"),
            ("s", "saltar gate"),
            ("q", "salir"),
        ],
        Phase::Failed(_) => vec![
            ("↑↓", "nodo"),
            ("espacio", "abrir gate"),
            ("v", "menú de fallo"),
        ],
        Phase::Finished { .. } => vec![
            ("↑↓", "nodo"),
            ("espacio", "abrir gate"),
            ("v", "resumen"),
            ("q", "salir"),
        ],
    }
}

pub fn render(state: &RunState, buf: &mut Buffer, area: Rect) {
    let mut right = Vec::new();
    for (i, b) in state.badges.iter().enumerate() {
        if i > 0 {
            right.push(Span::raw(" "));
        }
        right.push(Span::styled(
            format!("[{}]", b.label),
            Style::new().fg(theme::badge_color(b.tone)),
        ));
    }
    let title_style = match &state.phase {
        Phase::Failed(_) if !state.preview => Style::new().fg(theme::ERR),
        _ => theme::bold(),
    };
    let inner = frame(
        buf,
        area,
        vec![
            Span::styled("◇ ", Style::new().fg(theme::INFO)),
            Span::styled(format!("Pipeline · {}", state.plan), title_style),
        ],
        right,
        theme::border(),
    );

    let items = shortcut_items(state);
    let prompt = prompt_text(state);
    let content_w = inner.width.saturating_sub(2);
    let sc_h = shortcuts_height(&items, content_w.saturating_sub(prompt_width(&prompt))).max(1);
    let sc_y = inner.bottom().saturating_sub(sc_h);
    let sep_low = sc_y.saturating_sub(1);
    let sep_high = inner.y + 4;
    if sep_low < sep_high + 3 {
        return;
    }

    let ns = nodes(&state.rows);
    let cursor = effective_cursor(state, &ns);

    // estado del paso y reloj
    let clock = if state.preview {
        Vec::new()
    } else {
        vec![Span::styled(fmt_clock(state.elapsed), theme::secondary())]
    };
    let mut left = vec![Span::raw(status_left(state))];
    if state.paused {
        left.push(Span::styled(" · en pausa", Style::new().fg(theme::WARN)));
    }
    widgets::justify(left, clock, content_w)
        .render(Rect::new(inner.x + 1, inner.y, content_w, 1), buf);

    // riel
    let rail = rail_lines(state, &ns, cursor, content_w as usize);
    for (i, l) in rail.into_iter().enumerate() {
        l.render(
            Rect::new(inner.x + 1, inner.y + 1 + i as u16, content_w, 1),
            buf,
        );
    }
    hsep(buf, area, sep_high, theme::border());

    // timeline
    let body = pad(Rect::new(
        inner.x,
        sep_high + 1,
        inner.width,
        sep_low - sep_high - 1,
    ));
    let lines = build_timeline(state, &ns, cursor, body.width);
    // si no cabe todo, la última fila se reserva para decir cuánto queda fuera
    let total_h = body.height as usize;
    let view_h = if lines.len() > total_h {
        total_h.saturating_sub(1)
    } else {
        total_h
    };
    let first = lines
        .iter()
        .position(|l| l.node == Some(cursor))
        .unwrap_or(0);
    let last = lines
        .iter()
        .rposition(|l| l.node == Some(cursor))
        .unwrap_or(first);
    let mut top = 0usize;
    if last + 1 > view_h {
        top = last + 1 - view_h;
    }
    top = top.min(first);
    // si abajo solo quedan cierres de carril, se muestran en lugar de avisar que hay más
    if lines[(top + view_h).min(lines.len())..]
        .iter()
        .all(|l| l.node.is_none())
    {
        top = lines.len().saturating_sub(view_h).max(top);
    }
    // deja ver el encabezado del carril (`▸ destino`) si está justo encima
    let is_header =
        |l: &TLine| l.node.is_none() && l.line.spans.first().is_some_and(|s| s.content == "▸ ");
    if top > 0 && lines[top].node.is_some() && lines.get(top - 1).is_some_and(is_header) {
        top -= 1;
    }
    for (n, l) in lines.iter().skip(top).take(view_h).enumerate() {
        // el fondo de selección cubre la fila completa, no solo el texto
        if l.line.style.bg.is_some() {
            buf.set_style(
                Rect::new(body.x - 1, body.y + n as u16, body.width + 2, 1),
                Style::new().bg(theme::SELECTED_BG),
            );
        }
        l.line
            .clone()
            .render(Rect::new(body.x, body.y + n as u16, body.width, 1), buf);
    }
    let below = lines.len().saturating_sub(top + view_h);
    if top > 0 || below > 0 {
        let mut parts = Vec::new();
        if top > 0 {
            parts.push(format!("↑ {top} arriba"));
        }
        if below > 0 {
            parts.push(format!("↓ {below} abajo"));
        }
        Line::from(Span::styled(parts.join(" · "), theme::muted()))
            .right_aligned()
            .render(
                Rect::new(body.x, body.y + view_h as u16, body.width, 1),
                buf,
            );
    }

    hsep(buf, area, sep_low, theme::border());
    let bar = Rect::new(inner.x + 1, sc_y, content_w, sc_h);
    match prompt {
        Some(text) => {
            let pw = prompt_width(&Some(text.clone()));
            Line::from(Span::styled(
                format!("{text} "),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(bar.x, bar.y, pw.min(bar.width), 1), buf);
            let rest = Rect::new(bar.x + pw, bar.y, bar.width.saturating_sub(pw), bar.height);
            widgets::render_shortcuts(buf, rest, &items);
        }
        None => widgets::render_shortcuts(buf, bar, &items),
    }
}
