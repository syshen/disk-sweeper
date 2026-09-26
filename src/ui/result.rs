use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{display_path, popup};
use crate::action::Outcome;
use crate::app::App;
use crate::risk::Handling;
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let rect = popup(area, 90, 80);
    let count = |pred: fn(&Outcome) -> bool| app.results.iter().filter(|r| pred(&r.outcome)).count();
    let done = count(|o| matches!(o, Outcome::Done));
    let dry = count(|o| matches!(o, Outcome::DryRun));
    let skipped = count(|o| matches!(o, Outcome::Skipped(_)));
    let failed = count(|o| matches!(o, Outcome::Failed(_)));

    let dry_note = if dry > 0 { format!(" · 模擬 {dry}") } else { String::new() };
    let mut lines = vec![Line::from(Span::styled(
        format!(" 完成 {done} · 略過 {skipped} · 失敗 {failed}{dry_note}"),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    if let Some(freed) = app.freed {
        let sign = if freed < 0 { "-" } else { "+" };
        lines.push(Line::from(format!(" 可用空間變化：{sign}{}", format_size(freed.unsigned_abs()))));
    }
    if app.results.iter().any(|r| r.handling == Handling::Trash && r.outcome == Outcome::Done) {
        lines.push(Line::from(Span::styled(
            " 移到垃圾桶的項目要清空垃圾桶後才會釋放空間",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    for r in &app.results {
        let (label, color, detail) = match &r.outcome {
            Outcome::Done => ("完成", Color::Green, String::new()),
            Outcome::DryRun => ("模擬", Color::Magenta, String::new()),
            Outcome::Skipped(m) => ("略過", Color::Yellow, format!("：{m}")),
            Outcome::Failed(m) => ("失敗", Color::Red, format!("：{m}")),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {label} "), Style::default().fg(color)),
            Span::raw(format!("{}{detail}", display_path(&r.path, &app.ctx.home))),
        ]));
        if let Some(out) = &r.output {
            for l in out.lines().take(5) {
                lines.push(Line::from(Span::styled(format!("      {l}"), Style::default().fg(Color::DarkGray))));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(" 按任意鍵返回"));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 清理結果 ")), rect);
}
