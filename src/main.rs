use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use disk_sweeper::action::{self, journal::ActionLog, SystemTrash};
use disk_sweeper::app::{App, Effect};
use disk_sweeper::context::Context;
use disk_sweeper::docker::{self, SystemDocker};
use disk_sweeper::safety::{self, ApprovedDocker, ApprovedTarget};
use disk_sweeper::scan::{self, ScanEvent, ScanOptions};
use disk_sweeper::tree::NodeId;
use disk_sweeper::worktree::{self, Worktree};
use disk_sweeper::{disk, size, ui};

#[derive(Parser)]
#[command(version, about = "Mac 硬碟空間清理器（TUI）")]
struct Cli {
    /// 要掃描的目錄（預設 $HOME，必須在 $HOME 之下）
    path: Option<PathBuf>,
    /// 不實際變更任何檔案
    #[arg(long)]
    dry_run: bool,
    /// 小於此大小的檔案不建立節點，以節省記憶體（例：64K）
    #[arg(long, value_parser = size::parse_size)]
    min_node_size: Option<u64>,
    /// 只掃描並印出摘要（量測效能用），不啟動 TUI
    #[arg(long)]
    scan_only: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Context::from_env().context("無法取得 $HOME")?;
    let root = match &cli.path {
        Some(p) => p
            .canonicalize()
            .with_context(|| format!("找不到路徑 {}", p.display()))?,
        None => ctx.home.clone(),
    };
    if !root.starts_with(&ctx.home) {
        bail!("PATH 必須在 $HOME（{}）之下", ctx.home.display());
    }
    let opts = ScanOptions {
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        min_node_size: cli.min_node_size.unwrap_or(0),
    };

    if cli.scan_only {
        let (tx, _rx) = mpsc::channel();
        let r = scan::scan(&root, &opts, &tx);
        let n = r.tree.node(r.tree.root());
        println!(
            "{} 個項目 · {} · 無法讀取 {} · 耗時 {:.2} 秒",
            n.file_count,
            size::format_size(n.size),
            r.errors.len(),
            r.elapsed.as_secs_f64()
        );
        let git = safety::find_in_path("git", &ctx.path_var);
        for &id in &r.git_dirs {
            let path = r.tree.path_of(id);
            if let Some((branch, status)) = worktree::inspect(&path, git.as_deref()) {
                let wt = Worktree {
                    node: id,
                    branch,
                    status,
                };
                let tag = if wt.removable() {
                    "可移除"
                } else {
                    "保留"
                };
                println!(
                    "[worktree {tag}] {} {} · {}",
                    size::format_size(r.tree.node(id).size),
                    path.display(),
                    wt.description()
                );
            }
        }
        let docker_bin = safety::find_in_path("docker", &ctx.path_var);
        match docker::scan(docker_bin.as_deref(), &SystemDocker) {
            docker::Scan::Ready { context, items } => {
                for item in &items {
                    let tag = if item.removable() {
                        "可移除"
                    } else {
                        "保留"
                    };
                    println!(
                        "[docker {context} {tag}] {} {} · {}",
                        size::format_size(item.size),
                        item.label(),
                        item.description
                    );
                }
            }
            docker::Scan::Unavailable(reason) => println!("[docker] {reason}"),
        }
        return Ok(());
    }

    let rx = spawn_scan(&root, &opts, &ctx);
    let now = chrono::Local::now().timestamp();
    let mut app = App::new(ctx, cli.dry_run, disk::disk_info(&root).ok(), now);
    app.expect_docker();
    // ratatui::init 會安裝 panic hook，panic 時也會恢復終端機
    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, rx, &root, &opts);
    ratatui::restore();
    res
}

/// 背景掃描；完成後先送出 `Done`，再逐一用 git 檢查 worktree 候選、送出 `Worktrees`。
/// 同時另開一個執行緒問 docker，查到就送出 `Docker`（通常比目錄掃描早完成）。
fn spawn_scan(root: &Path, opts: &ScanOptions, ctx: &Context) -> mpsc::Receiver<ScanEvent> {
    let (tx, rx) = mpsc::channel();
    let (root, opts) = (root.to_path_buf(), opts.clone());
    let git = safety::find_in_path("git", &ctx.path_var);
    let docker_bin = safety::find_in_path("docker", &ctx.path_var);
    let docker_tx = tx.clone();
    std::thread::spawn(move || {
        let _ = docker_tx.send(ScanEvent::Docker(docker::scan(
            docker_bin.as_deref(),
            &SystemDocker,
        )));
    });
    std::thread::spawn(move || {
        let result = scan::scan(&root, &opts, &tx);
        let candidates: Vec<(NodeId, PathBuf)> = result
            .git_dirs
            .iter()
            .map(|&id| (id, result.tree.path_of(id)))
            .collect();
        if tx.send(ScanEvent::Done(result)).is_err() {
            return;
        }
        let worktrees = candidates
            .into_iter()
            .filter_map(|(node, path)| {
                let (branch, status) = worktree::inspect(&path, git.as_deref())?;
                Some(Worktree {
                    node,
                    branch,
                    status,
                })
            })
            .collect();
        let _ = tx.send(ScanEvent::Worktrees(worktrees));
    });
    rx
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: mpsc::Receiver<ScanEvent>,
    root: &Path,
    opts: &ScanOptions,
) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        while let Ok(ev) = rx.try_recv() {
            app.on_scan_event(ev);
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.on_key(key) {
            Some(Effect::Execute(targets, docker)) => {
                terminal.draw(|f| ui::draw(f, app))?; // 先畫出「執行中」
                execute(app, targets, docker, root);
                // 清理期間按下的鍵都丟掉，免得一回來就把結果畫面關掉
                while event::poll(Duration::ZERO)? {
                    let _ = event::read()?;
                }
            }
            Some(Effect::Rescan) => {
                let now = chrono::Local::now().timestamp();
                app.begin_rescan(disk::disk_info(root).ok(), now);
                app.expect_docker();
                rx = spawn_scan(root, opts, &app.ctx);
            }
            None => {}
        }
    }
    Ok(())
}

fn execute(
    app: &mut App,
    targets: Vec<(NodeId, ApprovedTarget)>,
    docker: Vec<ApprovedDocker>,
    root: &Path,
) {
    let mut log = match ActionLog::open(&app.ctx.log_dir()) {
        Ok(l) => Some(l),
        Err(_) if app.dry_run => None,
        Err(e) => {
            app.on_execute_error(format!("無法寫入操作紀錄（{e}），已取消執行"));
            return;
        }
    };
    let before = disk::disk_info(root).ok();
    let (ids, approved): (Vec<NodeId>, Vec<ApprovedTarget>) = targets.into_iter().unzip();
    let results = action::execute(&approved, app.dry_run, log.as_mut(), &SystemTrash);
    let docker_results = action::execute_docker(&docker, app.dry_run, log.as_mut(), &SystemDocker);
    let docker_done = docker
        .iter()
        .map(|t| t.item().key())
        .zip(docker_results)
        .collect();
    let after = disk::disk_info(root).ok();
    let freed = before
        .zip(after)
        .map(|(b, a)| a.free as i64 - b.free as i64);
    app.on_executed(ids.into_iter().zip(results).collect(), docker_done, freed);
}
