use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::{display_path, pad, popup};
use crate::app::App;
use crate::risk::Handling;
use crate::safety::Verdict;
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = &app.confirm else { return };
    let rect = popup(area, 90, 80);
    let max_items = rect.height.saturating_sub(7) as usize;

    let mut item_lines: Vec<Line> = Vec::new();
    let (mut count, mut total) = (0usize, 0u64);
    for (_, v) in &state.items {
        match v {
            Verdict::Approved(t) => {
                count += 1;
                total += t.size();
                let what = match t.handling() {
                    Handling::Command(cmd) => format!("執行 {}", cmd.display()),
                    h => h.label().to_string(),
                };
                item_lines.push(Line::from(vec![
                    Span::styled(
                        format!(" ✓ {} ", pad(&what, 12)),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(format!(
                        "{:>7}  {}",
                        format_size(t.size()),
                        display_path(t.path(), &app.ctx.home)
                    )),
                ]));
            }
            Verdict::Rejected { path, reason } => item_lines.push(Line::from(Span::styled(
                format!(" ✗ 略過 {}：{reason}", display_path(path, &app.ctx.home)),
                Style::default().fg(Color::Red),
            ))),
        }
    }
    let hidden = item_lines.len().saturating_sub(max_items);
    item_lines.truncate(max_items);
    let mut lines = item_lines;
    if hidden > 0 {
        lines.push(Line::from(format!(" … 還有 {hidden} 項")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" 將處理 {count} 項，共 {}", format_size(total)),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for w in &state.warnings {
        lines.push(Line::from(Span::styled(
            format!(" ⚠ {w}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    let prompt = if state.need_typed {
        format!(" 輸入 yes 並按 Enter 執行（Esc 取消）：{}_", state.input)
    } else {
        " 按 y 執行，n 或 Esc 取消".to_string()
    };
    lines.push(Line::from(Span::styled(
        prompt,
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let title = if app.dry_run {
        " 確認清理（模擬模式，不會變更檔案） "
    } else {
        " 確認清理 "
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        rect,
    );
}
