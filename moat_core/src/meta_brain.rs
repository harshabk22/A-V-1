use crate::sandbox::FileAccess;
use crate::{MoatError, MoatResult};
use bit_vec::BitVec;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::RwLock as AsyncRwLock;
use tokio::time::{sleep, Duration as TokioDuration};

pub type NodeId = u64;

pub const SYSTEM_ROOT_ID: NodeId = 0;
const PATH_SCOPE_KEY: &str = "scope:path";
const DOMAIN_SCOPE_KEY: &str = "scope:domain";
const APP_SCOPE_KEY: &str = "scope:app";
const URL_SCHEME_SCOPE_KEY: &str = "scope:url_scheme";
const TOOL_SCOPE_KEY: &str = "scope:tool";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFlags(pub u32);

impl PermissionFlags {
    pub const NONE: Self = Self(0);
    pub const FILE_READ: Self = Self(1 << 0);
    pub const FILE_WRITE: Self = Self(1 << 1);
    pub const NET_OUTBOUND: Self = Self(1 << 2);
    pub const NET_INBOUND: Self = Self(1 << 3);
    pub const EXEC: Self = Self(1 << 4);
    pub const URL_OPEN: Self = Self(1 << 5);
    pub const APP_LAUNCH: Self = Self(1 << 6);
    pub const TOOL_EXEC: Self = Self(1 << 7);
    pub const FILE_DELETE: Self = Self(1 << 8);

