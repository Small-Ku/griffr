use crate::error::{Error, Result};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use super::types::Task;

static DEPENDENCY_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Stable handle used by dynamically discovered work to depend on a node that
/// was installed by an earlier graph expansion. Tokens are command-local in
/// practice, but globally unique so accidental cross-graph reuse cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskDependencyToken(u64);

impl TaskDependencyToken {
    pub(crate) fn new() -> Self {
        Self(DEPENDENCY_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    fn new(index: usize) -> Result<Self> {
        let raw = u32::try_from(index)
            .map_err(|_| Error::TaskPool("task graph contains more than u32::MAX nodes".into()))?;
        Ok(Self(raw))
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }

    #[cfg(test)]
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Pending,
    Ready,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl NodeState {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskGraphSummary {
    pub total_nodes: usize,
    pub pending_nodes: usize,
    pub ready_nodes: usize,
    pub running_nodes: usize,
    pub waiting_nodes: usize,
    pub succeeded_nodes: usize,
    pub failed_nodes: usize,
    pub cancelled_nodes: usize,
    pub dynamic_expansions: usize,
}

#[derive(Debug)]
struct BuilderNode {
    task: Task,
    dependencies: Vec<NodeId>,
}

/// Builds a command-scoped DAG. A newly added node may depend only on nodes
/// already present in the builder, so cycles and forward references are
/// rejected at construction time.
#[derive(Debug, Default)]
pub struct TaskGraphBuilder {
    nodes: Vec<BuilderNode>,
}

impl TaskGraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root(&mut self, task: Task) -> NodeId {
        self.add_task(task, std::iter::empty())
            .expect("root task insertion cannot fail")
    }

    pub fn add_task(
        &mut self,
        task: Task,
        dependencies: impl IntoIterator<Item = NodeId>,
    ) -> Result<NodeId> {
        let id = NodeId::new(self.nodes.len())?;
        let dependencies = dependencies
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for dependency in &dependencies {
            if dependency.index() >= self.nodes.len() {
                return Err(Error::TaskPool(format!(
                    "task graph node {} depends on unknown or forward node {}",
                    id.index(),
                    dependency.index(),
                )));
            }
        }
        self.nodes.push(BuilderNode { task, dependencies });
        Ok(id)
    }

    pub fn build(self) -> TaskGraph {
        self.build_checked()
            .expect("validated task graph builder failed to build")
    }

    pub fn build_checked(self) -> Result<TaskGraph> {
        let mut graph_nodes = self
            .nodes
            .iter()
            .map(|node| TaskNode {
                task: None,
                state: NodeState::Pending,
                remaining_dependencies: node.dependencies.len(),
                dependents: Vec::new(),
                waiters: Vec::new(),
                waiting_remaining: 0,
                continuation: false,
            })
            .collect::<Vec<_>>();

        for (index, node) in self.nodes.iter().enumerate() {
            let id = NodeId::new(index)?;
            for dependency in &node.dependencies {
                graph_nodes[dependency.index()].dependents.push(id);
            }
        }
        for (graph_node, builder_node) in graph_nodes.iter_mut().zip(self.nodes) {
            graph_node.task = Some(builder_node.task);
        }

        let unresolved = graph_nodes.len();
        Ok(TaskGraph {
            nodes: graph_nodes,
            unresolved,
            dynamic_expansions: 0,
            bindings: HashMap::new(),
        })
    }
}

#[derive(Debug)]
struct TaskNode {
    task: Option<Task>,
    state: NodeState,
    remaining_dependencies: usize,
    dependents: Vec<NodeId>,
    waiters: Vec<NodeId>,
    waiting_remaining: usize,
    continuation: bool,
}

/// A command-scoped, append-only task DAG. Executors may discover a dynamic
/// subgraph; the executing parent then stays in `Waiting` until every leaf of
/// that subgraph succeeds.
#[derive(Debug)]
pub struct TaskGraph {
    nodes: Vec<TaskNode>,
    unresolved: usize,
    dynamic_expansions: usize,
    bindings: HashMap<TaskDependencyToken, NodeId>,
}

impl TaskGraph {
    pub fn builder() -> TaskGraphBuilder {
        TaskGraphBuilder::new()
    }

    pub fn from_tasks(tasks: Vec<Task>) -> Self {
        let mut builder = TaskGraphBuilder::new();
        for task in tasks {
            builder.add_root(task);
        }
        builder.build()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_state(&self, id: NodeId) -> Option<NodeState> {
        self.nodes.get(id.index()).map(|node| node.state)
    }

    pub fn summary(&self) -> TaskGraphSummary {
        let mut summary = TaskGraphSummary {
            total_nodes: self.nodes.len(),
            dynamic_expansions: self.dynamic_expansions,
            ..TaskGraphSummary::default()
        };
        for node in &self.nodes {
            let count = match node.state {
                NodeState::Pending => &mut summary.pending_nodes,
                NodeState::Ready => &mut summary.ready_nodes,
                NodeState::Running => &mut summary.running_nodes,
                NodeState::Waiting => &mut summary.waiting_nodes,
                NodeState::Succeeded => &mut summary.succeeded_nodes,
                NodeState::Failed => &mut summary.failed_nodes,
                NodeState::Cancelled => &mut summary.cancelled_nodes,
            };
            *count = count.saturating_add(1);
        }
        summary
    }

    pub(crate) fn has_unresolved(&self) -> bool {
        self.unresolved > 0
    }

    pub(crate) fn unresolved_count(&self) -> usize {
        self.unresolved
    }

    pub(crate) fn start(&mut self) -> Vec<ReadyTask> {
        let mut ready = Vec::new();
        for index in 0..self.nodes.len() {
            let id = NodeId::new(index).expect("existing graph node id must fit u32");
            self.activate_if_ready(id, &mut ready);
        }
        ready
    }

    pub(crate) fn is_ready(&self, id: NodeId) -> bool {
        self.nodes
            .get(id.index())
            .is_some_and(|node| node.state == NodeState::Ready)
    }

    pub(crate) fn mark_running(&mut self, id: NodeId) -> Result<()> {
        let node = self.nodes.get_mut(id.index()).ok_or_else(|| {
            Error::TaskPool(format!(
                "scheduler started unknown graph node {}",
                id.index()
            ))
        })?;
        if node.state != NodeState::Ready {
            return Err(Error::TaskPool(format!(
                "scheduler started graph node {} from invalid state {:?}",
                id.index(),
                node.state,
            )));
        }
        node.state = NodeState::Running;
        Ok(())
    }

    pub(crate) fn complete(
        &mut self,
        id: NodeId,
        execution: TaskExecution,
    ) -> Result<Vec<ReadyTask>> {
        let state = self.nodes.get(id.index()).map(|node| node.state);
        if state != Some(NodeState::Running) {
            return Err(Error::TaskPool(format!(
                "completion received for graph node {} outside Running state",
                id.index(),
            )));
        }

        let mut ready = Vec::new();
        match execution {
            TaskExecution::Succeeded => self.finish_succeeded(id, &mut ready),
            TaskExecution::Failed { .. } => self.finish_failed(id, &mut ready),
            TaskExecution::Cancelled => self.finish_cancelled(id, &mut ready),
            TaskExecution::Expand(expansion) => {
                self.install_expansion(id, expansion, &mut ready)?;
            }
        }
        Ok(ready)
    }

    /// Returns every existing node whose completion currently depends on
    /// `root`, either through an ordinary DAG edge or through a dynamic-parent
    /// waiter edge. A newly expanded child must not depend on any node in this
    /// set, otherwise the parent would wait for a child that waits for the
    /// parent (directly or transitively).
    fn completion_dependents_of(&self, root: NodeId) -> BTreeSet<NodeId> {
        let mut reachable = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let node = &self.nodes[id.index()];
            pending.extend(node.dependents.iter().copied());
            pending.extend(node.waiters.iter().copied());
        }
        reachable
    }

    fn install_expansion(
        &mut self,
        parent: NodeId,
        expansion: GraphExpansion,
        ready: &mut Vec<ReadyTask>,
    ) -> Result<()> {
        let GraphExpansion { nodes } = expansion;
        if nodes.is_empty() {
            self.finish_succeeded(parent, ready);
            return Ok(());
        }

        let base = self.nodes.len();
        let global_ids = (0..nodes.len())
            .map(|offset| NodeId::new(base.saturating_add(offset)))
            .collect::<Result<Vec<_>>>()?;

        // Resolve bindings and all dependency references before mutating the
        // graph. A token may refer to an existing node or to an earlier node in
        // this same append-only expansion, never to a forward node.
        let mut local_bindings = HashMap::new();
        for (local_index, node) in nodes.iter().enumerate() {
            if let Some(binding) = node.binding {
                if self.bindings.contains_key(&binding)
                    || local_bindings.insert(binding, local_index).is_some()
                {
                    return Err(Error::TaskPool(format!(
                        "dynamic graph dependency token {:?} is bound more than once",
                        binding,
                    )));
                }
            }
        }

        let completion_dependents = self.completion_dependents_of(parent);
        let mut resolved_dependencies = Vec::with_capacity(nodes.len());
        let mut blocked = vec![false; nodes.len()];
        let mut out_degree = vec![0usize; nodes.len()];
        for (local_index, node) in nodes.iter().enumerate() {
            let mut dependencies = BTreeSet::new();
            for dependency in &node.dependencies {
                if dependency.0 >= local_index {
                    return Err(Error::TaskPool(format!(
                        "dynamic graph node {local_index} has a forward dependency on {}",
                        dependency.0,
                    )));
                }
                dependencies.insert(global_ids[dependency.0]);
            }
            for token in &node.token_dependencies {
                let dependency = if let Some(existing) = self.bindings.get(token).copied() {
                    if completion_dependents.contains(&existing) {
                        return Err(Error::TaskPool(format!(
                            "dynamic graph node {local_index} creates a cycle through token {:?}",
                            token,
                        )));
                    }
                    existing
                } else if let Some(bound_local) = local_bindings.get(token).copied() {
                    if bound_local >= local_index {
                        return Err(Error::TaskPool(format!(
                            "dynamic graph node {local_index} has a forward token dependency"
                        )));
                    }
                    global_ids[bound_local]
                } else {
                    return Err(Error::TaskPool(format!(
                        "dynamic graph node {local_index} depends on unbound token {:?}",
                        token,
                    )));
                };
                dependencies.insert(dependency);
            }

            let dependencies = dependencies.into_iter().collect::<Vec<_>>();
            for dependency in &dependencies {
                if dependency.index() >= base {
                    out_degree[dependency.index() - base] =
                        out_degree[dependency.index() - base].saturating_add(1);
                } else if matches!(
                    self.nodes[dependency.index()].state,
                    NodeState::Failed | NodeState::Cancelled
                ) {
                    blocked[local_index] = true;
                }
            }
            resolved_dependencies.push(dependencies);
        }

        for (local_index, node) in nodes.iter().enumerate() {
            let unresolved_dependencies = resolved_dependencies[local_index]
                .iter()
                .filter(|dependency| {
                    dependency.index() >= base
                        || self.nodes[dependency.index()].state != NodeState::Succeeded
                })
                .count();
            self.nodes.push(TaskNode {
                task: None,
                state: NodeState::Pending,
                remaining_dependencies: unresolved_dependencies,
                dependents: Vec::new(),
                waiters: Vec::new(),
                waiting_remaining: 0,
                continuation: true,
            });
            if let Some(binding) = node.binding {
                self.bindings.insert(binding, global_ids[local_index]);
            }
        }

        for (local_index, dependencies) in resolved_dependencies.iter().enumerate() {
            let node_id = global_ids[local_index];
            for dependency in dependencies {
                if dependency.index() >= base
                    || self.nodes[dependency.index()].state != NodeState::Succeeded
                {
                    self.nodes[dependency.index()].dependents.push(node_id);
                }
            }
        }
        for (global_id, node) in global_ids.iter().copied().zip(nodes) {
            self.nodes[global_id.index()].task = Some(node.task);
        }

        self.unresolved = self.unresolved.saturating_add(global_ids.len());
        self.dynamic_expansions = self.dynamic_expansions.saturating_add(1);

        let terminals = out_degree
            .into_iter()
            .enumerate()
            .filter_map(|(index, degree)| (degree == 0).then_some(global_ids[index]))
            .collect::<Vec<_>>();
        if terminals.is_empty() {
            return Err(Error::TaskPool(
                "dynamic task expansion has no terminal nodes".to_string(),
            ));
        }

        let parent_node = &mut self.nodes[parent.index()];
        parent_node.state = NodeState::Waiting;
        parent_node.waiting_remaining = terminals.len();
        for terminal in terminals {
            self.nodes[terminal.index()].waiters.push(parent);
        }
        for (local_index, id) in global_ids.iter().copied().enumerate() {
            if blocked[local_index] {
                self.cancel_node(id, ready);
            }
        }
        for id in global_ids {
            self.activate_if_ready(id, ready);
        }
        Ok(())
    }

    fn activate_if_ready(&mut self, id: NodeId, ready: &mut Vec<ReadyTask>) {
        let node = &mut self.nodes[id.index()];
        if node.state != NodeState::Pending || node.remaining_dependencies != 0 {
            return;
        }
        node.state = NodeState::Ready;
        let task = node
            .task
            .take()
            .expect("pending graph node became ready without a task");
        ready.push(ReadyTask {
            id,
            task,
            continuation: node.continuation,
        });
    }

    fn finish_succeeded(&mut self, id: NodeId, ready: &mut Vec<ReadyTask>) {
        if self.nodes[id.index()].state.is_terminal() {
            return;
        }
        self.nodes[id.index()].state = NodeState::Succeeded;
        self.unresolved = self.unresolved.saturating_sub(1);

        let dependents = self.nodes[id.index()].dependents.clone();
        for dependent in dependents {
            self.nodes[dependent.index()].remaining_dependencies = self.nodes[dependent.index()]
                .remaining_dependencies
                .saturating_sub(1);
            self.activate_if_ready(dependent, ready);
        }
        self.notify_waiters(id, true, ready);
    }

    fn finish_failed(&mut self, id: NodeId, ready: &mut Vec<ReadyTask>) {
        if self.nodes[id.index()].state.is_terminal() {
            return;
        }
        self.nodes[id.index()].state = NodeState::Failed;
        self.unresolved = self.unresolved.saturating_sub(1);

        let dependents = self.nodes[id.index()].dependents.clone();
        for dependent in dependents {
            self.cancel_node(dependent, ready);
        }
        self.notify_waiters(id, false, ready);
    }

    fn finish_cancelled(&mut self, id: NodeId, ready: &mut Vec<ReadyTask>) {
        if self.nodes[id.index()].state.is_terminal() {
            return;
        }
        self.nodes[id.index()].state = NodeState::Cancelled;
        self.unresolved = self.unresolved.saturating_sub(1);

        let dependents = self.nodes[id.index()].dependents.clone();
        for dependent in dependents {
            self.cancel_node(dependent, ready);
        }
        self.notify_waiters(id, false, ready);
    }

    fn cancel_node(&mut self, id: NodeId, ready: &mut Vec<ReadyTask>) {
        let state = self.nodes[id.index()].state;
        if state.is_terminal() {
            return;
        }
        if matches!(state, NodeState::Running | NodeState::Waiting) {
            // A dependent cannot legitimately be running before its upstream
            // dependency finishes. Do not invalidate an in-flight job if this
            // invariant is ever violated by a future graph extension.
            return;
        }

        self.nodes[id.index()].state = NodeState::Cancelled;
        self.unresolved = self.unresolved.saturating_sub(1);
        let dependents = self.nodes[id.index()].dependents.clone();
        for dependent in dependents {
            self.cancel_node(dependent, ready);
        }
        self.notify_waiters(id, false, ready);
    }

    fn notify_waiters(&mut self, id: NodeId, succeeded: bool, ready: &mut Vec<ReadyTask>) {
        let waiters = std::mem::take(&mut self.nodes[id.index()].waiters);
        for waiter in waiters {
            if self.nodes[waiter.index()].state != NodeState::Waiting {
                continue;
            }
            if !succeeded {
                self.finish_failed(waiter, ready);
                continue;
            }

            let should_finish = {
                let node = &mut self.nodes[waiter.index()];
                node.waiting_remaining = node.waiting_remaining.saturating_sub(1);
                node.waiting_remaining == 0
            };
            if should_finish {
                self.finish_succeeded(waiter, ready);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReadyTask {
    pub(crate) id: NodeId,
    pub(crate) task: Task,
    pub(crate) continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExpansionNodeId(usize);

#[derive(Debug)]
struct ExpansionNode {
    task: Task,
    dependencies: Vec<ExpansionNodeId>,
    token_dependencies: Vec<TaskDependencyToken>,
    binding: Option<TaskDependencyToken>,
}

/// Append-only dynamic subgraph produced by one task execution. Local node
/// dependencies model joins discovered in this execution; dependency tokens
/// connect later discoveries to nodes installed by earlier expansions.
#[derive(Debug, Default)]
pub(crate) struct GraphExpansion {
    nodes: Vec<ExpansionNode>,
}

impl GraphExpansion {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn single(task: Task) -> Self {
        let mut expansion = Self::new();
        expansion.add_root(task);
        expansion
    }

    pub(crate) fn parallel(tasks: impl IntoIterator<Item = Task>) -> Self {
        let mut expansion = Self::new();
        for task in tasks {
            expansion.add_root(task);
        }
        expansion
    }

    pub(crate) fn add_root(&mut self, task: Task) -> ExpansionNodeId {
        self.add_task(task, std::iter::empty())
            .expect("root expansion task insertion cannot fail")
    }

    pub(crate) fn add_root_bound(
        &mut self,
        task: Task,
        binding: TaskDependencyToken,
    ) -> ExpansionNodeId {
        self.add_task_internal(task, std::iter::empty(), std::iter::empty(), Some(binding))
            .expect("bound root expansion task insertion cannot fail")
    }

    pub(crate) fn add_root_with_tokens(
        &mut self,
        task: Task,
        token_dependencies: impl IntoIterator<Item = TaskDependencyToken>,
    ) -> Result<ExpansionNodeId> {
        self.add_task_internal(task, std::iter::empty(), token_dependencies, None)
    }

    pub(crate) fn add_task(
        &mut self,
        task: Task,
        dependencies: impl IntoIterator<Item = ExpansionNodeId>,
    ) -> Result<ExpansionNodeId> {
        self.add_task_internal(task, dependencies, std::iter::empty(), None)
    }

    pub(crate) fn add_task_with_tokens(
        &mut self,
        task: Task,
        dependencies: impl IntoIterator<Item = ExpansionNodeId>,
        token_dependencies: impl IntoIterator<Item = TaskDependencyToken>,
    ) -> Result<ExpansionNodeId> {
        self.add_task_internal(task, dependencies, token_dependencies, None)
    }

    fn add_task_internal(
        &mut self,
        task: Task,
        dependencies: impl IntoIterator<Item = ExpansionNodeId>,
        token_dependencies: impl IntoIterator<Item = TaskDependencyToken>,
        binding: Option<TaskDependencyToken>,
    ) -> Result<ExpansionNodeId> {
        let id = ExpansionNodeId(self.nodes.len());
        let dependencies: Vec<ExpansionNodeId> = dependencies
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for dependency in &dependencies {
            if dependency.0 >= self.nodes.len() {
                return Err(Error::TaskPool(format!(
                    "dynamic graph node {} depends on unknown or forward node {}",
                    id.0, dependency.0,
                )));
            }
        }
        let token_dependencies: Vec<TaskDependencyToken> = token_dependencies
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.nodes.push(ExpansionNode {
            task,
            dependencies,
            token_dependencies,
            binding,
        });
        Ok(id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[derive(Debug)]
pub(crate) enum TaskExecution {
    Succeeded,
    Failed { reason: String, report: bool },
    Cancelled,
    Expand(GraphExpansion),
}

impl TaskExecution {
    pub(crate) fn succeeded() -> Self {
        Self::Succeeded
    }

    pub(crate) fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            report: true,
        }
    }

    pub(crate) fn silent_failure(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            report: false,
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self::Cancelled
    }

    pub(crate) fn expand(expansion: GraphExpansion) -> Self {
        if expansion.is_empty() {
            Self::Succeeded
        } else {
            Self::Expand(expansion)
        }
    }

    pub(crate) fn then(task: Task) -> Self {
        Self::Expand(GraphExpansion::single(task))
    }

    pub(crate) fn failure_details(&self) -> Option<(&str, bool)> {
        match self {
            Self::Failed { reason, report } => Some((reason, *report)),
            Self::Succeeded | Self::Cancelled | Self::Expand(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        GraphExpansion, NodeId, NodeState, TaskDependencyToken, TaskExecution, TaskGraphBuilder,
    };
    use crate::runtime::task_pool::Task;

    fn task(name: &str) -> Task {
        Task::Hardlink {
            src: PathBuf::from(format!("{name}.src")),
            dest: PathBuf::from(name),
        }
    }

    #[test]
    fn fan_in_releases_only_after_all_dependencies_succeed() {
        let mut builder = TaskGraphBuilder::new();
        let left = builder.add_root(task("left"));
        let right = builder.add_root(task("right"));
        let join = builder.add_task(task("join"), [left, right]).unwrap();
        let mut graph = builder.build_checked().unwrap();

        let ready = graph.start();
        assert_eq!(ready.len(), 2);
        graph.mark_running(left).unwrap();
        assert!(graph
            .complete(left, TaskExecution::succeeded())
            .unwrap()
            .is_empty());
        assert_eq!(graph.node_state(join), Some(NodeState::Pending));

        graph.mark_running(right).unwrap();
        let ready = graph.complete(right, TaskExecution::succeeded()).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, join);
    }

    #[test]
    fn dynamic_expansion_keeps_parent_waiting_for_leaf() {
        let mut builder = TaskGraphBuilder::new();
        let parent = builder.add_root(task("parent"));
        let dependent = builder.add_task(task("dependent"), [parent]).unwrap();
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(parent).unwrap();

        let ready = graph
            .complete(
                parent,
                TaskExecution::expand(GraphExpansion::single(task("child"))),
            )
            .unwrap();
        assert_eq!(graph.node_state(parent), Some(NodeState::Waiting));
        assert_eq!(graph.node_state(dependent), Some(NodeState::Pending));
        assert_eq!(ready.len(), 1);

        let child = ready[0].id;
        graph.mark_running(child).unwrap();
        let ready = graph.complete(child, TaskExecution::succeeded()).unwrap();
        assert_eq!(graph.node_state(parent), Some(NodeState::Succeeded));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, dependent);
    }

    #[test]
    fn failed_dependency_cancels_only_its_descendants() {
        let mut builder = TaskGraphBuilder::new();
        let failed_root = builder.add_root(task("failed-root"));
        let independent = builder.add_root(task("independent"));
        let dependent = builder.add_task(task("dependent"), [failed_root]).unwrap();
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(failed_root).unwrap();
        graph
            .complete(failed_root, TaskExecution::failed("boom"))
            .unwrap();

        assert_eq!(graph.node_state(dependent), Some(NodeState::Cancelled));
        assert_eq!(graph.node_state(independent), Some(NodeState::Ready));
    }

    #[test]
    fn duplicate_dependencies_are_collapsed() {
        let mut builder = TaskGraphBuilder::new();
        let root = builder.add_root(task("root"));
        let dependent = builder.add_task(task("dependent"), [root, root]).unwrap();
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(root).unwrap();

        let ready = graph.complete(root, TaskExecution::succeeded()).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, dependent);
    }

    #[test]
    fn cooperative_cancellation_fails_the_waiting_parent_without_reporting_failure() {
        let mut builder = TaskGraphBuilder::new();
        let parent = builder.add_root(task("parent"));
        let dependent = builder.add_task(task("dependent"), [parent]).unwrap();
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(parent).unwrap();

        let ready = graph
            .complete(
                parent,
                TaskExecution::expand(GraphExpansion::single(task("child"))),
            )
            .unwrap();
        let child = ready[0].id;
        graph.mark_running(child).unwrap();
        graph.complete(child, TaskExecution::cancelled()).unwrap();

        assert_eq!(graph.node_state(child), Some(NodeState::Cancelled));
        assert_eq!(graph.node_state(parent), Some(NodeState::Failed));
        assert_eq!(graph.node_state(dependent), Some(NodeState::Cancelled));
    }
    #[test]
    fn dynamic_node_waits_for_bound_node_from_an_earlier_expansion() {
        let mut builder = TaskGraphBuilder::new();
        let producer = builder.add_root(task("producer"));
        let planner = builder.add_root(task("planner"));
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();

        graph.mark_running(producer).unwrap();
        let token = TaskDependencyToken::new();
        let mut produced = GraphExpansion::new();
        produced.add_root_bound(task("volume"), token);
        let ready = graph
            .complete(producer, TaskExecution::expand(produced))
            .unwrap();
        let volume = ready[0].id;

        graph.mark_running(planner).unwrap();
        let mut planned = GraphExpansion::new();
        planned
            .add_root_with_tokens(task("index"), [token])
            .unwrap();
        let ready = graph
            .complete(planner, TaskExecution::expand(planned))
            .unwrap();
        assert!(ready.is_empty());
        assert_eq!(graph.node_state(planner), Some(NodeState::Waiting));

        graph.mark_running(volume).unwrap();
        let ready = graph.complete(volume, TaskExecution::succeeded()).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(graph.node_state(producer), Some(NodeState::Succeeded));
    }

    #[test]
    fn failed_bound_node_cancels_token_dependent_terminal_and_waiting_parent() {
        let mut builder = TaskGraphBuilder::new();
        let producer = builder.add_root(task("producer"));
        let planner = builder.add_root(task("planner"));
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();

        graph.mark_running(producer).unwrap();
        let token = TaskDependencyToken::new();
        let mut produced = GraphExpansion::new();
        produced.add_root_bound(task("volume"), token);
        let ready = graph
            .complete(producer, TaskExecution::expand(produced))
            .unwrap();
        let volume = ready[0].id;

        graph.mark_running(planner).unwrap();
        let mut planned = GraphExpansion::new();
        planned
            .add_root_with_tokens(task("commit"), [token])
            .unwrap();
        assert!(graph
            .complete(planner, TaskExecution::expand(planned))
            .unwrap()
            .is_empty());

        graph.mark_running(volume).unwrap();
        graph
            .complete(volume, TaskExecution::failed("bad package part"))
            .unwrap();

        assert_eq!(graph.node_state(volume), Some(NodeState::Failed));
        assert_eq!(
            graph.node_state(NodeId::from_index(3)),
            Some(NodeState::Cancelled)
        );
        assert_eq!(graph.node_state(producer), Some(NodeState::Failed));
        assert_eq!(graph.node_state(planner), Some(NodeState::Failed));
    }

    #[test]
    fn token_dependency_can_join_work_installed_by_a_waiting_parent() {
        let mut builder = TaskGraphBuilder::new();
        let parent = builder.add_root(task("parent"));
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(parent).unwrap();

        let token = TaskDependencyToken::new();
        let mut expansion = GraphExpansion::new();
        let volume = expansion.add_root_bound(task("volume"), token);
        expansion
            .add_root_with_tokens(task("index"), [token])
            .unwrap();
        let ready = graph
            .complete(parent, TaskExecution::expand(expansion))
            .unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id.index(), volume.0 + 1);

        let volume_id = ready[0].id;
        graph.mark_running(volume_id).unwrap();
        let ready = graph
            .complete(volume_id, TaskExecution::succeeded())
            .unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn token_dependency_cannot_wait_on_its_expanding_parent() {
        let mut builder = TaskGraphBuilder::new();
        let producer = builder.add_root(task("producer"));
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(producer).unwrap();

        let token = TaskDependencyToken::new();
        let mut produced = GraphExpansion::new();
        produced.add_root_bound(task("bound-parent"), token);
        let ready = graph
            .complete(producer, TaskExecution::expand(produced))
            .unwrap();
        let bound_parent = ready[0].id;
        graph.mark_running(bound_parent).unwrap();

        let mut recursive = GraphExpansion::new();
        recursive
            .add_root_with_tokens(task("cycle"), [token])
            .unwrap();
        let error = graph
            .complete(bound_parent, TaskExecution::expand(recursive))
            .unwrap_err();
        assert!(error.to_string().contains("creates a cycle"));
    }

    #[test]
    fn token_dependency_cannot_wait_on_a_dynamic_ancestor() {
        let mut builder = TaskGraphBuilder::new();
        let outer = builder.add_root(task("outer"));
        let mut graph = builder.build_checked().unwrap();
        let _ = graph.start();
        graph.mark_running(outer).unwrap();

        let ancestor_token = TaskDependencyToken::new();
        let mut outer_expansion = GraphExpansion::new();
        outer_expansion.add_root_bound(task("ancestor"), ancestor_token);
        let ready = graph
            .complete(outer, TaskExecution::expand(outer_expansion))
            .unwrap();
        let ancestor = ready[0].id;
        graph.mark_running(ancestor).unwrap();

        let ready = graph
            .complete(
                ancestor,
                TaskExecution::expand(GraphExpansion::single(task("inner-parent"))),
            )
            .unwrap();
        let inner_parent = ready[0].id;
        graph.mark_running(inner_parent).unwrap();

        let mut recursive = GraphExpansion::new();
        recursive
            .add_root_with_tokens(task("cycle"), [ancestor_token])
            .unwrap();
        let error = graph
            .complete(inner_parent, TaskExecution::expand(recursive))
            .unwrap_err();
        assert!(error.to_string().contains("creates a cycle"));
    }
}
