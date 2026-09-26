use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NodeMeta {
    pub size: u64,
    pub inode: u64,
    pub dev: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Box<str>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub kind: NodeKind,
    /// finalize 之後為整個子樹的佔用空間
    pub size: u64,
    pub file_count: u64,
    pub inode: u64,
    pub dev: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Tree {
    root_path: PathBuf,
    nodes: Vec<Node>,
}

impl Tree {
    pub fn new(root_path: PathBuf, meta: NodeMeta) -> Self {
        let root = Node {
            name: "".into(),
            parent: None,
            children: Vec::new(),
            kind: NodeKind::Dir,
            size: meta.size,
            file_count: 0,
            inode: meta.inode,
            dev: meta.dev,
            mtime: meta.mtime,
        };
        Self {
            root_path,
            nodes: vec![root],
        }
    }

    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn add(&mut self, parent: NodeId, name: &str, kind: NodeKind, meta: NodeMeta) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).expect("too many nodes"));
        self.nodes.push(Node {
            name: name.into(),
            parent: Some(parent),
            children: Vec::new(),
            kind,
            size: meta.size,
            file_count: u64::from(kind != NodeKind::Dir),
            inode: meta.inode,
            dev: meta.dev,
            mtime: meta.mtime,
        });
        self.nodes[parent.index()].children.push(id);
        id
    }

    /// 不建立節點的小檔案：直接計入父目錄。
    pub fn add_aggregate(&mut self, parent: NodeId, size: u64) {
        let node = &mut self.nodes[parent.index()];
        node.size += size;
        node.file_count += 1;
    }

    /// 由葉往根累加 size 與 file_count。子節點的 id 一定大於父節點，倒序走一次即可。
    /// 只能在建樹完成後呼叫一次。
    pub fn finalize(&mut self) {
        for i in (1..self.nodes.len()).rev() {
            let (size, count, parent) = {
                let n = &self.nodes[i];
                (n.size, n.file_count, n.parent)
            };
            if let Some(p) = parent {
                let pn = &mut self.nodes[p.index()];
                pn.size += size;
                pn.file_count += count;
            }
        }
    }

    pub fn path_of(&self, id: NodeId) -> PathBuf {
        let mut names = Vec::new();
        let mut cur = id;
        while let Some(parent) = self.nodes[cur.index()].parent {
            names.push(&*self.nodes[cur.index()].name);
            cur = parent;
        }
        let mut path = self.root_path.clone();
        for name in names.iter().rev() {
            path.push(name);
        }
        path
    }

    pub fn children_sorted_by_size(&self, id: NodeId) -> Vec<NodeId> {
        let mut children = self.nodes[id.index()].children.clone();
        children.sort_by(|a, b| {
            let (na, nb) = (self.node(*a), self.node(*b));
            nb.size.cmp(&na.size).then_with(|| na.name.cmp(&nb.name))
        });
        children
    }

    pub fn is_ancestor(&self, ancestor: NodeId, id: NodeId) -> bool {
        let mut cur = self.nodes[id.index()].parent;
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = self.nodes[p.index()].parent;
        }
        false
    }

    pub fn child_named(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.nodes[id.index()]
            .children
            .iter()
            .copied()
            .find(|c| &*self.node(*c).name == name)
    }

    /// 刪除成功後呼叫：從父節點移除並從所有祖先扣除大小。重複呼叫不會重複扣除。
    pub fn subtract(&mut self, id: NodeId) {
        let Some(parent) = self.node(id).parent else {
            return;
        };
        if !self.nodes[parent.index()].children.contains(&id) {
            return;
        }
        let (size, count) = (self.node(id).size, self.node(id).file_count);
        self.nodes[parent.index()].children.retain(|c| *c != id);
        let mut cur = Some(parent);
        while let Some(p) = cur {
            let n = &mut self.nodes[p.index()];
            n.size = n.size.saturating_sub(size);
            n.file_count = n.file_count.saturating_sub(count);
            cur = n.parent;
        }
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    /// 依 (相對路徑, 大小) 建立已 finalize 的樹。中間目錄自動建立（大小 0）；
    /// 以 "/" 結尾的項目視為空目錄。每個節點的 inode 都不同，mtime 為 0。
    pub fn tree_from(root: &str, files: &[(&str, u64)]) -> Tree {
        let mut t = Tree::new(PathBuf::from(root), NodeMeta::default());
        for (rel, size) in files {
            let is_dir = rel.ends_with('/');
            let parts: Vec<&str> = rel.trim_end_matches('/').split('/').collect();
            let mut cur = t.root();
            for (i, part) in parts.iter().enumerate() {
                let last = i == parts.len() - 1;
                cur = match t.child_named(cur, part) {
                    Some(c) => c,
                    None => {
                        let kind = if last && !is_dir {
                            NodeKind::File
                        } else {
                            NodeKind::Dir
                        };
                        let inode = t.len() as u64;
                        let size = if last { *size } else { 0 };
                        t.add(
                            cur,
                            part,
                            kind,
                            NodeMeta {
                                size,
                                inode,
                                dev: 1,
                                mtime: 0,
                            },
                        )
                    }
                };
            }
        }
        t.finalize();
        t
    }

    pub fn find(t: &Tree, rel: &str) -> NodeId {
        rel.split('/').fold(t.root(), |cur, part| {
            t.child_named(cur, part)
                .unwrap_or_else(|| panic!("no node {rel}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{find, tree_from};
    use super::*;

    #[test]
    fn finalize_accumulates_sizes_and_counts() {
        let t = tree_from("/h", &[("a/x", 100), ("a/b/y", 50), ("z", 1)]);
        assert_eq!(t.node(t.root()).size, 151);
        assert_eq!(t.node(t.root()).file_count, 3);
        let a = find(&t, "a");
        assert_eq!(t.node(a).size, 150);
        assert_eq!(t.node(a).file_count, 2);
    }

    #[test]
    fn path_of_handles_spaces_and_cjk() {
        let rel = "Library/Application Support/中文 檔案 🎉.txt";
        let t = tree_from("/h", &[(rel, 1)]);
        assert_eq!(t.path_of(find(&t, rel)), PathBuf::from("/h").join(rel));
        assert_eq!(t.path_of(t.root()), PathBuf::from("/h"));
    }

    #[test]
    fn children_sorted_by_size_desc_then_name() {
        let t = tree_from("/h", &[("small", 1), ("big", 10), ("also-big", 10)]);
        let names: Vec<&str> = t
            .children_sorted_by_size(t.root())
            .iter()
            .map(|&c| &*t.node(c).name)
            .collect();
        assert_eq!(names, ["also-big", "big", "small"]);
    }

    #[test]
    fn subtract_updates_ancestors_and_is_idempotent() {
        let mut t = tree_from("/h", &[("a/b/x", 100), ("a/y", 10)]);
        let a = find(&t, "a");
        let b = find(&t, "a/b");
        t.subtract(b);
        t.subtract(b);
        assert_eq!(t.node(t.root()).size, 10);
        assert_eq!(t.node(t.root()).file_count, 1);
        assert_eq!(t.node(a).size, 10);
        assert!(!t.node(a).children.contains(&b));
    }

    #[test]
    fn is_ancestor_follows_parent_chain() {
        let t = tree_from("/h", &[("a/b/x", 1), ("c", 1)]);
        let (a, x, c) = (find(&t, "a"), find(&t, "a/b/x"), find(&t, "c"));
        assert!(t.is_ancestor(a, x));
        assert!(!t.is_ancestor(x, a));
        assert!(!t.is_ancestor(c, x));
        assert!(!t.is_ancestor(a, a));
    }

    #[test]
    fn add_aggregate_counts_into_parent() {
        let mut t = Tree::new("/h".into(), NodeMeta::default());
        let root = t.root();
        t.add_aggregate(root, 7);
        t.finalize();
        assert_eq!(t.node(root).size, 7);
        assert_eq!(t.node(root).file_count, 1);
        assert!(t.node(root).children.is_empty());
    }
}
