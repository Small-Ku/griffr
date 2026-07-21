use std::collections::{BTreeSet, HashMap};

use super::super::types::Task;
use crate::error::{Error, Result};

/// Stable handle used by dynamically discovered work to depend on a node that
/// was installed by an earlier graph expansion. Tokens are command-local in
/// practice, but globally unique so accidental cross-graph reuse cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskDependencyToken(pub(super) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub(super) fn new(index: usize) -> Result<Self> {
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
    pub(super) fn is_terminal(self) -> bool {
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
            .collect::<BTreeSet<_>>()
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

    pub fn build(self) -> super::exec::TaskGraph {
        self.build_checked()
            .expect("validated task graph builder failed to build")
    }

    pub fn build_checked(self) -> Result<super::exec::TaskGraph> {
        let mut graph_nodes = self
            .nodes
            .iter()
            .map(|node| super::exec::TaskNode {
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
        Ok(super::exec::TaskGraph::from_raw(
            graph_nodes,
            unresolved,
            0,
            HashMap::new(),
        ))
    }
}
