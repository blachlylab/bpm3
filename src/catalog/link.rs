//! Bulk link, bulk unlink, and undo.
//!
//! A run is planned against a read snapshot, shown, and then planned again
//! inside the write transaction. The write goes ahead only when the second plan
//! is the one that was shown, so nothing changes between the operator's yes and
//! the commit. The whole run is one transaction.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{
    Catalog, Index, begin, delete_ids, insert_node, load_index, resolve_all, selector_matches,
    table_name, timestamp,
};
use crate::error::Error;
use crate::link::{Expect, Mode, Rendered, RenderedStep, Source, validate_role};
use crate::model::{FileRef, NodeType, Selector};

#[derive(Debug, Clone, Default)]
pub struct LinkOptions {
    pub expects: Vec<Expect>,
    /// Add roles that are not on the known list.
    pub new_role: bool,
    /// Change the role of a link that already exists.
    pub set_role: bool,
    /// Link a file even when it is already linked to a node of the target
    /// type under the anchor and the run would create a node for it.
    pub relink: bool,
    /// Plan an unlink: every step must exist, and every link must too.
    pub unlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Link,
    /// The link exists with this role already.
    Already,
    /// The link exists with another role, and `--set-role` changes it.
    SetRole {
        old: String,
    },
    /// The file is already linked to a node of the target type under the
    /// anchor, and linking it would create a node.
    Skip,
    Unlink,
}