    pub fn contains(self, flags: Self) -> bool {
        (self.0 & flags.0) == flags.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for PermissionFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for PermissionFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Root,
    Scope,
    Path,
    Domain,
    App,
    UrlScheme,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    pub id: NodeId,
    pub kind: NodeKind,
    pub key: String,
    pub label: String,
    pub resource: String,
    pub permissions: PermissionFlags,
    pub active: bool,
}

pub type NodeWeight = NodeData;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeData {
    pub weight: f32,
}

pub type EdgeWeight = EdgeData;

#[derive(Debug, Clone)]
pub struct BloomFilter {
    pub bits: BitVec,
    pub hash_count: usize,
}

#[derive(Debug, Clone)]
pub struct SubGraph {
    pub root_id: NodeId,
    pub nodes: Vec<NodeData>,
    pub allowed_paths: Vec<(String, FileAccess)>,
    pub allowed_domains: Vec<String>,
    pub allowed_apps: Vec<String>,
    pub allowed_url_schemes: Vec<String>,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PermissionSnapshot {
    pub paths: Vec<(String, FileAccess)>,
    pub domains: Vec<String>,
    pub apps: Vec<String>,
    pub url_schemes: Vec<String>,
    pub tools: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ExecutionPolicy {
    pub policy_id: String,
    pub source_worker: String,
    pub target_worker: String,
    pub allowed_tool: String,
    pub policy_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyHashInput {
    Relationship {
        source_worker: String,
        target_worker: String,
        allowed_tool: String,
    },
    Parameterized {
        source_worker: String,
        target_worker: String,
        allowed_tool: String,
        parameter_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaBrainSnapshot {
    pub nodes: Vec<NodeData>,
    pub edges: Vec<(NodeId, NodeId, f32)>,
}

pub struct MetaBrainInner {
    pub graph: DiGraph<NodeWeight, EdgeWeight>,
    pub bloom: BloomFilter,
    pub stable_id_map: HashMap<NodeId, NodeIndex>,
    pub key_map: HashMap<String, NodeId>,
    next_id: NodeId,
}

#[derive(Clone)]
struct FlushState {
    db_path: PathBuf,
    dirty: Arc<AtomicBool>,
}

pub struct MetaBrain {
    inner: MetaBrainInner,
    flush: Option<FlushState>,
}

#[inline(always)]
fn stable_fnv1a_hash(mut data: u64, seed: usize) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    data ^= seed as u64;
    for _ in 0..8 {
        hash ^= data & 0xFF;
        hash = hash.wrapping_mul(1099511628211);
        data >>= 8;
    }
    hash
}

fn tokenize_query(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|c: char| c.is_whitespace())
        .map(|token| {
            token
                .trim_matches(|c: char| {
                    !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'))
                })
                .to_ascii_lowercase()
        })
        .filter(|token| token.len() >= 2 && seen.insert(token.clone()))
        .collect()
}

fn path_chain(path: &str) -> Vec<(String, String)> {
    let expanded = if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            home.to_string_lossy().into_owned() + &path[1..]
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };
    let canonical = Path::new(&expanded);
    if !canonical.is_absolute() {
        return Vec::new();
    }

    let mut chain = vec![("/".to_string(), "/".to_string())];
    let mut current = PathBuf::from("/");
    for component in canonical.components() {
        if let std::path::Component::Normal(segment) = component {
            current.push(segment);
            let resource = current.to_string_lossy().into_owned();
            let label = segment.to_string_lossy().into_owned();
            chain.push((label, resource));
        }
    }
    chain
}

fn domain_chain(domain: &str) -> Vec<String> {
    let parts: Vec<&str> = domain
        .trim()
        .trim_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    let mut chain = Vec::new();
    let mut current = String::new();
    for part in parts.iter().rev() {
        current = if current.is_empty() {
            (*part).to_ascii_lowercase()
        } else {
            format!("{}.{}", part.to_ascii_lowercase(), current)
        };
        chain.push(current.clone());
    }
    chain
}

impl BloomFilter {
    #[inline]
    pub fn insert(&mut self, target_id: NodeId) {
        for i in 0..self.hash_count {
            let hash_val = stable_fnv1a_hash(target_id, i);
            let bit_idx = (hash_val % self.bits.len() as u64) as usize;
            self.bits.set(bit_idx, true);
        }
    }

    #[inline]
    pub fn probe(&self, target_id: NodeId) -> bool {
        for i in 0..self.hash_count {
            let hash_val = stable_fnv1a_hash(target_id, i);
            let bit_idx = (hash_val % self.bits.len() as u64) as usize;
            if !self.bits.get(bit_idx).unwrap_or(false) {
                return false;
            }
        }
        true
    }
}

impl MetaBrain {
    pub fn new_empty() -> Self {
        Self {
            inner: MetaBrainInner {
                graph: DiGraph::new(),
                bloom: BloomFilter {
                    bits: bit_vec::BitVec::from_elem(4096, false),
                    hash_count: 4,
                },
                stable_id_map: HashMap::new(),
                key_map: HashMap::new(),
                next_id: 1,
            },
            flush: None,
        }
    }

    pub fn new() -> Self {
        let mut brain = Self::new_empty();
        brain.ensure_base_hierarchy();
        brain
    }

    pub async fn load_or_new(db_path: PathBuf) -> MoatResult<Self> {
        match tokio::fs::read(&db_path).await {
            Ok(bytes) => {
                let snapshot: MetaBrainSnapshot = bincode::deserialize(&bytes)
                    .map_err(|err| MoatError::Graph(format!("meta-brain decode failed: {err}")))?;
                let mut brain = Self::new_empty();
                brain.restore_from_snapshot(snapshot);
                brain.attach_persistence(db_path);
                Ok(brain)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let mut brain = Self::new();
                brain.attach_persistence(db_path);
                Ok(brain)
            }
            Err(err) => Err(MoatError::Graph(format!(
                "meta-brain load failed from {}: {err}",
                db_path.display()
            ))),
        }
    }

    pub fn attach_persistence(&mut self, db_path: PathBuf) {
        self.flush = Some(FlushState {
            db_path,
            dirty: Arc::new(AtomicBool::new(true)),
        });
    }

    fn mark_dirty(&self) {
        if let Some(flush) = &self.flush {
            flush.dirty.store(true, Ordering::Release);
        }
    }

    pub fn spawn_flush_loop(brain: Arc<AsyncRwLock<Self>>) {
        tokio::spawn(async move {
            loop {
                sleep(TokioDuration::from_millis(250)).await;
                let should_flush = {
                    let brain = brain.read().await;
                    brain
                        .flush
                        .as_ref()
                        .map(|flush| flush.dirty.load(Ordering::Acquire))
                        .unwrap_or(false)
                };
                if !should_flush {
                    continue;
                }
                if let Err(e) = Self::flush_arc(Arc::clone(&brain)).await {
                    eprintln!("[MetaBrain] Flush failed: {e}");
                    // Retry after backoff — overwrite the dirty flag that was
                    // cleared on failure path inside flush_arc
                    if let Some(flush) = brain.read().await.flush.as_ref() {
                        flush.dirty.store(true, Ordering::Release);
                    }
                }
            }
        });
    }

    pub async fn flush_arc(brain: Arc<AsyncRwLock<Self>>) -> MoatResult<()> {
        let (snapshot, db_path, dirty) = {
            let brain = brain.read().await;
            let Some(flush) = brain.flush.as_ref() else {
                return Ok(());
            };
            (
                brain.snapshot(),
                flush.db_path.clone(),
                Arc::clone(&flush.dirty),
            )
        };

        let bytes = bincode::serialize(&snapshot)
            .map_err(|err| MoatError::Graph(format!("meta-brain encode failed: {err}")))?;
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| MoatError::Graph(format!("meta-brain dir create failed: {err}")))?;
        }
        let tmp = db_path.with_extension("bin.tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|err| MoatError::Graph(format!("meta-brain temp write failed: {err}")))?;
        tokio::fs::rename(&tmp, &db_path)
            .await
            .map_err(|err| MoatError::Graph(format!("meta-brain atomic rename failed: {err}")))?;
        dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub fn root_id(&self) -> NodeId {
        SYSTEM_ROOT_ID
    }

    pub fn ensure_base_hierarchy(&mut self) {
        self.upsert_node(NodeData {
            id: SYSTEM_ROOT_ID,
            kind: NodeKind::Root,
            key: "root:system".into(),
            label: "system".into(),
            resource: "system".into(),
            permissions: PermissionFlags::NONE,
            active: true,
        });

        let root = SYSTEM_ROOT_ID;
        for (key, label) in [
            (PATH_SCOPE_KEY, "path"),
            (DOMAIN_SCOPE_KEY, "domain"),
            (APP_SCOPE_KEY, "app"),
            (URL_SCHEME_SCOPE_KEY, "url_scheme"),
            (TOOL_SCOPE_KEY, "tool"),
        ] {
            let child = self.ensure_child_node(
                root,
                NodeKind::Scope,
                key,
                label,
                label,
                PermissionFlags::NONE,
            );
            self.add_edge_by_id(root, child, 1.0);
        }
    }

    pub fn snapshot(&self) -> MetaBrainSnapshot {
        let mut nodes = self
            .inner
            .graph
            .node_indices()
            .map(|idx| self.inner.graph[idx].clone())
            .collect::<Vec<_>>();
        nodes.sort_by_key(|node| node.id);

        let mut edges = self
            .inner
            .graph
            .edge_references()
            .map(|edge| {
                (
                    self.inner.graph[edge.source()].id,
                    self.inner.graph[edge.target()].id,
                    edge.weight().weight,
                )
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|(parent, child, _)| (*parent, *child));

        MetaBrainSnapshot { nodes, edges }
    }

    pub fn restore_from_snapshot(&mut self, snapshot: MetaBrainSnapshot) {
        let flush = self.flush.clone();
        *self = Self::new_empty();
        self.flush = flush;
        for node in snapshot.nodes {
            self.upsert_node(node);
        }
        for (parent, child, weight) in snapshot.edges {
            self.add_edge_by_id(parent, child, weight);
        }
        self.ensure_base_hierarchy();
        self.mark_dirty();
    }

    fn reserve_id(&mut self) -> NodeId {
        let id = self.inner.next_id;
        self.inner.next_id += 1;
        id
    }

    pub fn add_node(&mut self, node: NodeData) {
        self.upsert_node(node);
    }

    fn upsert_node(&mut self, node: NodeData) -> NodeId {
        if let Some(existing_id) = self.inner.key_map.get(&node.key).copied() {
            if let Some(idx) = self.inner.stable_id_map.get(&existing_id).copied() {
                let existing = &mut self.inner.graph[idx];
                existing.permissions |= node.permissions;
                existing.active = existing.active || node.active;
                if existing.label.is_empty() {
                    existing.label = node.label;
                }
                if existing.resource.is_empty() {
                    existing.resource = node.resource;
                }
                self.mark_dirty();
                return existing_id;
            }
        }

        let id = if node.id >= self.inner.next_id {
            self.inner.next_id = node.id + 1;
            node.id
        } else if self.inner.stable_id_map.contains_key(&node.id) {
            self.reserve_id()
        } else {
            if node.id == 0 {
                self.inner.next_id = self.inner.next_id.max(1);
            }
            node.id
        };

        self.inner.bloom.insert(id);
        let idx = self.inner.graph.add_node(NodeData { id, ..node });
        self.inner.stable_id_map.insert(id, idx);
        self.inner
            .key_map
            .insert(self.inner.graph[idx].key.clone(), id);
        self.mark_dirty();
        id
    }

    fn ensure_child_node(
        &mut self,
        parent_id: NodeId,
        kind: NodeKind,
        key: impl Into<String>,
        label: impl Into<String>,
        resource: impl Into<String>,
        permissions: PermissionFlags,
    ) -> NodeId {
        let key = key.into();
        let label = label.into();
        let resource = resource.into();
        let node_id = if let Some(existing_id) = self.inner.key_map.get(&key).copied() {
            if let Some(idx) = self.inner.stable_id_map.get(&existing_id).copied() {
                let existing = &mut self.inner.graph[idx];
                existing.permissions |= permissions;
                existing.active = true;
                self.mark_dirty();
            }
            existing_id
        } else {
            let id = self.reserve_id();
            self.upsert_node(NodeData {
                id,
                kind,
                key,
                label,
                resource,
                permissions,
                active: true,
            })
        };
        self.add_edge_by_id(parent_id, node_id, 1.0);
        node_id
    }

    pub fn add_edge_by_id(&mut self, parent_id: NodeId, child_id: NodeId, weight: f32) {
        let Some(parent_idx) = self.inner.stable_id_map.get(&parent_id).copied() else {
            return;
        };
        let Some(child_idx) = self.inner.stable_id_map.get(&child_id).copied() else {
            return;
        };
        let has_edge = self
            .inner
            .graph
            .edges_connecting(parent_idx, child_idx)
            .next()
            .is_some();
        if !has_edge {
            self.inner
                .graph
                .add_edge(parent_idx, child_idx, EdgeData { weight });
            self.mark_dirty();
        }
    }

    pub fn grant_path_permission(
        &mut self,
        path: &str,
        permissions: PermissionFlags,
    ) -> MoatResult<NodeId> {
        let scope_id = self
            .inner
            .key_map
            .get(PATH_SCOPE_KEY)
            .copied()
            .ok_or_else(|| MoatError::Graph("path scope missing".into()))?;

        let chain = path_chain(path);
        if chain.is_empty() {
            return Err(MoatError::Graph(format!(
                "invalid path for meta-brain hierarchy: {path}"
            )));
        }

        let mut parent = scope_id;
        let mut leaf = scope_id;
        for (label, resource) in chain {
            let key = format!("path:{resource}");
            leaf = self.ensure_child_node(
                parent,
                NodeKind::Path,
                key,
                label,
                resource.clone(),
                PermissionFlags::NONE,
            );
            parent = leaf;
        }

        self.merge_permissions(leaf, permissions);
        Ok(leaf)
    }

    pub fn grant_domain_permission(
        &mut self,
        domain: &str,
        permissions: PermissionFlags,
    ) -> MoatResult<NodeId> {
        let scope_id = self
            .inner
            .key_map
            .get(DOMAIN_SCOPE_KEY)
            .copied()
            .ok_or_else(|| MoatError::Graph("domain scope missing".into()))?;
        let chain = domain_chain(domain);
        if chain.is_empty() {
            return Err(MoatError::Graph(format!(
                "invalid domain for meta-brain hierarchy: {domain}"
            )));
        }

        let mut parent = scope_id;
        let mut leaf = scope_id;
        for resource in chain {
            let key = format!("domain:{resource}");
            leaf = self.ensure_child_node(
                parent,
                NodeKind::Domain,
                key,
                resource.clone(),
                resource.clone(),
                PermissionFlags::NONE,
            );
            parent = leaf;
        }

        self.merge_permissions(leaf, permissions);
        Ok(leaf)
    }

    pub fn grant_app_permission(
        &mut self,
        app_name: &str,
        permissions: PermissionFlags,
    ) -> MoatResult<NodeId> {
        let scope_id = self
            .inner
            .key_map
            .get(APP_SCOPE_KEY)
            .copied()
            .ok_or_else(|| MoatError::Graph("app scope missing".into()))?;
        let normalized = app_name.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(MoatError::Graph("app name cannot be empty".into()));
        }
        let node_id = self.ensure_child_node(
            scope_id,
            NodeKind::App,
            format!("app:{normalized}"),
            app_name.trim(),
            normalized,
            permissions,
        );
        self.merge_permissions(node_id, permissions);
        Ok(node_id)
    }

    pub fn grant_url_scheme_permission(
        &mut self,
        scheme: &str,
        permissions: PermissionFlags,
    ) -> MoatResult<NodeId> {
        let scope_id = self
            .inner
            .key_map
            .get(URL_SCHEME_SCOPE_KEY)
            .copied()
            .ok_or_else(|| MoatError::Graph("url scheme scope missing".into()))?;
        let normalized = scheme.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(MoatError::Graph("URL scheme cannot be empty".into()));
        }
        let node_id = self.ensure_child_node(
            scope_id,
            NodeKind::UrlScheme,
            format!("url_scheme:{normalized}"),
            normalized.clone(),
            normalized,
            permissions,
        );
        self.merge_permissions(node_id, permissions);
        Ok(node_id)
    }

    pub fn grant_tool_permission(
        &mut self,
        tool: &str,
        permissions: PermissionFlags,
    ) -> MoatResult<NodeId> {
        let scope_id = self
            .inner
            .key_map
            .get(TOOL_SCOPE_KEY)
            .copied()
            .ok_or_else(|| MoatError::Graph("tool scope missing".into()))?;
        let normalized = tool.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(MoatError::Graph("tool cannot be empty".into()));
        }
        let node_id = self.ensure_child_node(
            scope_id,
            NodeKind::Tool,
            format!("tool:{normalized}"),
            tool.trim(),
            normalized,
            permissions,
        );
        self.merge_permissions(node_id, permissions);
        Ok(node_id)
    }

