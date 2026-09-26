use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{display_path, pad, popup, risk_style, wrap_to_width};
use crate::app::{App, ConfirmItem};
use crate::risk::Handling;
use crate::rules::CATALOG;
use crate::safety::Verdict;
use crate::size::format_size;

/// 路徑、說明與拒絕原因的縮排
const INDENT: &str = "     ";

/// 依寬度折行並加上縮排；每一行都完整顯示，不會被右邊界切掉
fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let avail = width.saturating_sub(INDENT.len());
    wrap_to_width(text, avail)
        .into_iter()
        .map(|seg| Line::from(Span::styled(format!("{INDENT}{seg}"), style)))
        .collect()
}

/// 一個項目在清單中佔的行：第一行是風險／處理方式／大小，接著是完整路徑，最後是規則說明
fn item_lines(item: &ConfirmItem, app: &App, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    match &item.verdict {
        Verdict::Approved(t) => {
            let what = match t.handling() {
                Handling::Command(cmd) => format!("執行 {}", cmd.display()),
                h => h.label().to_string(),
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(" ✓ ", Style::default().fg(Color::Green)),
                Span::styled(pad(item.risk.label(), 10), risk_style(item.risk)),
                Span::raw(format!(" {} {:>7}", pad(&what, 12), format_size(t.size()))),
            ])];
            lines.extend(wrapped(
                &display_path(t.path(), &app.ctx.home),
                width,
                Style::default(),
            ));
            let desc = t
                .rule_id()
                .and_then(|id| CATALOG.iter().find(|r| r.id == id))
                .map(|r| r.description);
            if let Some(desc) = desc {
                lines.extend(wrapped(desc, width, dim));
            }
            lines
        }
        Verdict::Rejected { path, reason } => {
            let red = Style::default().fg(Color::Red);
            let mut lines = Vec::new();
            for (i, seg) in wrap_to_width(&format!("略過：{reason}"), width.saturating_sub(3))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 { " ✗ " } else { "   " };
                lines.push(Line::from(Span::styled(format!("{lead}{seg}"), red)));
            }
            lines.extend(wrapped(&display_path(path, &app.ctx.home), width, red));
            lines
        }
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
        let il = item_lines(item, app, inner.width as usize);
        if lines.len() + il.len() > list_h as usize {
            // 第一個項目就比清單區還高時，仍畫出放得下的部分，避免畫面空白
            if end == start {
                lines.extend(il.into_iter().take(list_h as usize));
                end += 1;
            }
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

    let indicator = if n > 0 && list_h == 0 {
        Line::from(Span::styled(
            " 視窗太小，無法顯示項目（請放大終端機）",
            Style::default().fg(Color::Yellow),
        ))
    } else if n > 0 && (start > 0 || end < n) {
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
