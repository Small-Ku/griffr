use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{GraphExpansion, NodeId, NodeState, TaskDependencyToken, TaskGraphBuilder, TaskRun};
use crate::runtime::task_pool::Task;

static TEST_DEPENDENCY_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

impl TaskDependencyToken {
    fn new() -> Self {
        Self(TEST_DEPENDENCY_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl GraphExpansion {
    fn single(task: Task) -> Self {
        let mut expansion = Self::new();
        expansion.add_root(task);
        expansion
    }

    fn add_root_bound(&mut self, task: Task, binding: TaskDependencyToken) -> usize {
        self.add_task_internal(task, std::iter::empty(), std::iter::empty(), Some(binding))
            .expect("bound root expansion task insertion cannot fail")
            .0
    }
}

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
    assert!(graph.finish(left, TaskRun::succeeded()).unwrap().is_empty());
    assert_eq!(graph.node_state(join), Some(NodeState::Pending));

    graph.mark_running(right).unwrap();
    let ready = graph.finish(right, TaskRun::succeeded()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, join);
}

#[test]
fn sequential_continuation_reuses_the_running_node() {
    let mut builder = TaskGraphBuilder::new();
    let root = builder.add_root(task("root"));
    let mut graph = builder.build_checked().unwrap();
    let ready = graph.start();
    assert_eq!(ready.len(), 1);

    graph.mark_running(root).unwrap();
    let ready = graph.finish(root, TaskRun::then(task("next"))).unwrap();

    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, root);
    assert!(ready[0].continuation);
    assert_eq!(graph.node_count(), 1);
    assert_eq!(graph.node_state(root), Some(NodeState::Ready));
    assert_eq!(graph.summary().dynamic_expansions, 0);
}

#[test]
fn dependent_waits_until_continued_node_finishes() {
    let mut builder = TaskGraphBuilder::new();
    let root = builder.add_root(task("root"));
    let dependent = builder.add_task(task("dependent"), [root]).unwrap();
    let mut graph = builder.build_checked().unwrap();
    let _ = graph.start();

    graph.mark_running(root).unwrap();
    let ready = graph.finish(root, TaskRun::then(task("next"))).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, root);
    assert_eq!(graph.node_state(dependent), Some(NodeState::Pending));

    graph.mark_running(root).unwrap();
    let ready = graph.finish(root, TaskRun::succeeded()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, dependent);
}

#[test]
fn fan_out_then_continuation_reuses_the_parent_node() {
    let mut builder = TaskGraphBuilder::new();
    let parent = builder.add_root(task("parent"));
    let dependent = builder.add_task(task("dependent"), [parent]).unwrap();
    let mut graph = builder.build_checked().unwrap();
    let _ = graph.start();
    graph.mark_running(parent).unwrap();

    let ready = graph
        .finish(
            parent,
            TaskRun::expand_then(
                GraphExpansion::parallel([task("left"), task("right")]),
                task("next"),
            ),
        )
        .unwrap();
    assert_eq!(ready.len(), 2);
    assert_eq!(graph.node_count(), 4);
    assert_eq!(graph.node_state(parent), Some(NodeState::Waiting));
    assert_eq!(graph.node_state(dependent), Some(NodeState::Pending));

    let left = ready[0].id;
    let right = ready[1].id;
    graph.mark_running(left).unwrap();
    assert!(graph.finish(left, TaskRun::succeeded()).unwrap().is_empty());
    assert_eq!(graph.node_state(parent), Some(NodeState::Waiting));

    graph.mark_running(right).unwrap();
    let ready = graph.finish(right, TaskRun::succeeded()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, parent);
    assert!(ready[0].continuation);
    assert_eq!(graph.node_state(parent), Some(NodeState::Ready));
    assert_eq!(graph.node_state(dependent), Some(NodeState::Pending));

    graph.mark_running(parent).unwrap();
    let ready = graph.finish(parent, TaskRun::succeeded()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, dependent);
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
        .finish(
            parent,
            TaskRun::expand(GraphExpansion::single(task("child"))),
        )
        .unwrap();
    assert_eq!(graph.node_state(parent), Some(NodeState::Waiting));
    assert_eq!(graph.node_state(dependent), Some(NodeState::Pending));
    assert_eq!(ready.len(), 1);

    let child = ready[0].id;
    graph.mark_running(child).unwrap();
    let ready = graph.finish(child, TaskRun::succeeded()).unwrap();
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
    graph.finish(failed_root, TaskRun::failed("boom")).unwrap();

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

    let ready = graph.finish(root, TaskRun::succeeded()).unwrap();
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
        .finish(
            parent,
            TaskRun::expand(GraphExpansion::single(task("child"))),
        )
        .unwrap();
    let child = ready[0].id;
    graph.mark_running(child).unwrap();
    graph.finish(child, TaskRun::cancelled()).unwrap();

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
    let ready = graph.finish(producer, TaskRun::expand(produced)).unwrap();
    let volume = ready[0].id;

    graph.mark_running(planner).unwrap();
    let mut planned = GraphExpansion::new();
    planned
        .add_root_with_tokens(task("index"), [token])
        .unwrap();
    let ready = graph.finish(planner, TaskRun::expand(planned)).unwrap();
    assert!(ready.is_empty());
    assert_eq!(graph.node_state(planner), Some(NodeState::Waiting));

    graph.mark_running(volume).unwrap();
    let ready = graph.finish(volume, TaskRun::succeeded()).unwrap();
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
    let ready = graph.finish(producer, TaskRun::expand(produced)).unwrap();
    let volume = ready[0].id;

    graph.mark_running(planner).unwrap();
    let mut planned = GraphExpansion::new();
    planned
        .add_root_with_tokens(task("commit"), [token])
        .unwrap();
    assert!(graph
        .finish(planner, TaskRun::expand(planned))
        .unwrap()
        .is_empty());

    graph.mark_running(volume).unwrap();
    graph
        .finish(volume, TaskRun::failed("bad package part"))
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
    let ready = graph.finish(parent, TaskRun::expand(expansion)).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id.index(), volume + 1);

    let volume_id = ready[0].id;
    graph.mark_running(volume_id).unwrap();
    let ready = graph.finish(volume_id, TaskRun::succeeded()).unwrap();
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
    let ready = graph.finish(producer, TaskRun::expand(produced)).unwrap();
    let bound_parent = ready[0].id;
    graph.mark_running(bound_parent).unwrap();

    let mut recursive = GraphExpansion::new();
    recursive
        .add_root_with_tokens(task("cycle"), [token])
        .unwrap();
    let error = graph
        .finish(bound_parent, TaskRun::expand(recursive))
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
        .finish(outer, TaskRun::expand(outer_expansion))
        .unwrap();
    let ancestor = ready[0].id;
    graph.mark_running(ancestor).unwrap();

    let ready = graph
        .finish(
            ancestor,
            TaskRun::expand(GraphExpansion::single(task("inner-parent"))),
        )
        .unwrap();
    let inner_parent = ready[0].id;
    graph.mark_running(inner_parent).unwrap();

    let mut recursive = GraphExpansion::new();
    recursive
        .add_root_with_tokens(task("cycle"), [ancestor_token])
        .unwrap();
    let error = graph
        .finish(inner_parent, TaskRun::expand(recursive))
        .unwrap_err();
    assert!(error.to_string().contains("creates a cycle"));
}
