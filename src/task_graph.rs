//! Task graph walks shared by admission, reopen, decomposition and audit.
//!
//! A task finishes only after its dependencies and its unfinished children,
//! so those two edge kinds form the wait-for graph. A loop in it means none
//! of its tasks can ever finish.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, VecDeque, hash_map::Entry};

/// Why one task waits for the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitEdge {
    Dependency,
    Child,
}

/// One hop of a wait chain: the waiting task finishes only after `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaitStep {
    pub edge: WaitEdge,
    pub seq: i64,
}

/// Every dependency edge and every unfinished-child edge, by task number.
pub(crate) struct WaitGraph {
    edges: HashMap<i64, Vec<WaitStep>>,
}

impl WaitGraph {
    /// Every stored edge, as admission sees it.
    pub(crate) fn load(conn: &Connection) -> Result<Self> {
        Self::load_with(conn, "")
    }

    /// Only the dependency edges between unfinished tasks: a loop through a
    /// finished task already moved on, so only these loops are stuck.
    pub(crate) fn load_unfinished(conn: &Connection) -> Result<Self> {
        Self::load_with(
            conn,
            "WHERE task.status IN ('proposed','in_progress')
               AND prerequisite.status IN ('proposed','in_progress')",
        )
    }

    fn load_with(conn: &Connection, dependency_filter: &str) -> Result<Self> {
        let mut edges: HashMap<i64, Vec<WaitStep>> = HashMap::new();
        let dependencies = format!(
            "SELECT task.seq, prerequisite.seq
               FROM deps
               JOIN tasks task ON task.task_id=deps.task_id
               JOIN tasks prerequisite ON prerequisite.task_id=deps.depends_on
             {dependency_filter}
              ORDER BY task.seq, prerequisite.seq"
        );
        for (sql, edge) in [
            (dependencies.as_str(), WaitEdge::Dependency),
            (
                "SELECT parent.seq, child.seq
                   FROM tasks child
                   JOIN tasks parent ON parent.task_id=child.parent_id
                  WHERE child.status IN ('proposed','in_progress')
                  ORDER BY parent.seq, child.seq",
                WaitEdge::Child,
            ),
        ] {
            let mut statement = conn.prepare(sql)?;
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (from, seq) in rows {
                edges.entry(from).or_default().push(WaitStep { edge, seq });
            }
        }
        Ok(Self { edges })
    }

    /// The shortest chain by which `from` finishes only after `target`, or
    /// `None` when it does not wait for it. A task waits for itself through
    /// an empty chain.
    pub(crate) fn chain(&self, from: i64, target: i64) -> Option<Vec<WaitStep>> {
        let mut reached: HashMap<i64, Option<(i64, WaitEdge)>> = HashMap::from([(from, None)]);
        let mut queue = VecDeque::from([from]);
        while let Some(current) = queue.pop_front() {
            if current == target {
                let mut steps = Vec::new();
                let mut seq = current;
                while let Some(&Some((previous, edge))) = reached.get(&seq) {
                    steps.push(WaitStep { edge, seq });
                    seq = previous;
                }
                steps.reverse();
                return Some(steps);
            }
            for step in self.edges.get(&current).into_iter().flatten() {
                if let Entry::Vacant(entry) = reached.entry(step.seq) {
                    entry.insert(Some((current, step.edge)));
                    queue.push_back(step.seq);
                }
            }
        }
        None
    }

    /// Every stored wait loop, each closed at its first task. Each reported
    /// loop removes its closing edge before the search resumes, so every loop
    /// that needs its own repair is reported once.
    pub(crate) fn loops(&self) -> Vec<(i64, Vec<WaitStep>)> {
        let mut nodes = self
            .edges
            .iter()
            .flat_map(|(from, steps)| std::iter::once(*from).chain(steps.iter().map(|s| s.seq)))
            .collect::<Vec<_>>();
        nodes.sort_unstable();
        nodes.dedup();
        let index = nodes
            .iter()
            .enumerate()
            .map(|(position, seq)| (*seq, position))
            .collect::<HashMap<_, _>>();
        let mut adjacency = nodes
            .iter()
            .map(|seq| {
                let mut targets = self
                    .edges
                    .get(seq)
                    .into_iter()
                    .flatten()
                    .map(|step| index[&step.seq])
                    .collect::<Vec<_>>();
                targets.sort_unstable();
                targets.dedup();
                targets
            })
            .collect::<Vec<_>>();
        let mut loops = Vec::new();
        while let Some(cycle) = first_cycle(&adjacency) {
            let closing_from = cycle[cycle.len() - 2];
            let closing_to = cycle[cycle.len() - 1];
            adjacency[closing_from].retain(|target| *target != closing_to);
            let start = nodes[cycle[0]];
            let steps = cycle
                .windows(2)
                .map(|pair| {
                    let (from, seq) = (nodes[pair[0]], nodes[pair[1]]);
                    WaitStep {
                        edge: self.edge(from, seq),
                        seq,
                    }
                })
                .collect();
            loops.push((start, steps));
        }
        loops
    }

    /// A dependency is reported ahead of a child edge between the same
    /// tasks because only it can be removed without moving a task.
    fn edge(&self, from: i64, to: i64) -> WaitEdge {
        let steps = self.edges.get(&from).into_iter().flatten();
        if steps
            .clone()
            .any(|step| step.seq == to && step.edge == WaitEdge::Dependency)
        {
            WaitEdge::Dependency
        } else {
            WaitEdge::Child
        }
    }
}

/// Whether task `from` finishes only after task `target`, and through which
/// chain of dependencies and unfinished children.
pub(crate) fn waits_for(
    conn: &Connection,
    from: i64,
    target: i64,
) -> Result<Option<Vec<WaitStep>>> {
    Ok(WaitGraph::load(conn)?.chain(from, target))
}

