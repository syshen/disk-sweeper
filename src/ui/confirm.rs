use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{display_path, pad, popup, risk_style};
use crate::app::{App, ConfirmItem};
use crate::risk::Handling;
use crate::rules::CATALOG;
use crate::safety::Verdict;
use crate::size::format_size;

/// 一個項目在清單中佔的行（第一行是項目本身，第二行是規則說明）
fn item_lines(item: &ConfirmItem, app: &App) -> Vec<Line<'static>> {
    match &item.verdict {
        Verdict::Approved(t) => {
            let what = match t.handling() {
                Handling::Command(cmd) => format!("執行 {}", cmd.display()),
                h => h.label().to_string(),
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(" ✓ ", Style::default().fg(Color::Green)),
                Span::styled(pad(item.risk.label(), 10), risk_style(item.risk)),
                Span::raw(format!(
                    " {} {:>7}  {}",
                    pad(&what, 12),
                    format_size(t.size()),
                    display_path(t.path(), &app.ctx.home)
                )),
            ])];
            let desc = t
                .rule_id()
                .and_then(|id| CATALOG.iter().find(|r| r.id == id))
                .map(|r| r.description);
            if let Some(desc) = desc {
                lines.push(Line::from(Span::styled(
                    format!("     {desc}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )));
            }
            lines
        }
        Verdict::Rejected { path, reason } => vec![Line::from(Span::styled(
            format!(" ✗ 略過 {}：{reason}", display_path(path, &app.ctx.home)),
            Style::default().fg(Color::Red),
        ))],
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = &app.confirm else { return };
    let rect = popup(area, 90, 80);
    let title = if app.dry_run {
        " 確認清理（模擬模式，不會變更檔案） "
    } else {
        " 確認清理 "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let (mut count, mut total) = (0usize, 0u64);
    for item in &state.items {
        if let Verdict::Approved(t) = &item.verdict {
            count += 1;
            total += t.size();
        }
    }

    // 底部固定區：捲動提示（或空行）、摘要、所有警告、提示輸入——永遠不會被裁掉
    let bottom_h = (3 + state.warnings.len() as u16).min(inner.height);
    let list_h = inner.height - bottom_h;

    let n = state.items.len();
    let start = state.scroll.min(n.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    let mut end = start;
    for item in &state.items[start.min(n)..] {
        let il = item_lines(item, app);
        if lines.len() + il.len() > list_h as usize {
            break;
        }
        lines.extend(il);
        end += 1;
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            height: list_h,
            ..inner
        },
    );

    let indicator = if n > 0 && (start > 0 || end < n) {
        Line::from(Span::styled(
            format!(
                " 第 {}–{} 項，共 {n} 項（↑↓ 捲動）",
                start + 1,
                end.max(start + 1)
            ),
            Style::default().fg(Color::Cyan),
        ))
    } else {
        Line::from("")
    };
    let mut bottom = vec![
        indicator,
        Line::from(Span::styled(
            format!(" 將處理 {count} 項，共 {}", format_size(total)),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    for w in &state.warnings {
        bottom.push(Line::from(Span::styled(
            format!(" ⚠ {w}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    let prompt = if state.need_typed {
        format!(" 輸入 yes 並按 Enter 執行（Esc 取消）：{}_", state.input)
    } else {
        " 按 y 執行，n 或 Esc 取消".to_string()
    };
    bottom.push(Line::from(Span::styled(
        prompt,
        Style::default().add_modifier(Modifier::BOLD),
    )));
    // 空間不夠時優先保留最後面的提示行
    let skip = bottom.len().saturating_sub(bottom_h as usize);
    f.render_widget(
        Paragraph::new(bottom.split_off(skip)),
        Rect {
            y: inner.y + list_h,
            height: bottom_h,
            ..inner
        },
    );
}
