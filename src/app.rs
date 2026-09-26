use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{procs, ActionResult, Outcome};
use crate::context::Context;
use crate::disk::DiskInfo;
use crate::risk::{Handling, Risk, ToolCommand};
use crate::rules::{self, Category, Suggestion};
use crate::safety::{self, ApprovedTarget, Candidate, Verdict};
use crate::scan::{self, Progress, ScanError, ScanEvent};
use crate::tree::{NodeId, Tree};

const TYPED_CONFIRM_THRESHOLD: u64 = 10 << 30;
const SMALL_ITEM: u64 = 10 << 20;
/// 掃描進度的目前路徑超過這麼久沒變，就提示可能在等 macOS 權限對話框
const SCAN_STALL: Duration = Duration::from_secs(3);
/// 確認清單 PgUp / PgDn 一次捲動的項目數
const CONFIRM_PAGE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Explore,
    Suggest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Main,
    Confirm,
    Running,
    Result,
    Help,
    Errors,
}

#[derive(Debug, Clone, Copy)]
pub struct Mark {
    pub risk: Risk,
    pub rule_id: Option<&'static str>,
    pub command: Option<ToolCommand>,
}

#[derive(Debug)]
pub struct ConfirmItem {
    pub node: NodeId,
    /// 標記時的風險等級（顯示用；處理方式以 verdict 為準）
    pub risk: Risk,
    pub verdict: Verdict,
}

#[derive(Debug)]
pub struct ConfirmState {
    pub items: Vec<ConfirmItem>,
    pub warnings: Vec<String>,
    pub need_typed: bool,
    pub input: String,
    /// 清單最上面顯示的是第幾項（從 0 起算）
    pub scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestRow {
    Header(Category),
    Item(usize),
}

pub enum Effect {
    Execute(Vec<(NodeId, ApprovedTarget)>),
}

pub struct App {
    pub ctx: Context,
    pub dry_run: bool,
    pub disk: Option<DiskInfo>,
    pub progress: Progress,
    pub tree: Option<Tree>,
    pub errors: Vec<ScanError>,
    pub needs_fda: bool,
    pub suggestions: Vec<Suggestion>,
    sugg_by_node: HashMap<NodeId, usize>,
    pub mode: Mode,
    pub screen: Screen,
    /// 目錄堆疊：(目錄, 游標)，最後一個是目前所在目錄
    pub explore: Vec<(NodeId, usize)>,
    pub suggest_cursor: usize,
    pub expanded: [bool; 3],
    pub show_small: bool,
    pub marks: BTreeMap<NodeId, Mark>,
    pub confirm: Option<ConfirmState>,
    pub results: Vec<ActionResult>,
    pub freed: Option<i64>,
    pub status: Option<String>,
    pub quit: bool,
    /// 最近一次掃描進度的目前路徑改變的時間
    pub last_progress_change: Instant,
    now: i64,
}

impl App {
    pub fn new(ctx: Context, dry_run: bool, disk: Option<DiskInfo>, now: i64) -> Self {
        Self {
            ctx,
            dry_run,
            disk,
            progress: Progress::default(),
            tree: None,
            errors: Vec::new(),
            needs_fda: false,
            suggestions: Vec::new(),
            sugg_by_node: HashMap::new(),
            mode: Mode::Explore,
            screen: Screen::Main,
            explore: Vec::new(),
            suggest_cursor: 0,
            expanded: [true; 3],
            show_small: false,
            marks: BTreeMap::new(),
            confirm: None,
            results: Vec::new(),
            freed: None,
            status: None,
            quit: false,
            last_progress_change: Instant::now(),
            now,
        }
    }