/// "#a depends on #b, which waits for unfinished child #c".
pub(crate) fn describe_chain(from: i64, steps: &[WaitStep]) -> String {
    let mut text = format!("#{from}");
    for (position, step) in steps.iter().enumerate() {
        if position > 0 {
            text.push_str(", which");
        }
        match step.edge {
            WaitEdge::Dependency => text.push_str(&format!(" depends on #{}", step.seq)),
            WaitEdge::Child => text.push_str(&format!(" waits for unfinished child #{}", step.seq)),
        }
    }
    text
}

/// The `dep remove` commands that would break the chain, one per dependency
/// edge in it; empty when only parent-child edges make the task wait.
pub(crate) fn dependency_removals(from: i64, steps: &[WaitStep]) -> Vec<String> {
    let mut waiting = from;
    let mut removals = Vec::new();
    for step in steps {
        if step.edge == WaitEdge::Dependency {
            removals.push(format!(
                "`papertiger dep remove {waiting} {} --why <reason>`",
                step.seq
            ));
        }
        waiting = step.seq;
    }
    removals
}

/// Where a refusal's own correction ends: `", or first remove ..."` naming
/// each dependency that would break the chain, or nothing when only
/// parent-child edges make the task wait.
pub(crate) fn or_remove_dependency(from: i64, steps: &[WaitStep]) -> String {
    let removals = dependency_removals(from, steps);
    if removals.is_empty() {
        String::new()
    } else {
        format!(
            ", or first remove a dependency in that chain with {}",
            removals.join(" or ")
        )
    }
}

/// The first cycle found by an iterative depth-first search, as a closed path
/// of node indices (`[a, b, a]`). Iterative so that arbitrarily long chains
/// cannot exhaust the stack.
pub(crate) fn first_cycle(adjacency: &[Vec<usize>]) -> Option<Vec<usize>> {
    const UNVISITED: u8 = 0;
    const ON_PATH: u8 = 1;
    const FINISHED: u8 = 2;
    let mut state = vec![UNVISITED; adjacency.len()];
    for root in 0..adjacency.len() {
        if state[root] != UNVISITED {
            continue;
        }
        // Each frame is a node on the current path and its next edge position.
        let mut path: Vec<(usize, usize)> = vec![(root, 0)];
        state[root] = ON_PATH;
        while let Some(frame) = path.last_mut() {
            let (node, next) = *frame;
            let Some(&target) = adjacency[node].get(next) else {
                state[node] = FINISHED;
                path.pop();
                continue;
            };
            frame.1 += 1;
            match state[target] {
                UNVISITED => {
                    state[target] = ON_PATH;
                    path.push((target, 0));
                }
                ON_PATH => {
                    let start = path
                        .iter()
                        .position(|(on_path, _)| *on_path == target)
                        .expect("a node on the path has a frame");
                    let mut cycle = path[start..]
                        .iter()
                        .map(|(on_path, _)| *on_path)
                        .collect::<Vec<_>>();
                    cycle.push(target);
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(i64, WaitEdge, i64)]) -> WaitGraph {
        let mut map: HashMap<i64, Vec<WaitStep>> = HashMap::new();
        for &(from, edge, seq) in edges {
            map.entry(from).or_default().push(WaitStep { edge, seq });
        }
        WaitGraph { edges: map }
    }

    #[test]
    fn chains_name_each_hop_and_the_dependencies_that_break_them() {
        use WaitEdge::{Child, Dependency};
        let waits = graph(&[(3, Dependency, 1), (1, Child, 2), (2, Dependency, 4)]);
        let steps = waits.chain(3, 4).unwrap();
        assert_eq!(
            describe_chain(3, &steps),
            "#3 depends on #1, which waits for unfinished child #2, which depends on #4"
        );
        assert_eq!(
            dependency_removals(3, &steps),
            [
                "`papertiger dep remove 3 1 --why <reason>`",
                "`papertiger dep remove 2 4 --why <reason>`"
            ]
        );
        assert_eq!(waits.chain(4, 3), None);
        assert_eq!(waits.chain(3, 3), Some(Vec::new()));
    }

    #[test]
    fn cycles_are_closed_paths_and_long_chains_do_not_recurse() {
        assert_eq!(first_cycle(&[vec![1], vec![2], vec![]]), None);
        assert_eq!(
            first_cycle(&[vec![1], vec![2], vec![0]]),
            Some(vec![0, 1, 2, 0])
        );
        assert_eq!(first_cycle(&[vec![0]]), Some(vec![0, 0]));
        let long = 1_000_000;
        let mut chain = (1..long).map(|next| vec![next]).collect::<Vec<_>>();
        chain.push(Vec::new());
        assert_eq!(first_cycle(&chain), None);
        chain[long - 1].push(0);
        assert_eq!(first_cycle(&chain).map(|cycle| cycle.len()), Some(long + 1));
    }

    #[test]
    fn every_stored_loop_is_reported_once_preferring_dependency_edges() {
        use WaitEdge::{Child, Dependency};
        let waits = graph(&[
            (1, Child, 2),
            (2, Dependency, 3),
            (3, Dependency, 1),
            (5, Child, 6),
            (5, Dependency, 6),
            (6, Dependency, 5),
        ]);
        let loops = waits.loops();
        assert_eq!(loops.len(), 2);
        assert_eq!(
            describe_chain(loops[0].0, &loops[0].1),
            "#1 waits for unfinished child #2, which depends on #3, which depends on #1"
        );
        assert_eq!(
            describe_chain(loops[1].0, &loops[1].1),
            "#5 depends on #6, which depends on #5"
        );
        assert!(graph(&[(1, Dependency, 2)]).loops().is_empty());
    }
}
