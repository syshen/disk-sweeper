use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::{bar, display_path, pad, risk_style};
use crate::app::App;
use crate::risk::Risk;
use crate::safety;
use crate::size::format_size;
use crate::tree::NodeKind;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let (Some(tree), Some(&(dir, cursor))) = (&app.tree, app.explore.last()) else {
        return;
    };
    let dir_path = tree.path_of(dir);
    let total = tree.node(dir).size;
    let items: Vec<ListItem> = app
        .explore_children()
        .into_iter()
        .map(|id| {
            let n = tree.node(id);
            let mark = if app.marks.contains_key(&id) {
                "✓"
            } else {
                " "
            };
            let arrow = if n.kind == NodeKind::Dir { "▸" } else { " " };
            let mut spans = vec![
                Span::raw(format!("{mark}{arrow} ")),
                Span::raw(pad(&n.name, 36)),
                Span::raw(format!(" {:>7} ", format_size(n.size))),
                Span::raw(bar(n.size, total, 20)),
            ];
            if let Some(s) = app.suggestion_for(id) {
                spans.push(Span::styled(
                    format!("  {} · {}", s.risk.label(), s.rule_id),
                    risk_style(s.risk),
                ));
            } else if safety::protection_reason(&dir_path.join(&*n.name), &app.ctx).is_some() {
                spans.push(Span::styled("  受保護", risk_style(Risk::Protected)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = format!(
        " {} · {} ",
        display_path(&dir_path, &app.ctx.home),
        format_size(total)
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(cursor));
    f.render_stateful_widget(list, area, &mut state);
}