    fn merge_permissions(&mut self, node_id: NodeId, permissions: PermissionFlags) {
        if let Some(idx) = self.inner.stable_id_map.get(&node_id).copied() {
            self.inner.graph[idx].permissions |= permissions;
            self.inner.graph[idx].active = true;
            self.mark_dirty();
        }
    }

    pub fn path_has_permission(&self, path: &str, permission: PermissionFlags) -> bool {
        if permission.is_empty() {
            return true;
        }
        path_chain(path)
            .into_iter()
            .rev()
            .any(|(_, resource)| self.node_has_permission(&format!("path:{resource}"), permission))
    }

    pub fn domain_has_permission(&self, domain: &str, permission: PermissionFlags) -> bool {
        if permission.is_empty() {
            return true;
        }
        domain_chain(domain)
            .into_iter()
            .rev()
            .any(|resource| self.node_has_permission(&format!("domain:{resource}"), permission))
            || self.node_has_permission("domain:*", permission)
    }

    pub fn app_has_permission(&self, app_name: &str, permission: PermissionFlags) -> bool {
        self.node_has_permission(
            &format!("app:{}", app_name.trim().to_ascii_lowercase()),
            permission,
        ) || self.node_has_permission("app:*", permission)
    }

    pub fn url_scheme_has_permission(&self, scheme: &str, permission: PermissionFlags) -> bool {
        self.node_has_permission(
            &format!("url_scheme:{}", scheme.trim().to_ascii_lowercase()),
            permission,
        ) || self.node_has_permission("url_scheme:*", permission)
    }

