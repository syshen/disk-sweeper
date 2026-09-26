mod confirm;
mod explore;
mod overlay;
mod result;
mod suggest;

use std::path::Path;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Mode, Screen};
use crate::risk::Risk;
use crate::size::format_size;

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(f.area());
    draw_header(f, header, app);
    if app.tree.is_none() {
        overlay::draw_scanning(f, body, app);
    } else {
        match app.mode {
            Mode::Explore => explore::draw(f, body, app),
            Mode::Suggest => suggest::draw(f, body, app),
        }
    }
    draw_footer(f, footer, app);
    match app.screen {
        Screen::Main => {}
        Screen::Confirm => confirm::draw(f, body, app),
        Screen::Running => overlay::draw_running(f, body),
        Screen::Result => result::draw(f, body, app),
        Screen::Help => overlay::draw_help(f, body),
        Screen::Errors => overlay::draw_errors(f, body, app),
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let root = app.tree.as_ref().map_or_else(
        || "…".to_string(),
        |t| display_path(t.root_path(), &app.ctx.home),
    );
    let disk = app
        .disk
        .map(|d| {
            format!(
                " · 共 {} · 可用 {}",
                format_size(d.total),
                format_size(d.free)
            )
        })
        .unwrap_or_default();
    let tab = |m: Mode, label: &'static str| {
        if app.mode == m {
            Span::styled(
                format!("[{label}]"),
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            )
        } else {
            Span::raw(format!(" {label} "))
        }
    };
    let mut spans = vec![
        Span::styled(
            " disk-sweeper ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{root}{disk}  ")),
        tab(Mode::Explore, "Explore"),
        Span::raw(" "),
        tab(Mode::Suggest, "Suggest"),
    ];
    if app.dry_run {
        spans.push(Span::styled(
            "  模擬模式",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut info = vec![Span::raw(format!(
        " 已標記 {} 項 · {}",
        app.marks.len(),
        format_size(app.marked_total())
    ))];
    if !app.errors.is_empty() {
        info.push(Span::styled(
            format!("   ⚠ {} 個路徑無法讀取 (u)", app.errors.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.needs_fda {
        info.push(Span::styled(
            "   需要「完全取用磁碟」權限 (u)",
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(s) = &app.status {
        info.push(Span::styled(
            format!("   {s}"),
            Style::default().fg(Color::Cyan),
        ));
    }
    let keys = match app.mode {
        Mode::Explore => {
            " ↑↓/jk 移動  →/l 進入  ←/h 返回  space 標記  tab 切換  d 清理  ? 說明  q 離開"
        }
        Mode::Suggest => {
            " ↑↓/jk 移動  ⏎ 展開  space 標記  a 小項目  tab 切換  d 清理  ? 說明  q 離開"
        }
    };
    let lines = vec![
        Line::from(info),
        Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn risk_style(r: Risk) -> Style {
    Style::default().fg(match r {
        Risk::Regenerable => Color::Green,
        Risk::UserData => Color::Yellow,
        Risk::Protected => Color::Red,
        Risk::ToolManaged => Color::Cyan,
    })
}

pub(crate) fn bar(size: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 {
        0
    } else {
        ((size as f64 / total as f64) * width as f64).round() as usize
    };
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// 依終端機顯示寬度補空白或截斷（以 … 結尾），CJK 字元算兩欄。
pub(crate) fn pad(s: &str, width: usize) -> String {
    let w = s.width();
    if w <= width {
        return format!("{s}{}", " ".repeat(width - w));
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > width {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    used += 1;
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}

pub(crate) fn display_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

pub(crate) fn popup(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let [_, v, _] = Layout::vertical([
        Constraint::Percentage((100 - height_pct) / 2),
        Constraint::Percentage(height_pct),
        Constraint::Min(0),
    ])
    .areas(area);
    let [_, h, _] = Layout::horizontal([
        Constraint::Percentage((100 - width_pct) / 2),
        Constraint::Percentage(width_pct),
        Constraint::Min(0),
    ])
    .areas(v);
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{ActionResult, Outcome};
    use crate::app::{ConfirmItem, ConfirmState};
    use crate::context::Context;
    use crate::disk::DiskInfo;
    use crate::risk::Handling;
    use crate::safety::{self, Candidate, Verdict};
    use crate::scan::{Progress, ScanEvent, ScanResult};
    use crate::tree::testutil::{find, tree_from};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    fn base_app() -> App {
        let ctx = Context {
            home: PathBuf::from("/h"),
            home_dev: 0,
            path_var: OsString::new(),
        };
        App::new(
            ctx,
            false,
            Some(DiskInfo {
                total: 228 << 30,
                free: 10 << 30,
            }),
            1_800_000_000,
        )
    }

    fn sample_app() -> App {
        let mut app = base_app();
        let tree = tree_from(
            "/h",
            &[
                ("Library/Caches/com.spotify.client/data", 3 << 30),
                ("Projects/web/package.json", 4096),
                ("Projects/web/node_modules/react/index.js", 800 << 20),
                ("Downloads/中文 安裝檔.dmg", 1 << 30),
                ("Movies/raw.mov", 2 << 30),
            ],
        );
        app.on_scan_event(ScanEvent::Done(ScanResult {
            tree,
            errors: vec![],
            elapsed: Duration::ZERO,
        }));
        app.mode = Mode::Explore; // 快照以 Explore 為底層畫面
        app
    }

    fn render(app: &App) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        t
    }

    #[test]
    fn pad_uses_display_width() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("中文", 6), "中文  ");
        // 輸出寬度一律剛好等於 width：「中文」4 欄 + 「…」1 欄 + 補 1 個空白
        assert_eq!(pad("中文字很長", 6), "中文… ");
        assert_eq!(pad("中文字很長", 6).width(), 6);
    }

    #[test]
    fn bar_is_proportional() {
        assert_eq!(bar(5, 10, 4), "██░░");
        assert_eq!(bar(1, 0, 3), "░░░");
    }

    #[test]
    fn display_path_abbreviates_home() {
        let home = Path::new("/h");
        assert_eq!(display_path(Path::new("/h"), home), "~");
        assert_eq!(display_path(Path::new("/h/a b"), home), "~/a b");
        assert_eq!(display_path(Path::new("/etc"), home), "/etc");
    }

    #[test]
    fn scanning_screen() {
        let mut app = base_app();
        app.on_scan_event(ScanEvent::Progress(Progress {
            files: 12_345,
            bytes: 5 << 30,
            errors: 2,
            current: "/h/Library/Caches".into(),
            top: vec![("Library".into(), 3 << 30), ("Projects".into(), 1 << 30)],
        }));
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn explore_screen() {
        insta::assert_snapshot!(render(&sample_app()).backend());
    }

    #[test]
    fn suggest_screen() {
        let mut app = sample_app();
        app.mode = Mode::Suggest;
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn confirm_screen_shows_rejections_and_warnings() {
        let mut app = sample_app();
        let lib = find(app.tree.as_ref().unwrap(), "Library");
        app.confirm = Some(ConfirmState {
            items: vec![ConfirmItem {
                node: lib,
                risk: Risk::UserData,
                verdict: Verdict::Rejected {
                    path: "/h/Library".into(),
                    reason: "系統或使用者的根目錄本身不可刪除".into(),
                },
            }],
            warnings: vec!["Xcode 正在執行，建議先關閉再清理".into()],
            need_typed: true,
            input: "ye".into(),
            scroll: 0,
        });
        app.screen = Screen::Confirm;
        insta::assert_snapshot!(render(&app).backend());
    }

    /// 在真的 temp home 裡建立檔案並經過 safety::review，產生可顯示的核准項目。
    /// 畫面上的路徑一律以 `~/...` 顯示，所以快照與 temp 目錄位置無關。
    fn confirm_app_with_many_items(home: &Path, scroll: usize) -> App {
        let ctx = Context::new(home, OsString::new()).unwrap();
        let mut app = App::new(ctx, false, None, 1_800_000_000);
        let tree = tree_from("/h", &[("Projects/x", 1)]);
        let node = tree.root();
        app.on_scan_event(ScanEvent::Done(ScanResult {
            tree,
            errors: vec![],
            elapsed: Duration::ZERO,
        }));
        app.mode = Mode::Explore; // 快照以 Explore 為底層畫面
        let mut items = Vec::new();
        for i in 0..22 {
            let p = app
                .ctx
                .home
                .join(format!("Projects/app{i:02}/node_modules"));
            std::fs::create_dir_all(&p).unwrap();
            let (risk, rule) = if i % 3 == 0 {
                (Risk::UserData, None)
            } else {
                (Risk::Regenerable, Some("node-modules"))
            };
            let c = Candidate {
                path: p,
                risk,
                rule_id: rule,
                command: None,
                size: (i + 1) << 20,
                inode: 0,
                mtime: 0,
            };
            let verdict = safety::review(c, &app.ctx);
            assert!(matches!(verdict, Verdict::Approved(_)), "{verdict:?}");
            items.push(ConfirmItem {
                node,
                risk,
                verdict,
            });
        }
        items.push(ConfirmItem {
            node,
            risk: Risk::UserData,
            verdict: Verdict::Rejected {
                path: app.ctx.home.join("Library"),
                reason: "系統或使用者的根目錄本身不可刪除".into(),
            },
        });
        app.confirm = Some(ConfirmState {
            items,
            warnings: vec![
                "Xcode 正在執行，建議先關閉再清理".into(),
                "Docker Desktop 未執行，docker 指令會失敗".into(),
            ],
            need_typed: true,
            input: String::new(),
            scroll,
        });
        app.screen = Screen::Confirm;
        app
    }

    #[test]
    fn confirm_screen_with_many_items_scrolls_and_keeps_prompt() {
        let home = tempfile::tempdir().unwrap();
        let app = confirm_app_with_many_items(home.path(), 0);
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn confirm_screen_scrolled_to_the_end_shows_last_item() {
        let home = tempfile::tempdir().unwrap();
        let app = confirm_app_with_many_items(home.path(), 22);
        let text = format!("{}", render(&app).backend());
        assert!(text.contains("第 23–23 項，共 23 項"), "{text}");
        assert!(
            text.contains("~/Library：系統或使用者的根目錄本身不可刪除"),
            "{text}"
        );
        assert!(text.contains("輸入 yes 並按 Enter 執行"), "{text}");
    }

    #[test]
    fn scanning_screen_shows_permission_hint_when_stalled() {
        let mut app = base_app();
        assert!(!format!("{}", render(&app).backend()).contains("權限對話框"));
        app.last_progress_change -= Duration::from_secs(10);
        let text = format!("{}", render(&app).backend());
        assert!(text.contains("可能正在等待 macOS 權限對話框"), "{text}");
    }

    #[test]
    fn running_screen_mentions_finder_automation_prompt() {
        let mut app = sample_app();
        app.screen = Screen::Running;
        let text = format!("{}", render(&app).backend());
        assert!(text.contains("執行中，請稍候"), "{text}");
        assert!(
            text.contains("第一次移到垃圾桶時，macOS 可能會詢問是否允許控制 Finder"),
            "{text}"
        );
    }

    #[test]
    fn result_screen() {
        let mut app = sample_app();
        app.results = vec![
            ActionResult {
                path: "/h/Projects/web/node_modules".into(),
                size: 800 << 20,
                handling: Handling::Delete,
                outcome: Outcome::Done,
                output: None,
            },
            ActionResult {
                path: "/h/Downloads/中文 安裝檔.dmg".into(),
                size: 1 << 30,
                handling: Handling::Trash,
                outcome: Outcome::Skipped("掃描後已變動".into()),
                output: None,
            },
        ];
        app.freed = Some(800 << 20);
        app.screen = Screen::Result;
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn dry_run_confirm_screen_uses_localized_label_not_english() {
        let ctx = Context {
            home: PathBuf::from("/h"),
            home_dev: 0,
            path_var: OsString::new(),
        };
        let mut app = App::new(
            ctx,
            true,
            Some(DiskInfo {
                total: 228 << 30,
                free: 10 << 30,
            }),
            1_800_000_000,
        );
        let tree = tree_from(
            "/h",
            &[("Projects/web/node_modules/react/index.js", 800 << 20)],
        );
        app.on_scan_event(ScanEvent::Done(ScanResult {
            tree,
            errors: vec![],
            elapsed: Duration::ZERO,
        }));
        app.confirm = Some(ConfirmState {
            items: vec![],
            warnings: vec![],
            need_typed: false,
            input: String::new(),
            scroll: 0,
        });
        app.screen = Screen::Confirm;
        let terminal = render(&app);
        let rendered = format!("{}", terminal.backend());
        assert!(rendered.contains("模擬模式"), "{rendered}");
        assert!(!rendered.contains("DRY-RUN"), "{rendered}");
    }
}