    pub fn on_scan_event(&mut self, ev: ScanEvent) {
        match ev {
            ScanEvent::Progress(p) => {
                if p.current != self.progress.current {
                    self.last_progress_change = Instant::now();
                }
                self.progress = p;
            }
            ScanEvent::Done(r) => {
                self.needs_fda = scan::needs_full_disk_access(&r.errors, &self.ctx.home);
                self.errors = r.errors;
                self.explore = vec![(r.tree.root(), 0)];
                self.status = Some(format!("掃描完成，耗時 {:.1} 秒", r.elapsed.as_secs_f64()));
                self.tree = Some(r.tree);
                self.refresh_suggestions();
            }
        }
    }

    fn refresh_suggestions(&mut self) {
        if let Some(tree) = &self.tree {
            self.suggestions = rules::evaluate(tree, &self.ctx, self.now);
            self.sugg_by_node = self
                .suggestions
                .iter()
                .enumerate()
                .map(|(i, s)| (s.node, i))
                .collect();
        }
    }

    /// 掃描中且目前路徑超過 3 秒沒變（可能卡在 macOS 權限對話框）
    pub fn scan_stalled(&self) -> bool {
        self.stalled_at(Instant::now())
    }

    pub(crate) fn stalled_at(&self, now: Instant) -> bool {
        self.tree.is_none() && now.saturating_duration_since(self.last_progress_change) > SCAN_STALL
    }

    pub fn explore_children(&self) -> Vec<NodeId> {
        match (&self.tree, self.explore.last()) {
            (Some(t), Some(&(dir, _))) => t.children_sorted_by_size(dir),
            _ => Vec::new(),
        }
    }

    pub fn suggestion_for(&self, id: NodeId) -> Option<&Suggestion> {
        self.sugg_by_node.get(&id).map(|&i| &self.suggestions[i])
    }

    pub fn visible_items(&self, cat: Category) -> impl Iterator<Item = usize> + '_ {
        self.suggestions
            .iter()
            .enumerate()
            .filter(move |(_, s)| s.category == cat && (self.show_small || s.size >= SMALL_ITEM))
            .map(|(i, _)| i)
    }

    pub fn suggest_rows(&self) -> Vec<SuggestRow> {
        let mut rows = Vec::new();
        for cat in Category::ALL {
            rows.push(SuggestRow::Header(cat));
            if self.expanded[cat as usize] {
                rows.extend(self.visible_items(cat).map(SuggestRow::Item));
            }
        }
        rows
    }