    pub fn tool_has_permission(&self, tool: &str, permission: PermissionFlags) -> bool {
        self.node_has_permission(
            &format!("tool:{}", tool.trim().to_ascii_lowercase()),
            permission,
        ) || self.node_has_permission("tool:*", permission)
    }

    fn node_has_permission(&self, key: &str, permission: PermissionFlags) -> bool {
        let Some(node_id) = self.inner.key_map.get(key).copied() else {
            return false;
        };
        let Some(idx) = self.inner.stable_id_map.get(&node_id).copied() else {
            return false;
        };
        let node = &self.inner.graph[idx];
        node.active && node.permissions.contains(permission)
    }

    pub fn permission_snapshot(&self) -> PermissionSnapshot {
        let mut paths = HashMap::new();
        let mut domains = HashSet::new();
        let mut apps = HashSet::new();
        let mut url_schemes = HashSet::new();
        let mut tools = HashSet::new();

        for idx in self.inner.graph.node_indices() {
            let node = &self.inner.graph[idx];
            if !node.active {
                continue;
            }
            match node.kind {
                NodeKind::Path => {
                    if node.permissions.contains(PermissionFlags::FILE_READ)
                        && node.permissions.contains(PermissionFlags::FILE_WRITE)
                    {
                        paths.insert(node.resource.clone(), FileAccess::ReadWrite);
                    } else if node.permissions.contains(PermissionFlags::FILE_WRITE) {
                        paths.insert(node.resource.clone(), FileAccess::WriteOnly);
                    } else if node.permissions.contains(PermissionFlags::FILE_READ) {
                        paths.insert(node.resource.clone(), FileAccess::ReadOnly);
                    }
                }
                NodeKind::Domain => {
                    if node.permissions.contains(PermissionFlags::NET_OUTBOUND) {
                        domains.insert(node.resource.clone());
                    }
                }
                NodeKind::App => {
                    if node.permissions.contains(PermissionFlags::APP_LAUNCH) {
                        apps.insert(node.label.clone());
                    }
                }
                NodeKind::UrlScheme => {
                    if node.permissions.contains(PermissionFlags::URL_OPEN) {
                        url_schemes.insert(node.resource.clone());
                    }
                }
                NodeKind::Tool => {
                    if node.permissions.contains(PermissionFlags::TOOL_EXEC) {
                        tools.insert(node.resource.clone());
                    }
                }
                NodeKind::Root | NodeKind::Scope => {}
            }
        }

        PermissionSnapshot {
            paths: paths.into_iter().collect(),
            domains: domains.into_iter().collect(),
            apps: apps.into_iter().collect(),
            url_schemes: url_schemes.into_iter().collect(),
            tools: tools.into_iter().collect(),
        }
    }

