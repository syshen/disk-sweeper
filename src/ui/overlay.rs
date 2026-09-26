use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::{bar, display_path, pad, popup};
use crate::app::App;
use crate::size::format_size;

const MAX_ERRORS_SHOWN: usize = 200;

pub fn draw_scanning(f: &mut Frame, area: Rect, app: &App) {
    let p = &app.progress;
    let mut lines = vec![
        Line::from(Span::styled(
            " 掃描中…",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            " 已掃描 {} 個項目 · {} · 無法讀取 {}",
            p.files,
            format_size(p.bytes),
            p.errors
        )),
        Line::from(Span::styled(
            format!(" {}", display_path(&p.current, &app.ctx.home)),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];
    let max = p.top.first().map_or(0, |t| t.1);
    for (name, size) in &p.top {
        lines.push(Line::from(format!(
            " {} {:>7} {}",
            pad(name, 30),
            format_size(*size),
            bar(*size, max, 20)
        )));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 掃描 ")),
        area,
    );
}

pub fn draw_running(f: &mut Frame, area: Rect) {
    let rect = popup(area, 40, 20);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(" 執行中，請稍候…")
            .block(Block::default().borders(Borders::ALL).title(" 清理 ")),
        rect,
    );
}

pub fn draw_help(f: &mut Frame, area: Rect) {
    let lines: Vec<Line> = [
        " Explore：瀏覽目錄大小；space 可標記任何項目（未比對到規則的一律移到垃圾桶）",
        " Suggest：依內建規則列出可清理項目；在類別上按 space 整組標記",
        "",
        " ↑↓ / j k    移動",
        " → / l / ⏎   進入目錄（Suggest：展開或收合類別）",
        " ← / h       返回上層",
        " space       標記 / 取消標記",
        " a           顯示 / 隱藏小於 10MB 的建議",
        " tab         切換 Explore / Suggest",
        " d           檢視並執行清理",
        " u           無法讀取的路徑",
        " q / Ctrl-C  離開",
        "",
        " 可重建＝直接刪除 · 使用者資料＝移到垃圾桶 · 工具管理＝執行官方指令 · 受保護＝不可刪除",
        "",
        " 按任意鍵返回",
    ]
    .into_iter()
    .map(Line::from)
    .collect();
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 說明 ")),
        area,
    );
}

pub fn draw_errors(f: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();
    if app.needs_fda {
        lines.push(Line::from(Span::styled(
            " 部分受 macOS 保護的目錄無法讀取。若要完整掃描：系統設定 → 隱私權與安全性 → 完全取用磁碟，加入你的終端機 App 後重新啟動。",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(""));
    }
    for e in app.errors.iter().take(MAX_ERRORS_SHOWN) {
        lines.push(Line::from(format!(
            " {}：{}",
            display_path(&e.path, &app.ctx.home),
            e.message
        )));
    }
    if app.errors.len() > MAX_ERRORS_SHOWN {
        lines.push(Line::from(format!(
            " … 還有 {} 項",
            app.errors.len() - MAX_ERRORS_SHOWN
        )));
    }
    let title = format!(" 無法讀取的路徑（{}） · 按任意鍵返回 ", app.errors.len());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}
