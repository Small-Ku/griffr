use std::collections::BTreeSet;

use super::super::super::types::Task;
use super::super::builder::TaskDependencyToken;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExpansionNodeId(pub(crate) usize);

#[derive(Debug)]
pub(super) struct ExpansionNode {
    pub(super) task: Task,
    pub(super) dependencies: Vec<ExpansionNodeId>,
    pub(super) token_dependencies: Vec<TaskDependencyToken>,
    pub(super) binding: Option<TaskDependencyToken>,
}

/// Append-only dynamic subgraph produced by one task run.
#[derive(Debug, Default)]
pub(crate) struct GraphExpansion {
    pub(super) nodes: Vec<ExpansionNode>,
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

    pub(crate) fn add_task_internal(
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
                return Err(Error::Message {
                    context: "Task pool error: ",
                    detail: format!(
                        "dynamic graph node {} depends on unknown or forward node {}",
                        id.0, dependency.0,
                    ),
                });
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
pub(crate) enum TaskRun {
    Succeeded,
    Failed { reason: String, report: bool },
    Cancelled,
    Expand(GraphExpansion),
}

impl TaskRun {
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