    pub fn extract_subgraph(&self, root: NodeId, depth: usize) -> MoatResult<SubGraph> {
        self.focused_subgraph(root, "", depth, 1)
    }

    pub fn focused_subgraph(
        &self,
        root: NodeId,
        query: &str,
        depth: usize,
        max_matches: usize,
    ) -> MoatResult<SubGraph> {
        let start_idx = self
            .inner
            .stable_id_map
            .get(&root)
            .copied()
            .ok_or_else(|| {
                MoatError::Graph(format!("NodeId '{}' not found in index_map.", root))
            })?;

        if !self.inner.bloom.probe(root) {
            return Err(MoatError::Graph(format!(
                "Access Denied: NodeId '{}' rejected by BloomFilter.",
                root
            )));
        }

        let mut visited = HashSet::new();
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        let relevant_roots = self.resolve_relevant_roots(root, query, max_matches);

        for idx in relevant_roots {
            queue.push_back((idx, 0));
            visited.insert(idx);
            self.include_ancestors(idx, start_idx, &mut visited);
        }

        if queue.is_empty() {
            queue.push_back((start_idx, 0));
            visited.insert(start_idx);
        }

        let mut expanded = visited.clone();
        while let Some((curr_idx, curr_depth)) = queue.pop_front() {
            if curr_depth >= depth {
                continue;
            }
            for neighbor_idx in self
                .inner
                .graph
                .neighbors_directed(curr_idx, Direction::Outgoing)
            {
                if expanded.insert(neighbor_idx) {
                    queue.push_back((neighbor_idx, curr_depth + 1));
                }
            }
        }

        let mut nodes: Vec<NodeData> = expanded
            .into_iter()
            .map(|idx| self.inner.graph[idx].clone())
            .filter(|node| node.active)
            .collect();
        nodes.sort_by_key(|node| node.id);

        let mut subgraph = SubGraph {
            root_id: root,
            nodes,
            allowed_paths: Vec::new(),
            allowed_domains: Vec::new(),
            allowed_apps: Vec::new(),
            allowed_url_schemes: Vec::new(),
            allowed_tools: Vec::new(),
        };

        for node in &subgraph.nodes {
            match node.kind {
                NodeKind::Path => {
                    if node.permissions.contains(PermissionFlags::FILE_READ)
                        && node.permissions.contains(PermissionFlags::FILE_WRITE)
                    {
                        subgraph
                            .allowed_paths
                            .push((node.resource.clone(), FileAccess::ReadWrite));
                    } else if node.permissions.contains(PermissionFlags::FILE_WRITE) {
                        subgraph
                            .allowed_paths
                            .push((node.resource.clone(), FileAccess::WriteOnly));
                    } else if node.permissions.contains(PermissionFlags::FILE_READ) {
                        subgraph
                            .allowed_paths
                            .push((node.resource.clone(), FileAccess::ReadOnly));
                    }
                }
                NodeKind::Domain => {
                    if node.permissions.contains(PermissionFlags::NET_OUTBOUND) {
                        subgraph.allowed_domains.push(node.resource.clone());
                    }
                }
                NodeKind::App => {
                    if node.permissions.contains(PermissionFlags::APP_LAUNCH) {
                        subgraph.allowed_apps.push(node.label.clone());
                    }
                }
                NodeKind::UrlScheme => {
                    if node.permissions.contains(PermissionFlags::URL_OPEN) {
                        subgraph.allowed_url_schemes.push(node.resource.clone());
                    }
                }
                NodeKind::Tool => {
                    if node.permissions.contains(PermissionFlags::TOOL_EXEC) {
                        subgraph.allowed_tools.push(node.resource.clone());
                    }
                }
                NodeKind::Root | NodeKind::Scope => {}
            }
        }

        Ok(subgraph)
    }

