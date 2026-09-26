use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::{display_path, pad, risk_style};
use crate::app::{App, SuggestRow};
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(tree) = &app.tree else { return };
    let [list_area, detail_area] = Layout::vertical([Constraint::Min(0), Constraint::Length(4)]).areas(area);
    let rows = app.suggest_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match *row {
            SuggestRow::Header(cat) => {
                let (count, total) =
                    app.visible_items(cat).fold((0usize, 0u64), |(c, t), i| (c + 1, t + app.suggestions[i].size));
                let open = if app.expanded[cat as usize] { "▾" } else { "▸" };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{open} {}", cat.label()), Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  {count} 項 · {}", format_size(total))),
                ]))
            }
            SuggestRow::Item(i) => {
                let s = &app.suggestions[i];
                let mark = if app.marks.contains_key(&s.node) { "✓" } else { " " };
                let path = display_path(&tree.path_of(s.node), &app.ctx.home);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("   {mark} ")),
                    Span::raw(pad(&path, 56)),
                    Span::raw(format!(" {:>7}  ", format_size(s.size))),
                    Span::styled(s.risk.label(), risk_style(s.risk)),
                ]))
            }
        })
        .collect();
    let hint = if app.show_small { "a：隱藏小於 10MB 的項目" } else { "a：顯示小於 10MB 的項目" };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(" 清理建議 · {hint} ")))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(app.suggest_cursor));
    f.render_stateful_widget(list, list_area, &mut state);

    let detail = match rows.get(app.suggest_cursor) {
        Some(SuggestRow::Item(i)) => {
            let s = &app.suggestions[*i];
            let mut lines = vec![Line::from(format!("{} · {}", s.rule_id, s.description))];
            if let Some(cmd) = s.command {
                lines.push(Line::from(format!("官方指令：{}", cmd.display())));
            }
            lines
        }
        _ => vec![Line::from("選取項目以查看說明；在類別上按 space 可整組標記")],
    };
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)),
        detail_area,
    );
}
