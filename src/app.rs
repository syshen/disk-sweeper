use std::collections::{BTreeMap, HashMap};

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
pub struct ConfirmState {
    pub items: Vec<(NodeId, Verdict)>,
    pub warnings: Vec<String>,
    pub need_typed: bool,
    pub input: String,
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
            now,
        }
    }

    pub fn on_scan_event(&mut self, ev: ScanEvent) {
        match ev {
            ScanEvent::Progress(p) => self.progress = p,
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
                self.results.clear();
                self.freed = None;
                self.screen = Screen::Main;
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
        let items: Vec<(NodeId, Verdict)> = self
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
                (id, safety::review(c, &self.ctx))
            })
            .collect();
        let approved: Vec<&ApprovedTarget> = items
            .iter()
            .filter_map(|(_, v)| match v {
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
        });
        self.screen = Screen::Confirm;
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let state = self.confirm.as_mut()?;
        if key.code == KeyCode::Esc {
            self.cancel_confirm();
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
            KeyCode::Char('y') | KeyCode::Enter => self.confirm_execute(),
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
            .filter_map(|(id, v)| match v {
                Verdict::Approved(t) => Some((id, t)),
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
    use std::time::Duration;

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
        app.on_key(key('x'));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.results.is_empty());
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