    pub fn extract_relevant_subgraph(&self, keyword: &str) -> String {
        let tokens = tokenize_query(keyword);
        if tokens.is_empty() {
            return format!(
                "No permissions found in MetaBrain for keyword \"{}\".",
                keyword.trim()
            );
        }

        let mut neighborhood = HashSet::new();
        for idx in self.inner.graph.node_indices() {
            let node = &self.inner.graph[idx];
            if !node.active || !self.inner.bloom.probe(node.id) {
                continue;
            }

            let label = node.label.to_ascii_lowercase();
            let resource = node.resource.to_ascii_lowercase();
            let matches = tokens
                .iter()
                .any(|token| label.contains(token) || resource.contains(token));
            if !matches {
                continue;
            }

            neighborhood.insert(idx);
            for neighbor in self
                .inner
                .graph
                .neighbors_directed(idx, Direction::Incoming)
            {
                if self.inner.graph[neighbor].active
                    && self.inner.bloom.probe(self.inner.graph[neighbor].id)
                {
                    neighborhood.insert(neighbor);
                }
            }
            for neighbor in self
                .inner
                .graph
                .neighbors_directed(idx, Direction::Outgoing)
            {
                if self.inner.graph[neighbor].active
                    && self.inner.bloom.probe(self.inner.graph[neighbor].id)
                {
                    neighborhood.insert(neighbor);
                }
            }
        }

        let rendered = self.render_seatbelt_neighborhood(&neighborhood);
        if rendered.is_empty() {
            format!(
                "No permissions found in MetaBrain for keyword \"{}\".",
                keyword.trim()
            )
        } else {
            rendered
        }
    }

    fn include_ancestors(
        &self,
        node_idx: NodeIndex,
        stop_idx: NodeIndex,
        visited: &mut HashSet<NodeIndex>,
    ) {
        let mut current = node_idx;
        loop {
            visited.insert(current);
            if current == stop_idx {
                break;
            }
            let Some(parent) = self
                .inner
                .graph
                .neighbors_directed(current, Direction::Incoming)
                .next()
            else {
                break;
            };
            current = parent;
        }
    }

