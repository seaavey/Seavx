use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use thinking_orbs_rs::{OrbAnimator, OrbConfig, OrbSize, OrbState};

use super::theme;
use crate::tui::app::App;
use crate::tui::event::AgentState;

/// Cells the orb takes on the session line. The crate draws on a canvas many
/// rows tall, so one row of it, five cells wide, is what a single line shows.
const ORB_CELLS: usize = 5;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame, millis: u128) {
    let dim = theme::dim();
    let title = app.session_title.as_deref().unwrap_or("new session");
    // The session line says what the agent is doing, never which command it is
    // running: the transcript carries the call and `/status` names it.
    let (state, state_style) = match &app.status {
        AgentState::Idle => ("ready", dim),
        AgentState::Thinking => ("thinking", Style::default().fg(Color::Yellow)),
        AgentState::Checking => ("checking", Style::default().fg(Color::Yellow)),
        AgentState::Compacting => ("compacting", Style::default().fg(Color::Yellow)),
        AgentState::Streaming => ("streaming", Style::default().fg(Color::Yellow)),
        AgentState::ExecutingTool(_) => ("working", Style::default().fg(Color::Cyan)),
    };
    let orb_style = match app.status {
        AgentState::Idle => dim,
        _ => Style::default().fg(Color::Cyan),
    };
    let used = title.chars().count() + state.chars().count() + ORB_CELLS + 7;
    let fill = "─".repeat((area.width as usize).saturating_sub(used));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─ ", dim),
            Span::styled(title.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(format!(" {fill} "), dim),
            Span::styled(orb_cells(&app.status, millis), orb_style),
            Span::styled(format!(" {state}"), state_style),
            Span::styled(" ─", dim),
        ])),
        area,
    );
}

/// One frame of the crate's own animation, folded into the one row the line has.
///
/// The canvas is fourteen rows tall, so the five cells track the heaviest
/// particle: where it sits across the canvas picks the cell, its weight picks
/// the glyph. The orb still travels and pulses, in one row.
pub(crate) fn orb_cells(state: &AgentState, millis: u128) -> String {
    if matches!(state, AgentState::Idle) {
        // The crate's idle drifts for ever; a finished turn must not look busy.
        return " ○ ".to_string();
    }
    let mut animator = OrbAnimator::new(OrbConfig {
        size: OrbSize::Compact,
        ..OrbConfig::default()
    });
    animator.set_state(orb_state(state));
    let frame = animator.frame_at(millis as f64 / 1000.0);
    let Some(heaviest) = frame
        .particles
        .iter()
        .max_by(|left, right| left.visual_weight().total_cmp(&right.visual_weight()))
    else {
        return " · ".to_string();
    };
    let mut cells = [' '; ORB_CELLS];
    let across = heaviest.x / f32::from(frame.width.max(1));
    let slot = ((across * ORB_CELLS as f32) as usize).min(ORB_CELLS - 1);
    cells[slot] = glyph(heaviest.visual_weight());
    cells.iter().collect()
}

/// The renderer's own ramp, so a one-row orb uses the same glyphs.
fn glyph(weight: f32) -> char {
    match weight {
        weight if weight < 0.22 => '·',
        weight if weight < 0.48 => '∘',
        weight if weight < 0.74 => '•',
        _ => '●',
    }
}

/// The crate's state for what the agent is doing.
fn orb_state(state: &AgentState) -> OrbState {
    match state {
        AgentState::Idle => OrbState::Idle,
        AgentState::Thinking => OrbState::Breathing,
        AgentState::Streaming => OrbState::Composing,
        AgentState::Checking => OrbState::Connecting,
        AgentState::Compacting => OrbState::Shaping,
        AgentState::ExecutingTool(_) => OrbState::Working,
    }
}