    pub fn marked_total(&self) -> u64 {
        self.tree
            .as_ref()
            .map_or(0, |t| self.marks.keys().map(|&id| t.node(id).size).sum())
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        self.status = None;
        match self.screen {
            Screen::Main => self.on_main_key(key),
            Screen::Confirm => self.on_confirm_key(key),
            Screen::Result => {
                // 只有明確的按鍵才關閉結果畫面，避免執行期間誤按的鍵把結果直接跳過
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                    self.results.clear();
                    self.freed = None;
                    self.screen = Screen::Main;
                }
                None
            }
            Screen::Help | Screen::Errors => {
                self.screen = Screen::Main;
                None
            }
            Screen::Running => None,
        }
    }

    fn on_main_key(&mut self, key: KeyEvent) -> Option<Effect> {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.screen = Screen::Help,
            KeyCode::Char('u') => self.screen = Screen::Errors,
            _ if self.tree.is_none() => {}
            KeyCode::Tab => {
                self.mode = match self.mode {
                    Mode::Explore => Mode::Suggest,
                    Mode::Suggest => Mode::Explore,
                }
            }
            KeyCode::Char('d') => self.begin_confirm(),
            code => match self.mode {
                Mode::Explore => self.on_explore_key(code),
                Mode::Suggest => self.on_suggest_key(code),
            },
        }
        None
    }

    fn on_explore_key(&mut self, code: KeyCode) {
        let children = self.explore_children();
        let Some(&(_, cursor)) = self.explore.last() else {
            return;
        };
        let selected = children.get(cursor).copied();
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.set_explore_cursor((cursor + 1).min(children.len().saturating_sub(1)))
            }
            KeyCode::Up | KeyCode::Char('k') => self.set_explore_cursor(cursor.saturating_sub(1)),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if let (Some(id), Some(tree)) = (selected, &self.tree) {
                    if !tree.node(id).children.is_empty() {
                        self.explore.push((id, 0));
                    }
                }
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if self.explore.len() > 1 {
                    self.explore.pop();
                }
            }
            KeyCode::Char(' ') => {
                if let Some(id) = selected {
                    self.toggle_mark(id);
                }
            }
            _ => {}
        }
    }

    fn set_explore_cursor(&mut self, cursor: usize) {
        if let Some(level) = self.explore.last_mut() {
            level.1 = cursor;
        }
    }

    fn on_suggest_key(&mut self, code: KeyCode) {
        let rows = self.suggest_rows();
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.suggest_cursor = (self.suggest_cursor + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.suggest_cursor = self.suggest_cursor.saturating_sub(1)
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(SuggestRow::Header(cat)) = rows.get(self.suggest_cursor) {
                    let i = *cat as usize;
                    self.expanded[i] = !self.expanded[i];
                    self.clamp_suggest_cursor();
                }
            }
            KeyCode::Char(' ') => match rows.get(self.suggest_cursor) {
                Some(SuggestRow::Header(cat)) => self.toggle_group(*cat),
                Some(SuggestRow::Item(i)) => {
                    let node = self.suggestions[*i].node;
                    self.toggle_mark(node);
                }
                None => {}
            },
            KeyCode::Char('a') => {
                self.show_small = !self.show_small;
                self.clamp_suggest_cursor();
            }
            _ => {}
        }
    }

    fn clamp_suggest_cursor(&mut self) {
        let n = self.suggest_rows().len();
        self.suggest_cursor = self.suggest_cursor.min(n.saturating_sub(1));
    }

    fn toggle_group(&mut self, cat: Category) {
        let nodes: Vec<NodeId> = self
            .visible_items(cat)
            .map(|i| self.suggestions[i].node)
            .collect();
        if nodes.is_empty() {
            return;
        }
        if nodes.iter().all(|n| self.marks.contains_key(n)) {
            for n in nodes {
                self.marks.remove(&n);
            }
        } else {
            for n in nodes {
                if !self.marks.contains_key(&n) {
                    self.mark(n);
                }
            }
        }
    }

    fn toggle_mark(&mut self, id: NodeId) {
        if self.marks.remove(&id).is_none() {
            self.mark(id);
        }
    }

    /// 標記一個節點：受保護的拒絕；上層已標記的拒絕；已標記的子孫會被取代，避免重複計算與重複刪除。
    fn mark(&mut self, id: NodeId) {
        let Some(tree) = &self.tree else { return };
        if id == tree.root() {
            return;
        }
        if let Some(reason) = safety::protection_reason(&tree.path_of(id), &self.ctx) {
            self.status = Some(format!("無法標記：{reason}"));
            return;
        }
        if self.marks.keys().any(|&m| tree.is_ancestor(m, id)) {
            self.status = Some("上層目錄已標記".into());
            return;
        }
        let mark = match self.sugg_by_node.get(&id) {
            Some(&i) => {
                let s = &self.suggestions[i];
                Mark {
                    risk: s.risk,
                    rule_id: Some(s.rule_id),
                    command: s.command,
                }
            }
            None => Mark {
                risk: Risk::UserData,
                rule_id: None,
                command: None,
            },
        };
        self.marks.retain(|&m, _| !tree.is_ancestor(id, m));
        self.marks.insert(id, mark);
    }

    fn begin_confirm(&mut self) {
        let Some(tree) = &self.tree else { return };
        if self.marks.is_empty() {
            self.status = Some("尚未標記任何項目".into());
            return;
        }
        let items: Vec<ConfirmItem> = self
            .marks
            .iter()
            .map(|(&id, m)| {
                let n = tree.node(id);
                let c = Candidate {
                    path: tree.path_of(id),
                    risk: m.risk,
                    rule_id: m.rule_id,
                    command: m.command,
                    size: n.size,
                    inode: n.inode,
                    mtime: n.mtime,
                };
                ConfirmItem {
                    node: id,
                    risk: m.risk,
                    verdict: safety::review(c, &self.ctx),
                }
            })
            .collect();
        let approved: Vec<&ApprovedTarget> = items
            .iter()
            .filter_map(|i| match &i.verdict {
                Verdict::Approved(t) => Some(t),
                Verdict::Rejected { .. } => None,
            })
            .collect();
        let total: u64 = approved.iter().map(|t| t.size()).sum();
        let need_typed = total > TYPED_CONFIRM_THRESHOLD
            || approved.iter().any(|t| t.handling() == Handling::Trash);
        let warnings = procs::running_app_warnings(approved.iter().filter_map(|t| t.rule_id()));
        self.confirm = Some(ConfirmState {
            items,
            warnings,
            need_typed,
            input: String::new(),
            scroll: 0,
        });
        self.screen = Screen::Confirm;
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let state = self.confirm.as_mut()?;
        if key.code == KeyCode::Esc {
            self.cancel_confirm();
            return None;
        }
        // 捲動只用方向鍵與 PgUp / PgDn：需要輸入 yes 時 j / k 是輸入的字元
        let last = state.items.len().saturating_sub(1);
        let scroll = match key.code {
            KeyCode::Down => Some((state.scroll + 1).min(last)),
            KeyCode::Up => Some(state.scroll.saturating_sub(1)),
            KeyCode::PageDown => Some((state.scroll + CONFIRM_PAGE).min(last)),
            KeyCode::PageUp => Some(state.scroll.saturating_sub(CONFIRM_PAGE)),
            _ => None,
        };
        if let Some(scroll) = scroll {
            state.scroll = scroll;
            return None;
        }
        if state.need_typed {
            match key.code {
                KeyCode::Char(c) => state.input.push(c),
                KeyCode::Backspace => {
                    state.input.pop();
                }
                KeyCode::Enter if state.input == "yes" => return self.confirm_execute(),
                KeyCode::Enter => {
                    state.input.clear();
                    self.status = Some("請輸入 yes 以確認".into());
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Char('y') => self.confirm_execute(),
            KeyCode::Char('n') => {
                self.cancel_confirm();
                None
            }
            _ => None,
        }
    }

    fn cancel_confirm(&mut self) {
        self.confirm = None;
        self.screen = Screen::Main;
    }

    fn confirm_execute(&mut self) -> Option<Effect> {
        let state = self.confirm.take()?;
        let targets: Vec<(NodeId, ApprovedTarget)> = state
            .items
            .into_iter()
            .filter_map(|i| match i.verdict {
                Verdict::Approved(t) => Some((i.node, t)),
                Verdict::Rejected { .. } => None,
            })
            .collect();
        if targets.is_empty() {
            self.status = Some("沒有可執行的項目".into());
            self.screen = Screen::Main;
            return None;
        }
        self.screen = Screen::Running;
        Some(Effect::Execute(targets))
    }

    pub fn on_executed(&mut self, done: Vec<(NodeId, ActionResult)>, freed: Option<i64>) {
        if let Some(tree) = &mut self.tree {
            for (id, r) in &done {
                if r.outcome == Outcome::Done && !matches!(r.handling, Handling::Command(_)) {
                    tree.subtract(*id);
                }
            }
        }
        if let Some(tree) = &self.tree {
            self.explore = vec![(tree.root(), 0)];
        }
        self.marks.clear();
        self.suggest_cursor = 0;
        self.refresh_suggestions();
        self.results = done.into_iter().map(|(_, r)| r).collect();
        self.freed = freed;
        self.screen = Screen::Result;
    }

    pub fn on_execute_error(&mut self, msg: String) {
        self.status = Some(msg);
        self.screen = Screen::Main;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Outcome;
    use crate::scan::{ScanOptions, ScanResult};
    use crate::tree::testutil::{find, tree_from};
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            assert!(app.on_key(key(c)).is_none());
        }
    }

    /// 以 /h 為 home 的假樹（不碰檔案系統）
    fn app_with(files: &[(&str, u64)]) -> App {
        let ctx = Context {
            home: PathBuf::from("/h"),
            home_dev: 0,
            path_var: OsString::new(),
        };
        let mut app = App::new(ctx, false, None, 1_800_000_000);
        let tree = tree_from("/h", files);
        app.on_scan_event(ScanEvent::Done(ScanResult {
            tree,
            errors: vec![],
            elapsed: Duration::ZERO,
        }));
        app
    }

    /// 真正掃描 temp home（確認流程需要 safety::review 碰檔案系統）
    fn scanned_app(home: &Path) -> App {
        let ctx = Context::new(home, OsString::from("/usr/bin:/bin")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let result = scan::scan(
            &ctx.home,
            &ScanOptions {
                threads: 2,
                min_node_size: 0,
            },
            &tx,
        );
        let mut app = App::new(ctx, false, None, chrono::Local::now().timestamp());
        app.on_scan_event(ScanEvent::Done(result));
        app
    }

    fn write(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; len]).unwrap();
    }

    #[test]
    fn done_event_builds_tree_and_suggestions() {
        let app = app_with(&[
            ("Projects/web/package.json", 1),
            ("Projects/web/node_modules/a/i.js", 100),
        ]);
        assert!(app.tree.is_some());
        assert_eq!(app.suggestions.len(), 1);
        assert_eq!(app.suggestions[0].rule_id, "node-modules");
    }

    #[test]
    fn keys_are_ignored_while_scanning_except_quit() {
        let ctx = Context {
            home: PathBuf::from("/h"),
            home_dev: 0,
            path_var: OsString::new(),
        };
        let mut app = App::new(ctx, false, None, 0);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.mode, Mode::Explore);
        app.on_key(key('q'));
        assert!(app.quit);
    }

    #[test]
    fn explore_navigation_enters_backs_and_clamps() {
        let mut app = app_with(&[("a/b/x", 100), ("c", 1)]);
        let tree = app.tree.as_ref().unwrap();
        let (a, c) = (find(tree, "a"), find(tree, "c"));
        assert_eq!(app.explore_children(), vec![a, c]);
        app.on_key(key('j'));
        app.on_key(key('j'));
        assert_eq!(app.explore.last().unwrap().1, 1);
        app.on_key(key('k'));
        app.on_key(key('l'));
        assert_eq!(app.explore.last().unwrap().0, a);
        app.on_key(key('h'));
        assert_eq!(app.explore.len(), 1);
        app.on_key(key('h'));
        assert_eq!(app.explore.len(), 1);
    }

    #[test]
    fn marking_parent_and_child_does_not_double_count() {
        // 先標記父層，再標記子項目 → 子項目被拒絕
        let mut app = app_with(&[("a/b/x", 100), ("a/y", 10)]);
        app.on_key(key(' ')); // 標記 a
        app.on_key(key('l')); // 進入 a
        app.on_key(key(' ')); // 嘗試標記 a/b
        assert_eq!(app.marks.len(), 1);
        assert_eq!(app.marked_total(), 110);
        assert!(app.status.is_some());

        // 先標記子項目，再標記父層 → 子項目被取代
        let mut app = app_with(&[("a/b/x", 100), ("a/y", 10)]);
        app.on_key(key('l'));
        app.on_key(key(' ')); // 標記 a/b
        app.on_key(key('h'));
        app.on_key(key(' ')); // 標記 a
        let a = find(app.tree.as_ref().unwrap(), "a");
        assert_eq!(app.marks.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(app.marked_total(), 110);
    }

    #[test]
    fn protected_dirs_cannot_be_marked() {
        let mut app = app_with(&[("Library/Caches/x", 1)]);
        app.on_key(key(' '));
        assert!(app.marks.is_empty());
        assert!(app.status.as_deref().unwrap().contains("無法標記"));
    }

    #[test]
    fn suggest_header_toggles_whole_group() {
        let mut app = app_with(&[
            ("Projects/a/package.json", 1),
            ("Projects/a/node_modules/x", 20 << 20),
            ("Projects/b/package.json", 1),
            ("Projects/b/node_modules/x", 20 << 20),
        ]);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(
            app.suggest_rows()[0],
            SuggestRow::Header(Category::DevTools)
        );
        app.on_key(key(' '));
        assert_eq!(app.marks.len(), 2);
        app.on_key(key(' '));
        assert!(app.marks.is_empty());
    }

    #[test]
    fn small_items_are_hidden_until_toggled() {
        let mut app = app_with(&[
            ("Projects/a/package.json", 1),
            ("Projects/a/node_modules/x", 1),
        ]);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.visible_items(Category::DevTools).count(), 0);
        app.on_key(key('a'));
        assert_eq!(app.visible_items(Category::DevTools).count(), 1);
    }

    #[test]
    fn d_without_marks_shows_status() {
        let mut app = app_with(&[("a", 1)]);
        app.on_key(key('d'));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.status.is_some());
    }

    #[test]
    fn trash_items_require_typed_yes() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/notes.txt"), 10);
        let mut app = scanned_app(home.path());
        app.on_key(key('l')); // 進入 Projects
        app.on_key(key(' ')); // 標記 notes.txt（使用者資料 → 垃圾桶）
        app.on_key(key('d'));
        assert_eq!(app.screen, Screen::Confirm);
        assert!(app.confirm.as_ref().unwrap().need_typed);
        assert!(app.on_key(key('y')).is_none()); // 單按 y 不夠
        assert!(app.on_key(code(KeyCode::Enter)).is_none());
        assert_eq!(app.screen, Screen::Confirm);
        typed(&mut app, "yes");
        let Some(Effect::Execute(targets)) = app.on_key(code(KeyCode::Enter)) else {
            panic!("no effect")
        };
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].1.handling(), Handling::Trash);
        assert_eq!(app.screen, Screen::Running);
    }

    #[test]
    fn regenerable_only_accepts_y_and_results_update_tree() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/web/package.json"), 10);
        write(&home.path().join("Projects/web/node_modules/a/i.js"), 5000);
        let mut app = scanned_app(home.path());
        let root_before = app
            .tree
            .as_ref()
            .unwrap()
            .node(app.tree.as_ref().unwrap().root())
            .size;
        app.on_key(code(KeyCode::Tab));
        app.on_key(key('a')); // 顯示小項目
        app.on_key(key('j')); // 移到 node_modules
        app.on_key(key(' '));
        app.on_key(key('d'));
        assert!(!app.confirm.as_ref().unwrap().need_typed);
        assert!(app.on_key(code(KeyCode::Enter)).is_none()); // 只有 y 會執行
        assert_eq!(app.screen, Screen::Confirm);
        let Some(Effect::Execute(targets)) = app.on_key(key('y')) else {
            panic!("no effect")
        };
        let (id, target) = &targets[0];
        let removed = app.tree.as_ref().unwrap().node(*id).size;
        let result = ActionResult {
            path: target.path().to_path_buf(),
            size: target.size(),
            handling: target.handling(),
            outcome: Outcome::Done,
            output: None,
        };
        app.on_executed(vec![(*id, result)], Some(4096));
        let tree = app.tree.as_ref().unwrap();
        assert_eq!(tree.node(tree.root()).size, root_before - removed);
        assert!(app.suggestions.is_empty());
        assert!(app.marks.is_empty());
        assert_eq!(app.screen, Screen::Result);
        app.on_key(key('x')); // 其他鍵不會關掉結果畫面
        assert_eq!(app.screen, Screen::Result);
        app.on_key(code(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.results.is_empty());
    }

    #[test]
    fn result_screen_closes_only_on_enter_esc_or_q() {
        for (k, closes) in [
            (key('j'), false),
            (key(' '), false),
            (code(KeyCode::Down), false),
            (code(KeyCode::Enter), true),
            (code(KeyCode::Esc), true),
            (key('q'), true),
        ] {
            let mut app = app_with(&[("a", 1)]);
            app.screen = Screen::Result;
            app.on_key(k);
            assert_eq!(app.screen == Screen::Main, closes, "{k:?}");
            assert!(!app.quit, "q on the result screen must not quit");
        }
    }

    #[test]
    fn confirm_list_scrolls_with_arrows_and_typed_j_goes_to_input() {
        let home = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(&home.path().join(format!("Projects/f{i}.txt")), 10);
        }
        let mut app = scanned_app(home.path());
        app.on_key(key('l')); // 進入 Projects
        for _ in 0..5 {
            app.on_key(key(' '));
            app.on_key(key('j'));
        }
        app.on_key(key('d'));
        let state = app.confirm.as_ref().unwrap();
        assert!(state.need_typed);
        assert_eq!(state.items.len(), 5);
        assert!(state.items.iter().all(|i| i.risk == Risk::UserData));
        assert_eq!(state.scroll, 0);

        app.on_key(code(KeyCode::Down));
        assert_eq!(app.confirm.as_ref().unwrap().scroll, 1);
        app.on_key(code(KeyCode::PageDown));
        assert_eq!(
            app.confirm.as_ref().unwrap().scroll,
            4,
            "clamped to last item"
        );
        app.on_key(code(KeyCode::Up));
        assert_eq!(app.confirm.as_ref().unwrap().scroll, 3);
        app.on_key(code(KeyCode::PageUp));
        assert_eq!(app.confirm.as_ref().unwrap().scroll, 0);

        // 需要輸入 yes 時，j / k 是輸入的字元，不是捲動
        app.on_key(key('j'));
        app.on_key(key('k'));
        let state = app.confirm.as_ref().unwrap();
        assert_eq!(state.input, "jk");
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn scan_stall_is_detected_when_current_path_stops_changing() {
        let mut app = App::new(
            Context {
                home: PathBuf::from("/h"),
                home_dev: 0,
                path_var: OsString::new(),
            },
            false,
            None,
            0,
        );
        let t0 = Instant::now();
        assert!(!app.scan_stalled(), "a fresh app is not stalled");
        assert!(app.stalled_at(t0 + Duration::from_secs(4)));
        let p = Progress {
            current: "/h/a".into(),
            ..Progress::default()
        };
        app.on_scan_event(ScanEvent::Progress(p.clone()));
        let changed = app.last_progress_change;
        assert!(changed >= t0);
        assert!(!app.stalled_at(changed + Duration::from_secs(2)));
        assert!(app.stalled_at(changed + Duration::from_secs(4)));
        // 同一個路徑重複回報不算變動
        app.on_scan_event(ScanEvent::Progress(Progress { files: 5, ..p }));
        assert_eq!(app.last_progress_change, changed);
        // 掃描完成後不算卡住
        app.on_scan_event(ScanEvent::Done(ScanResult {
            tree: tree_from("/h", &[("a", 1)]),
            errors: vec![],
            elapsed: Duration::ZERO,
        }));
        assert!(!app.stalled_at(changed + Duration::from_secs(60)));
    }

    #[test]
    fn esc_cancels_confirm() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/notes.txt"), 10);
        let mut app = scanned_app(home.path());
        app.on_key(key('l'));
        app.on_key(key(' '));
        app.on_key(key('d'));
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.confirm.is_none());
        assert_eq!(app.marks.len(), 1);
    }
}