    fn resolve_relevant_roots(
        &self,
        root: NodeId,
        query: &str,
        max_matches: usize,
    ) -> Vec<NodeIndex> {
        let Some(root_idx) = self.inner.stable_id_map.get(&root).copied() else {
            return Vec::new();
        };
        let tokens = tokenize_query(query);
        if tokens.is_empty() {
            return vec![root_idx];
        }

        let mut scored = self
            .inner
            .graph
            .node_indices()
            .filter_map(|idx| {
                let node = &self.inner.graph[idx];
                if !node.active || matches!(node.kind, NodeKind::Root | NodeKind::Scope) {
                    return None;
                }
                let label = node.label.to_ascii_lowercase();
                let resource = node.resource.to_ascii_lowercase();
                let mut score = 0usize;
                for token in &tokens {
                    if label == *token || resource == *token {
                        score += 100;
                    } else if resource.contains(token) {
                        score += 25 + token.len();
                    } else if label.contains(token) {
                        score += 15 + token.len();
                    }
                }
                if score == 0 {
                    None
                } else {
                    Some((score, idx))
                }
            })
            .collect::<Vec<_>>();

        if scored.is_empty() {
            return vec![root_idx];
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.inner.graph[a.1].id.cmp(&self.inner.graph[b.1].id))
        });
        let mut roots = vec![root_idx];
        for (_, idx) in scored.into_iter().take(max_matches.max(1)) {
            if !roots.contains(&idx) {
                roots.push(idx);
            }
        }
        roots
    }

    fn render_seatbelt_neighborhood(&self, neighborhood: &HashSet<NodeIndex>) -> String {
        let mut rules = HashSet::new();

        for idx in neighborhood {
            let node = &self.inner.graph[*idx];
            match node.kind {
                NodeKind::Path => {
                    if node.permissions.contains(PermissionFlags::FILE_READ) {
                        rules.insert(format!(
                            "(allow file-read* (subpath \"{}\"))",
                            node.resource
                        ));
                    }
                    if node.permissions.contains(PermissionFlags::FILE_WRITE) {
                        rules.insert(format!(
                            "(allow file-write* (subpath \"{}\"))",
                            node.resource
                        ));
                    }
                }
                NodeKind::Domain => {
                    if node.permissions.contains(PermissionFlags::NET_OUTBOUND) {
                        rules.insert(format!(
                            "(allow network-outbound (host \"{}\"))",
                            node.resource
                        ));
                    }
                    if node.permissions.contains(PermissionFlags::NET_INBOUND) {
                        rules.insert(format!(
                            "(allow network-inbound (host \"{}\"))",
                            node.resource
                        ));
                    }
                }
                NodeKind::App => {
                    if node.permissions.contains(PermissionFlags::APP_LAUNCH) {
                        rules.insert(format!("(allow app-launch (app \"{}\"))", node.label));
                    }
                }
                NodeKind::UrlScheme => {
                    if node.permissions.contains(PermissionFlags::URL_OPEN) {
                        rules.insert(format!("(allow open-url (scheme \"{}\"))", node.resource));
                    }
                }
                NodeKind::Tool => {
                    if node.permissions.contains(PermissionFlags::TOOL_EXEC)
                        || node.permissions.contains(PermissionFlags::EXEC)
                    {
                        rules.insert(format!("(allow process-exec (tool \"{}\"))", node.resource));
                    }
                }
                NodeKind::Root | NodeKind::Scope => {}
            }
        }

        if rules.is_empty() {
            return String::new();
        }

        let mut rendered = rules.into_iter().collect::<Vec<_>>();
        rendered.sort();
        rendered.join("\n")
    }
}

pub fn prepare_policy_table(conn: &Connection) -> MoatResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS metabrain_policies (
            policy_id TEXT PRIMARY KEY,
            source_worker TEXT NOT NULL,
            target_worker TEXT NOT NULL,
            allowed_tool TEXT NOT NULL,
            policy_hash TEXT NOT NULL UNIQUE
        )",
        [],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn normalize_subspawn_tool_name(tool: &str) -> String {
    if tool.eq_ignore_ascii_case("swarm:delegate") {
        "request_sub_agent".to_string()
    } else {
        tool.trim().to_ascii_lowercase()
    }
}