impl Action {
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Already => "already",
            Self::SetRole { .. } => "set-role",
            Self::Skip => "skip",
            Self::Unlink => "unlink",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannedLink {
    pub file_id: String,
    pub uri: String,
    pub target: String,
    pub role: String,
    pub action: Action,
    node: Option<NodeRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NodeRef {
    Existing(Uuid),
    Planned(usize),
}

#[derive(Debug, Clone)]
struct PlannedNode {
    node_type: NodeType,
    parent: NodeRef,
    pairs: Vec<(String, String)>,
    display: String,
}

/// What a run would do. `errors` non-empty means it cannot run.
#[derive(Debug, Clone, Default)]
pub struct LinkPlan {
    pub links: Vec<PlannedLink>,
    pub new_roles: Vec<String>,
    pub errors: Vec<String>,
    /// Coverage notes. They do not stop the run.
    pub notes: Vec<String>,
    nodes: Vec<PlannedNode>,
}

impl LinkPlan {
    /// New entities by type, in tree order.
    pub fn creates(&self) -> Vec<(NodeType, usize)> {
        let mut counts: BTreeMap<u8, (NodeType, usize)> = BTreeMap::new();
        for node in &self.nodes {
            counts
                .entry(node.node_type.rank())
                .or_insert((node.node_type, 0))
                .1 += 1;
        }
        counts.into_values().collect()
    }

    pub fn created(&self) -> usize {
        self.nodes.len()
    }

    pub fn count(&self, action: &str) -> usize {
        self.links
            .iter()
            .filter(|link| link.action.slug() == action)
            .count()
    }

    /// Everything the run would write, in order. Two plans with the same
    /// signature write the same rows.
    fn signature(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .nodes
            .iter()
            .map(|node| format!("create\t{}", node.display))
            .collect();
        for link in &self.links {
            lines.push(format!(
                "{}\t{}\t{}\t{}",
                link.action.slug(),
                link.file_id,
                link.target,
                link.role
            ));
        }
        for role in &self.new_roles {
            lines.push(format!("role\t{role}"));
        }
        lines
    }
}

#[derive(Debug, Clone)]
pub struct LinkRunRow {
    pub id: String,
    pub command: String,
    pub applied_at: String,
    pub entities_created: i64,
    pub links_created: i64,
    pub roles_changed: i64,
    pub undone_at: Option<String>,
}

/// What `bpm undo` would remove and restore. `blockers` non-empty means it
/// cannot run.
#[derive(Debug, Clone)]
pub struct UndoPlan {
    pub run: LinkRunRow,
    pub links: usize,
    pub entities: Vec<(NodeType, Uuid)>,
    /// (file id, node type, node id, role now, role to restore)
    pub roles: Vec<(String, NodeType, String, String, String)>,
    pub blockers: Vec<String>,
}

impl UndoPlan {
    fn signature(&self) -> String {
        format!(
            "{} {:?} {:?} {:?}",
            self.links, self.entities, self.roles, self.blockers
        )
    }
}

impl Catalog {
    /// The catalog locations each operand reaches. A file id reaches every
    /// location of that file. A location reaches itself if it is one, and
    /// otherwise every location below it, with its path relative to it.
    pub fn link_sources(&mut self, operands: &[FileRef]) -> Result<Vec<Source>, Error> {
        let snapshot = self.conn.transaction()?;
        let mut sources = Vec::new();
        let mut missing = Vec::new();
        for operand in operands {
            match operand {
                FileRef::Id(id) => {
                    let id = id.to_string();
                    let exists: bool = snapshot.query_row(
                        "SELECT EXISTS (SELECT 1 FROM files WHERE id = ?)",
                        [&id],
                        |row| row.get(0),
                    )?;
                    if !exists {
                        missing.push(id);
                        continue;
                    }
                    let mut stmt = snapshot.prepare_cached(
                        "SELECT uri FROM file_locations WHERE file_id = ? ORDER BY backend, uri",
                    )?;
                    let uris = stmt
                        .query_map([&id], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    if uris.is_empty() {
                        sources.push(Source {
                            file_id: id.clone(),
                            uri: id.clone(),
                            relpath: id.clone(),
                        });
                    }
                    for uri in uris {
                        let relpath = uri.rsplit('/').next().unwrap_or(&uri).to_string();
                        sources.push(Source {
                            file_id: id.clone(),
                            uri,
                            relpath,
                        });
                    }
                }
                FileRef::Location { backend, uri } => {
                    let exact: Option<String> = snapshot
                        .query_row(
                            "SELECT file_id FROM file_locations WHERE backend = ? AND uri = ?",
                            params![backend, uri],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(file_id) = exact {
                        let relpath = uri.rsplit('/').next().unwrap_or(uri).to_string();
                        sources.push(Source {
                            file_id,
                            uri: uri.clone(),
                            relpath,
                        });
                        continue;
                    }
                    // Below the location: a range on the key, at a component
                    // boundary, so /data/run1 does not reach /data/run10.
                    let prefix = if uri.ends_with('/') {
                        uri.clone()
                    } else {
                        format!("{uri}/")
                    };
                    let mut upper = prefix.clone();
                    upper.pop();
                    upper.push('0');
                    let mut stmt = snapshot.prepare_cached(
                        "SELECT file_id, uri FROM file_locations
                         WHERE backend = ? AND uri >= ? AND uri < ? ORDER BY uri",
                    )?;
                    let found = stmt
                        .query_map(params![backend, prefix, upper], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    if found.is_empty() {
                        missing.push(uri.clone());
                    }
                    for (file_id, location) in found {
                        let relpath = location[prefix.len()..].to_string();
                        sources.push(Source {
                            file_id,
                            uri: location,
                            relpath,
                        });
                    }
                }
            }
        }
        if !missing.is_empty() {
            return Err(Error::Message(format!(
                "no catalog location at or under {}; run `bpm ingest` first",
                missing.join(", ")
            )));
        }
        Ok(sources)
    }

    /// Plan a link or unlink run against a read snapshot. Writes nothing.
    pub fn plan_links(
        &mut self,
        rendered: &[Rendered],
        options: &LinkOptions,
    ) -> Result<LinkPlan, Error> {
        let snapshot = self.conn.transaction()?;
        let index = load_index(&snapshot)?;
        plan(&snapshot, &index, rendered, options)
    }

    /// Plan again inside the write transaction and, if that is the plan that
    /// was shown, write it. Returns the run id, or `None` when there was
    /// nothing to write.
    pub fn apply_links(
        &mut self,
        rendered: &[Rendered],
        options: &LinkOptions,
        shown: &LinkPlan,
        command: &str,
    ) -> Result<Option<Uuid>, Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let plan = plan(&tx, &index, rendered, options)?;
        if plan.signature() != shown.signature() || !plan.errors.is_empty() {
            return Err(changed());
        }
        if options.unlink {
            for link in &plan.links {
                let (node_type, node_id) = existing(&index, link.node)?;
                tx.execute(
                    "DELETE FROM file_links WHERE file_id = ? AND node_type = ? AND node_id = ?",
                    params![link.file_id, node_type.slug(), node_id.to_string()],
                )?;
            }
            tx.commit()?;
            return Ok(None);
        }
        let writes = plan.created() + plan.count("link") + plan.count("set-role");
        if writes == 0 {
            return Ok(None);
        }
        let run = Uuid::now_v7();
        let run_text = run.to_string();
        let stamp = timestamp();
        // First, so the role changes below can name it.
        tx.execute(
            "INSERT INTO link_runs
               (id, command, applied_at, entities_created, links_created, roles_changed)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                run_text,
                command,
                stamp,
                plan.created() as i64,
                plan.count("link") as i64,
                plan.count("set-role") as i64
            ],
        )?;
        let mut ids: Vec<Uuid> = Vec::with_capacity(plan.nodes.len());
        for node in &plan.nodes {
            let parent = match node.parent {
                NodeRef::Existing(id) => id,
                NodeRef::Planned(position) => ids[position],
            };
            let id = Uuid::now_v7();
            insert_node(&tx, node.node_type, id, Some(parent), None, &stamp)?;
            tx.execute(
                &format!(
                    "UPDATE {} SET run_id = ? WHERE id = ?",
                    table_name(node.node_type)
                ),
                params![run_text, id.to_string()],
            )?;
            for (key, value) in &node.pairs {
                tx.execute(
                    "INSERT INTO entity_metadata (node_type, node_id, key, value, updated_at)
                     VALUES (?, ?, ?, ?, ?)",
                    params![node.node_type.slug(), id.to_string(), key, value, stamp],
                )?;
            }
            ids.push(id);
        }
        let target = |link: &PlannedLink| -> Result<(NodeType, Uuid), Error> {
            match link.node {
                Some(NodeRef::Planned(position)) => {
                    Ok((plan.nodes[position].node_type, ids[position]))
                }
                other => existing(&index, other),
            }
        };
        for link in &plan.links {
            match &link.action {
                Action::Link => {
                    let (node_type, node_id) = target(link)?;
                    tx.execute(
                        "INSERT INTO file_links (file_id, node_type, node_id, role, run_id)
                         VALUES (?, ?, ?, ?, ?)",
                        params![
                            link.file_id,
                            node_type.slug(),
                            node_id.to_string(),
                            link.role,
                            run_text
                        ],
                    )?;
                }
                Action::SetRole { old } => {
                    let (node_type, node_id) = target(link)?;
                    tx.execute(
                        "UPDATE file_links SET role = ? WHERE file_id = ? AND node_type = ? AND node_id = ?",
                        params![link.role, link.file_id, node_type.slug(), node_id.to_string()],
                    )?;
                    tx.execute(
                        "INSERT INTO link_role_changes
                           (run_id, file_id, node_type, node_id, old_role, new_role)
                         VALUES (?, ?, ?, ?, ?, ?)",
                        params![
                            run_text,
                            link.file_id,
                            node_type.slug(),
                            node_id.to_string(),
                            old,
                            link.role
                        ],
                    )?;
                }
                Action::Already | Action::Skip | Action::Unlink => {}
            }
        }
        for role in &plan.new_roles {
            tx.execute(
                "INSERT OR IGNORE INTO link_roles (role, added_at) VALUES (?, ?)",
                params![role, stamp],
            )?;
        }
        tx.commit()?;
        Ok(Some(run))
    }

    /// Link runs, newest first.
    pub fn link_runs(&mut self) -> Result<Vec<LinkRunRow>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, command, applied_at, entities_created, links_created, roles_changed, undone_at
             FROM link_runs ORDER BY applied_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn plan_undo(&mut self, run: &str) -> Result<UndoPlan, Error> {
        let snapshot = self.conn.transaction()?;
        let index = load_index(&snapshot)?;
        plan_undo(&snapshot, &index, run)
    }

    pub fn apply_undo(&mut self, run: &str, shown: &UndoPlan) -> Result<(), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let plan = plan_undo(&tx, &index, run)?;
        if plan.signature() != shown.signature() || !plan.blockers.is_empty() {
            return Err(changed());
        }
        tx.execute("DELETE FROM file_links WHERE run_id = ?", [&plan.run.id])?;
        let ids: Vec<Uuid> = plan.entities.iter().map(|(_, id)| *id).collect();
        delete_ids(&tx, &ids)?;
        for (file_id, node_type, node_id, _, restore) in &plan.roles {
            tx.execute(
                "UPDATE file_links SET role = ? WHERE file_id = ? AND node_type = ? AND node_id = ?",
                params![restore, file_id, node_type.slug(), node_id],
            )?;
        }
        tx.execute(
            "UPDATE link_runs SET undone_at = ? WHERE id = ?",
            params![timestamp(), plan.run.id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn changed() -> Error {
    Error::Message(
        "the catalog changed after the plan was shown; nothing was written. Run the command again"
            .into(),
    )
}

fn existing(index: &Index, node: Option<NodeRef>) -> Result<(NodeType, Uuid), Error> {
    match node {
        Some(NodeRef::Existing(id)) => Ok((index.get(id).node_type, id)),
        _ => Err(Error::Message("link target was not resolved".into())),
    }
}

fn run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkRunRow> {
    Ok(LinkRunRow {
        id: row.get(0)?,
        command: row.get(1)?,
        applied_at: row.get(2)?,
        entities_created: row.get(3)?,
        links_created: row.get(4)?,
        roles_changed: row.get(5)?,
        undone_at: row.get(6)?,
    })
}

/// Errors repeat across files (500 files under one missing case): keep the
/// first of each message, in order.
#[derive(Default)]
struct Messages {
    list: Vec<String>,
    seen: HashSet<String>,
}

impl Messages {
    fn push(&mut self, message: String) {
        if self.seen.insert(message.clone()) {
            self.list.push(message);
        }
    }
}

struct Planner<'a> {
    conn: &'a Connection,
    index: &'a Index,
    anchors: HashMap<String, Result<Uuid, String>>,
    selectors: HashMap<Selector, HashSet<Uuid>>,
    descendants: HashMap<Uuid, HashSet<Uuid>>,
    nodes: Vec<PlannedNode>,
    planned: HashMap<NodeKey, usize>,
}

/// A planned node is one per parent, type, and pairs.
type NodeKey = (NodeRef, NodeType, Vec<(String, String)>);

impl<'a> Planner<'a> {
    fn anchor(&mut self, address: &str) -> Result<Uuid, String> {
        if let Some(found) = self.anchors.get(address) {
            return found.clone();
        }
        let found = match resolve_all(self.conn, self.index, address) {
            Ok(ids) => match ids.as_slice() {
                [id] => Ok(*id),
                [] => Err(format!("no entity matches {address}")),
                _ => Err(format!("{address} matches more than one entity")),
            },
            Err(err) => Err(format!("{address}: {err}")),
        };
        self.anchors.insert(address.to_string(), found.clone());
        found
    }

    fn matching(&mut self, selector: &Selector) -> Result<&HashSet<Uuid>, Error> {
        if !self.selectors.contains_key(selector) {
            let matched = selector_matches(self.conn, self.index, selector)?;
            self.selectors.insert(selector.clone(), matched);
        }
        Ok(&self.selectors[selector])
    }

    /// Existing children of `parent` of the step's type that carry every pair.
    fn children(&mut self, parent: Uuid, step: &RenderedStep) -> Result<Vec<Uuid>, Error> {
        let index = self.index;
        let mut found: Vec<Uuid> = index
            .children
            .get(&parent)
            .map(|children| {
                children
                    .iter()
                    .copied()
                    .filter(|id| index.get(*id).node_type == step.node_type)
                    .collect()
            })
            .unwrap_or_default();
        for selector in &step.selectors {
            let matched = self.matching(selector)?;
            found.retain(|id| matched.contains(id));
        }
        found.sort();
        Ok(found)
    }

    /// Whether resolving these steps would create a node, without planning
    /// one. An error here is reported again by [`Planner::resolve`].
    fn would_create(&mut self, anchor: Uuid, steps: &[RenderedStep]) -> Result<bool, Error> {
        let mut parent = anchor;
        for step in steps {
            if step.mode == Mode::New {
                return Ok(true);
            }
            let found = self.children(parent, step)?;
            match (found.as_slice(), step.mode) {
                ([id], _) => parent = *id,
                ([], Mode::Ensure) => return Ok(true),
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    /// The node at each step, planning new nodes as the modes allow.
    fn resolve(
        &mut self,
        anchor: Uuid,
        rendered: &Rendered,
    ) -> Result<Result<Vec<NodeRef>, String>, Error> {
        let mut path = Vec::with_capacity(rendered.steps.len());
        let mut parent = NodeRef::Existing(anchor);
        for (position, step) in rendered.steps.iter().enumerate() {
            let here = rendered.display(position + 1);
            let node = match parent {
                NodeRef::Existing(parent_id) => {
                    let found = if step.mode == Mode::New {
                        Vec::new()
                    } else {
                        self.children(parent_id, step)?
                    };
                    match (found.as_slice(), step.mode) {
                        ([id], Mode::Exist | Mode::Ensure) => NodeRef::Existing(*id),
                        ([], Mode::Exist) => {
                            return Ok(Err(format!("nothing matches {here}")));
                        }
                        ([], Mode::Ensure | Mode::New) => self.plan_node(parent, step, here),
                        (_, _) => {
                            return Ok(Err(format!(
                                "{here} matches {} entities; add pairs that tell them apart",
                                found.len()
                            )));
                        }
                    }
                }
                NodeRef::Planned(_) => match step.mode {
                    Mode::Exist => {
                        return Ok(Err(format!(
                            "{here} must exist, but its parent is new in this run"
                        )));
                    }
                    Mode::Ensure | Mode::New => self.plan_node(parent, step, here),
                },
            };
            path.push(node);
            parent = node;
        }
        Ok(Ok(path))
    }

    fn plan_node(&mut self, parent: NodeRef, step: &RenderedStep, display: String) -> NodeRef {
        let key = (parent, step.node_type, step.pairs());
        if let Some(&position) = self.planned.get(&key) {
            return NodeRef::Planned(position);
        }
        let position = self.nodes.len();
        self.nodes.push(PlannedNode {
            node_type: step.node_type,
            parent,
            pairs: key.2.clone(),
            display,
        });
        self.planned.insert(key, position);
        NodeRef::Planned(position)
    }

    fn under(&mut self, anchor: Uuid) -> &HashSet<Uuid> {
        let index = self.index;
        self.descendants.entry(anchor).or_insert_with(|| {
            let mut out = vec![anchor];
            super::collect_descendants(anchor, index, &mut out);
            out.into_iter().collect()
        })
    }
}

fn plan(
    conn: &Connection,
    index: &Index,
    rendered: &[Rendered],
    options: &LinkOptions,
) -> Result<LinkPlan, Error> {
    let mut planner = Planner {
        conn,
        index,
        anchors: HashMap::new(),
        selectors: HashMap::new(),
        descendants: HashMap::new(),
        nodes: Vec::new(),
        planned: HashMap::new(),
    };
    let mut errors = Messages::default();
    let known: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT role FROM link_roles")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?
    };
    let mut new_roles: Vec<String> = Vec::new();
    let mut links = Vec::with_capacity(rendered.len());
    // (step position, node) → (display, files)
    let mut reached: Vec<HashMap<NodeRef, (String, usize)>> =
        vec![HashMap::new(); rendered.first().map_or(0, |r| r.steps.len())];
    let mut parents_seen: Vec<HashSet<Uuid>> = vec![HashSet::new(); reached.len()];
    let mut link_stmt =
        conn.prepare_cached("SELECT node_type, node_id, role FROM file_links WHERE file_id = ?")?;
    for item in rendered {
        if !options.unlink {
            if let Err(message) = validate_role(&item.role) {
                errors.push(message);
                continue;
            }
            if !known.contains(&item.role) && !new_roles.contains(&item.role) {
                if options.new_role {
                    new_roles.push(item.role.clone());
                } else {
                    let mut listed: Vec<&String> = known.iter().collect();
                    listed.sort();
                    errors.push(format!(
                        "role {} is new to this catalog (known: {}); pass --new-role to add it",
                        item.role,
                        listed
                            .iter()
                            .map(|role| role.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    continue;
                }
            }
        }
        let anchor = match planner.anchor(&item.anchor) {
            Ok(anchor) => anchor,
            Err(message) => {
                errors.push(message);
                continue;
            }
        };
        if let Some(first) = item.steps.first() {
            let anchor_type = index.get(anchor).node_type;
            if first.node_type.parent_type() != Some(anchor_type) {
                errors.push(format!(
                    "{} is a {}; a {} step goes under a {}",
                    item.anchor,
                    anchor_type.slug(),
                    first.node_type.slug(),
                    first
                        .node_type
                        .parent_type()
                        .map(NodeType::slug)
                        .unwrap_or("nothing")
                ));
                continue;
            }
        }
        let file_links: Vec<(String, String, String)> = link_stmt
            .query_map([&item.file_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        if !options.unlink && !options.relink && planner.would_create(anchor, &item.steps)? {
            let leaf = item
                .steps
                .last()
                .map(|step| step.node_type)
                .unwrap_or_else(|| index.get(anchor).node_type);
            let under = planner.under(anchor);
            let linked_here = file_links.iter().any(|(node_type, node_id, _)| {
                node_type == leaf.slug()
                    && Uuid::parse_str(node_id).is_ok_and(|id| under.contains(&id))
            });
            if linked_here {
                links.push(PlannedLink {
                    file_id: item.file_id.clone(),
                    uri: item.uri.clone(),
                    target: item.target(),
                    role: item.role.clone(),
                    action: Action::Skip,
                    node: None,
                });
                continue;
            }
        }
        let path = match planner.resolve(anchor, item)? {
            Ok(path) => path,
            Err(message) => {
                errors.push(message);
                continue;
            }
        };
        let node = path.last().copied().unwrap_or(NodeRef::Existing(anchor));
        let current = match node {
            NodeRef::Existing(id) => {
                let node_type = index.get(id).node_type;
                file_links
                    .iter()
                    .find(|(t, n, _)| t == node_type.slug() && *n == id.to_string())
                    .map(|(_, _, role)| role.clone())
            }
            NodeRef::Planned(_) => None,
        };
        let action = match (options.unlink, current) {
            (true, Some(_)) => Action::Unlink,
            (true, None) => {
                errors.push(format!("{} is not linked to {}", item.uri, item.target()));
                continue;
            }
            (false, None) => Action::Link,
            (false, Some(role)) if role == item.role => Action::Already,
            (false, Some(role)) if options.set_role => Action::SetRole { old: role },
            (false, Some(role)) => {
                errors.push(format!(
                    "{} is already linked to {} as {role}; pass --set-role to make it {}",
                    item.uri,
                    item.target(),
                    item.role
                ));
                continue;
            }
        };
        let mut parent = NodeRef::Existing(anchor);
        for (position, step_node) in path.iter().enumerate() {
            let entry = reached[position]
                .entry(*step_node)
                .or_insert_with(|| (item.display(position + 1), 0));
            entry.1 += 1;
            if let (NodeRef::Existing(parent_id), Mode::Exist) = (parent, item.steps[position].mode)
            {
                parents_seen[position].insert(parent_id);
            }
            parent = *step_node;
        }
        links.push(PlannedLink {
            file_id: item.file_id.clone(),
            uri: item.uri.clone(),
            target: item.target(),
            role: item.role.clone(),
            action,
            node: Some(node),
        });
    }

    // --expect: every node the run touches at that step has N..M files.
    let step_types: Vec<(NodeType, Mode)> = rendered
        .first()
        .map(|r| r.steps.iter().map(|s| (s.node_type, s.mode)).collect())
        .unwrap_or_default();
    for expect in &options.expects {
        let Some(position) = step_types
            .iter()
            .position(|(node_type, _)| *node_type == expect.node_type)
        else {
            continue;
        };
        let mut nodes: Vec<&(String, usize)> = reached[position].values().collect();
        nodes.sort();
        for (display, files) in nodes {
            if *files < expect.min || *files > expect.max {
                let noun = if *files == 1 { "file" } else { "files" };
                errors.push(format!(
                    "--expect {}={}: {display} gets {files} {noun}",
                    expect.node_type.slug(),
                    expect.describe()
                ));
            }
        }
    }

    // Coverage: existing nodes a must-exist step could have reached, that no
    // file did.
    let mut notes = Vec::new();
    for (position, (node_type, mode)) in step_types.iter().enumerate() {
        if *mode != Mode::Exist || parents_seen[position].is_empty() {
            continue;
        }
        let siblings: usize = parents_seen[position]
            .iter()
            .flat_map(|parent| index.children.get(parent).into_iter().flatten())
            .filter(|id| {
                index.get(**id).node_type == *node_type
                    && !reached[position].contains_key(&NodeRef::Existing(**id))
            })
            .count();
        if siblings > 0 {
            notes.push(format!(
                "{siblings} existing {} under the same parents got no files",
                node_type.slug()
            ));
        }
    }

    Ok(LinkPlan {
        links,
        new_roles,
        errors: errors.list,
        notes,
        nodes: planner.nodes,
    })
}

const NODE_TYPES_WITH_RUNS: [NodeType; 4] = [
    NodeType::Case,
    NodeType::Sample,
    NodeType::RawData,
    NodeType::Analysis,
];

fn plan_undo(conn: &Connection, index: &Index, run: &str) -> Result<UndoPlan, Error> {
    let row = conn
        .query_row(
            "SELECT id, command, applied_at, entities_created, links_created, roles_changed, undone_at
             FROM link_runs WHERE id = ?",
            [run],
            run_row,
        )
        .optional()?
        .ok_or_else(|| Error::Message(format!("no link run {run}; `bpm undo --list` shows them")))?;
    if let Some(when) = &row.undone_at {
        return Err(Error::Message(format!(
            "link run {run} was undone at {when}"
        )));
    }
    let links: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_links WHERE run_id = ?",
        [run],
        |r| r.get(0),
    )?;
    let mut entities = Vec::new();
    for node_type in NODE_TYPES_WITH_RUNS {
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM {} WHERE run_id = ? ORDER BY id",
            table_name(node_type)
        ))?;
        for id in stmt.query_map([run], |r| r.get::<_, String>(0))? {
            let id = id?;
            let id = Uuid::parse_str(&id)
                .map_err(|err| Error::Message(format!("invalid entity id {id}: {err}")))?;
            entities.push((node_type, id));
        }
    }
    let created: HashSet<Uuid> = entities.iter().map(|(_, id)| *id).collect();
    let mut blockers = Vec::new();
    for (node_type, id) in &entities {
        for child in index.children.get(id).into_iter().flatten() {
            if created.contains(child) {
                continue;
            }
            let child_type = index.get(*child).node_type;
            let from: Option<String> = conn
                .query_row(
                    &format!("SELECT run_id FROM {} WHERE id = ?", table_name(child_type)),
                    [child.to_string()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            blockers.push(format!(
                "{} {id} has a {} {child} that this run did not create{}",
                node_type.slug(),
                child_type.slug(),
                later_run(from)
            ));
        }
        let mut stmt = conn.prepare_cached(
            "SELECT file_id, run_id FROM file_links
             WHERE node_type = ? AND node_id = ? AND (run_id IS NULL OR run_id <> ?)",
        )?;
        let foreign = stmt
            .query_map(params![node_type.slug(), id.to_string(), run], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (file_id, from) in foreign {
            blockers.push(format!(
                "{} {id} has file {file_id} linked after this run{}",
                node_type.slug(),
                later_run(from)
            ));
        }
        let mut stmt = conn.prepare_cached(
            "SELECT key FROM entity_metadata
             WHERE node_type = ? AND node_id = ? AND updated_at <> ? ORDER BY key",
        )?;
        let keys = stmt
            .query_map(
                params![node_type.slug(), id.to_string(), row.applied_at],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if !keys.is_empty() {
            blockers.push(format!(
                "{} {id} has metadata set after this run: {}",
                node_type.slug(),
                keys.join(", ")
            ));
        }
    }
    let mut roles = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT c.file_id, c.node_type, c.node_id, c.old_role, c.new_role, l.role
             FROM link_role_changes c
             LEFT JOIN file_links l
               ON l.file_id = c.file_id AND l.node_type = c.node_type AND l.node_id = c.node_id
             WHERE c.run_id = ? ORDER BY c.file_id, c.node_type, c.node_id",
        )?;
        let rows = stmt
            .query_map([run], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (file_id, node_type, node_id, old, new, now) in rows {
            let Some(node_type) = NodeType::parse(&node_type) else {
                continue;
            };
            match now {
                // The link was removed since; there is nothing to restore.
                None => {}
                Some(now) if now == new => roles.push((file_id, node_type, node_id, now, old)),
                Some(now) => blockers.push(format!(
                    "file {file_id} on {} {node_id} is now {now}, not {new}, so its role was changed after this run",
                    node_type.slug()
                )),
            }
        }
    }
    Ok(UndoPlan {
        run: row,
        links: links as usize,
        entities,
        roles,
        blockers,
    })
}

fn later_run(from: Option<String>) -> String {
    match from {
        Some(run) => format!(" (link run {run}; undo it first)"),
        None => String::new(),
    }
}
