use std::collections::{BTreeSet, HashMap};

use super::super::super::types::Task;
use super::super::builder::{
    NodeId, NodeState, TaskDependencyToken, TaskGraphBuilder, TaskGraphSummary,
};
use super::expansion::{GraphExpansion, TaskExecution};
use crate::error::{Error, Result};

#[derive(Debug)]
pub(crate) struct TaskNode {
    pub(crate) task: Option<Task>,
    pub(crate) state: NodeState,
    pub(crate) remaining_dependencies: usize,
    pub(crate) dependents: Vec<NodeId>,
    pub(crate) waiters: Vec<NodeId>,
    pub(crate) waiting_remaining: usize,
    pub(crate) continuation: bool,
}

/// A command-scoped, append-only task DAG.
#[derive(Debug)]
pub struct TaskGraph {
    nodes: Vec<TaskNode>,
    unresolved: usize,
    dynamic_expansions: usize,
    bindings: HashMap<TaskDependencyToken, NodeId>,
}

impl TaskGraph {
    pub(crate) fn from_raw(
        nodes: Vec<TaskNode>,
        unresolved: usize,
        dynamic_expansions: usize,
        bindings: HashMap<TaskDependencyToken, NodeId>,
    ) -> Self {
        Self {
            nodes,
            unresolved,
            dynamic_expansions,
            bindings,
        }
    }

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