pub fn compute_policy_hash(input: PolicyHashInput) -> String {
    let mut hasher = Sha256::new();
    match input {
        PolicyHashInput::Relationship {
            source_worker,
            target_worker,
            allowed_tool,
        } => {
            hasher.update(b"relationship");
            hasher.update(source_worker.trim().as_bytes());
            hasher.update(b"::");
            hasher.update(target_worker.trim().as_bytes());
            hasher.update(b"::");
            hasher.update(normalize_subspawn_tool_name(&allowed_tool).as_bytes());
        }
        PolicyHashInput::Parameterized {
            source_worker,
            target_worker,
            allowed_tool,
            parameter_fingerprint,
        } => {
            hasher.update(b"parameterized");
            hasher.update(source_worker.trim().as_bytes());
            hasher.update(b"::");
            hasher.update(target_worker.trim().as_bytes());
            hasher.update(b"::");
            hasher.update(normalize_subspawn_tool_name(&allowed_tool).as_bytes());
            hasher.update(b"::");
            hasher.update(parameter_fingerprint.trim().as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

pub fn verify_policy(
    conn: &Connection,
    source_worker: &str,
    target_worker: &str,
    allowed_tool: &str,
) -> MoatResult<bool> {
    let policy_hash = compute_policy_hash(PolicyHashInput::Relationship {
        source_worker: source_worker.trim().to_string(),
        target_worker: target_worker.trim().to_string(),
        allowed_tool: allowed_tool.trim().to_string(),
    });
    let mut stmt = conn
        .prepare("SELECT 1 FROM metabrain_policies WHERE policy_hash = ?1 LIMIT 1")
        .map_err(MoatError::Database)?;
    let mut rows = stmt
        .query(params![policy_hash])
        .map_err(MoatError::Database)?;
    Ok(rows.next().map_err(MoatError::Database)?.is_some())
}

pub fn create_policy(
    conn: &Connection,
    source_worker: &str,
    target_worker: &str,
    allowed_tool: &str,
) -> MoatResult<ExecutionPolicy> {
    let policy = ExecutionPolicy {
        policy_id: uuid::Uuid::new_v4().to_string(),
        source_worker: source_worker.trim().to_string(),
        target_worker: target_worker.trim().to_string(),
        allowed_tool: normalize_subspawn_tool_name(allowed_tool),
        policy_hash: compute_policy_hash(PolicyHashInput::Relationship {
            source_worker: source_worker.trim().to_string(),
            target_worker: target_worker.trim().to_string(),
            allowed_tool: allowed_tool.trim().to_string(),
        }),
    };
    conn.execute(
        "INSERT OR IGNORE INTO metabrain_policies
         (policy_id, source_worker, target_worker, allowed_tool, policy_hash)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            policy.policy_id,
            policy.source_worker,
            policy.target_worker,
            policy.allowed_tool,
            policy.policy_hash
        ],
    )
    .map_err(MoatError::Database)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::{MetaBrain, PermissionFlags, SYSTEM_ROOT_ID};

    #[test]
    fn path_permissions_inherit_from_ancestor() {
        let mut brain = MetaBrain::new();
        brain
            .grant_path_permission("/tmp/project", PermissionFlags::FILE_READ)
            .expect("grant path");

        assert!(brain.path_has_permission("/tmp/project/src/main.rs", PermissionFlags::FILE_READ));
        assert!(!brain.path_has_permission("/tmp/other/file.txt", PermissionFlags::FILE_READ));
    }

    #[test]
    fn domain_permissions_inherit_downward() {
        let mut brain = MetaBrain::new();
        brain
            .grant_domain_permission("github.com", PermissionFlags::NET_OUTBOUND)
            .expect("grant domain");

        assert!(brain.domain_has_permission("api.github.com", PermissionFlags::NET_OUTBOUND));
        assert!(!brain.domain_has_permission("example.com", PermissionFlags::NET_OUTBOUND));
    }

    #[test]
    fn relevant_subgraph_prefers_matching_branch() {
        let mut brain = MetaBrain::new();
        brain
            .grant_path_permission("/workspace/src", PermissionFlags::FILE_READ)
            .expect("grant src");
        brain
            .grant_path_permission("/workspace/docs", PermissionFlags::FILE_READ)
            .expect("grant docs");

        let subgraph = brain
            .focused_subgraph(SYSTEM_ROOT_ID, "read src file", 2, 2)
            .expect("extract subgraph");

        assert!(subgraph
            .nodes
            .iter()
            .any(|node| node.resource.ends_with("/workspace/src")));
    }

    #[test]
    fn extracted_permission_string_is_keyword_scoped() {
        let mut brain = MetaBrain::new();
        brain
            .grant_path_permission("/workspace/src", PermissionFlags::FILE_READ)
            .expect("grant src");
        brain
            .grant_domain_permission("api.github.com", PermissionFlags::NET_OUTBOUND)
            .expect("grant domain");

        let result = brain.extract_relevant_subgraph("github");
        assert!(result.contains("(allow network-outbound (host \"api.github.com\"))"));
        assert!(!result.contains("/workspace/src"));
    }

    #[test]
    fn extracted_permission_string_reports_missing_keyword() {
        let brain = MetaBrain::new();
        let result = brain.extract_relevant_subgraph("nonexistent-scope");
        assert!(result.contains("No permissions found in MetaBrain"));
    }
}
