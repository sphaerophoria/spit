use crate::git::{CommitMetadata, ObjectId, Repo};

use anyhow::{Context, Result};
use log::debug;

#[derive(Debug)]
pub struct GraphPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug)]
pub struct Edge {
    pub a: GraphPoint,
    pub b: GraphPoint,
}

impl Edge {
    fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Edge {
        Edge {
            a: GraphPoint { x: x1, y: y1 },
            b: GraphPoint { x: x2, y: y2 },
        }
    }
}

#[derive(Debug)]
pub struct CommitNode {
    pub position: GraphPoint,
    pub id: ObjectId,
}

pub struct HistoryGraph {
    pub nodes: Vec<CommitNode>,
    pub edges: Vec<Edge>,
}

#[derive(Debug)]
struct TailData {
    oid: ObjectId,
    edge_start_y: i32,
}

#[derive(Default)]
struct GraphBuilder {
    nodes: Vec<CommitNode>,
    edges: Vec<Edge>,
    tails: Vec<TailData>,
}

impl GraphBuilder {
    fn process_commit(&mut self, commit: &CommitMetadata) -> Result<()> {
        let commit_y_pos = self.nodes.len().try_into().context("Too many commits")?;
        let commit_tail_idx = ensure_commit_in_vec(commit, &mut self.tails, commit_y_pos);
        let parent_ids = &commit.parents;

        add_commit_to_node_list(commit_tail_idx, commit, &mut self.nodes)?;
        debug!("Tails before removal: {:?}", self.tails);
        let removed_data =
            replace_tail_with_parents(parent_ids, commit_tail_idx, commit_y_pos, &mut self.tails)?;
        // If we did not replace ourselves we need to adjust all lines
        debug!("Tails after removal: {:?}", self.tails);
        let initial_edges = self.edges.len();
        if let Some(removed_data) = removed_data {
            let mut removed_node_above_parent = false;
            // If we removed this tail we need to draw it's unfinished line
            if removed_data.edge_start_y != commit_y_pos {
                // If any of our parents end up under us we should just merge our undrawn line
                if self.tails.len() > commit_tail_idx
                    && parent_ids
                        .iter()
                        .any(|id| self.tails[commit_tail_idx].oid == *id)
                {
                    removed_node_above_parent = true;
                } else {
                    let x_pos = commit_tail_idx.try_into()?;
                    self.edges.push(Edge::new(
                        x_pos,
                        removed_data.edge_start_y,
                        x_pos,
                        commit_y_pos,
                    ));
                }
            }

            // And then we need to move all commits over by 1
            draw_removed_node_edges(
                commit_tail_idx,
                commit_y_pos,
                removed_node_above_parent,
                removed_data,
                &mut self.tails,
                &mut self.edges,
            )?;
        }
        draw_parent_connections(
            commit_tail_idx,
            commit_y_pos,
            parent_ids,
            &mut self.tails,
            &mut self.edges,
        )?;
        debug!("Added edges: {:?}", &self.edges[initial_edges..]);

        Ok(())
    }

    fn build(mut self) -> Result<HistoryGraph> {
        let end_y = self.nodes.len().try_into()?;
        finish_edges(&self.tails, end_y, &mut self.edges)?;
        Ok(HistoryGraph {
            nodes: self.nodes,
            edges: self.edges,
        })
    }
}

fn add_commit_to_node_list(
    x_idx: usize,
    commit: &CommitMetadata,
    node_list: &mut Vec<CommitNode>,
) -> Result<()> {
    let x = x_idx.try_into().context("Commit index too large")?;
    let y = node_list
        .last()
        .map(|node| node.position.y + 1)
        .unwrap_or(0);
    let position = GraphPoint { x, y };
    let id = commit.id.clone();
    node_list.push(CommitNode { position, id });
    Ok(())
}

fn ensure_commit_in_vec(commit: &CommitMetadata, vec: &mut Vec<TailData>, y_pos: i32) -> usize {
    let found_idx = vec
        .iter()
        .enumerate()
        .find(|(_, tail_data)| tail_data.oid == commit.id)
        .map(|(idx, _)| idx);
    if let Some(found_idx) = found_idx {
        found_idx
    } else {
        let tail_data = TailData {
            oid: commit.id.clone(),
            edge_start_y: y_pos,
        };
        vec.push(tail_data);
        vec.len() - 1
    }
}

fn replace_tail_with_parents(
    parent_ids: &[ObjectId],
    x_idx: usize,
    commit_y: i32,
    tails: &mut Vec<TailData>,
) -> Result<Option<TailData>> {
    let mut replaced_self = false;
    for parent_id in parent_ids {
        let parent_exists = tails.iter().any(|tail_data| tail_data.oid == *parent_id);
        if parent_exists {
            continue;
        }

        if replaced_self {
            let tail_data = TailData {
                oid: parent_id.clone(),
                edge_start_y: commit_y + 1,
            };
            tails.push(tail_data);
        } else {
            tails[x_idx].oid = parent_id.clone();
            replaced_self = true;
        }
    }

    if !replaced_self {
        Ok(Some(tails.remove(x_idx)))
    } else {
        Ok(None)
    }
}

fn draw_removed_node_edges(
    commit_x_idx: usize,
    commit_y_pos: i32,
    removed_node_above_parent: bool,
    removed_data: TailData,
    tails: &mut [TailData],
    edges: &mut Vec<Edge>,
) -> Result<()> {
    for (i, tail) in tails.iter_mut().enumerate().skip(commit_x_idx) {
        let x = i.try_into()?;
        edges.push(Edge::new(x + 1, tail.edge_start_y, x + 1, commit_y_pos));
        edges.push(Edge::new(x + 1, commit_y_pos, x, commit_y_pos + 1));
        // If we're merging into the x idx, and it's a parent we shouldn't heal the start id
        if removed_node_above_parent && i == commit_x_idx {
            tail.edge_start_y = removed_data.edge_start_y;
        } else {
            tail.edge_start_y = commit_y_pos + 1;
        }
    }

    Ok(())
}

fn draw_parent_connections(
    commit_x_idx: usize,
    commit_y_pos: i32,
    parent_ids: &[ObjectId],
    tails: &mut [TailData],
    edges: &mut Vec<Edge>,
) -> Result<()> {
    for (i, tail) in tails.iter().enumerate() {
        if i == commit_x_idx {
            continue;
        }

        if parent_ids.iter().any(|id| *id == tail.oid) {
            let x_pos = commit_x_idx.try_into()?;
            edges.push(Edge::new(
                x_pos,
                commit_y_pos,
                i.try_into()?,
                commit_y_pos + 1,
            ));
        }
    }

    Ok(())
}

fn finish_edges(tails: &[TailData], end_y: i32, edges: &mut Vec<Edge>) -> Result<()> {
    for (x_idx, tail_data) in tails.iter().enumerate() {
        let x_pos = x_idx.try_into()?;
        let edge = Edge {
            a: GraphPoint {
                x: x_pos,
                y: tail_data.edge_start_y,
            },
            b: GraphPoint { x: x_pos, y: end_y },
        };

        edges.push(edge)
    }

    Ok(())
}

pub(crate) fn build_git_history_graph(repo: &mut Repo) -> Result<HistoryGraph> {
    let mut graph_builder = GraphBuilder::default();
    let mut parents: Vec<ObjectId> = Vec::new();
    for branch in repo.branches()? {
        parents.push(branch?.head);
    }

    let revwalk = repo.metadata_iter(&parents)?;
    for metadata in revwalk {
        graph_builder
            .process_commit(metadata)
            .context("Failed to add commit to graph")?;
    }

    graph_builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::TempDir;

    fn find_edge(x1: i32, y1: i32, x2: i32, y2: i32, edges: &[Edge]) -> bool {
        edges
            .iter()
            .find(|edge| {
                (edge.a.x == x1 && edge.a.y == y1 && edge.b.x == x2 && edge.b.y == y2)
                    || (edge.a.x == x2 && edge.a.y == y2 && edge.b.x == x1 && edge.b.y == y1)
            })
            .is_some()
    }

    const STRAIGHT_TREE: &[u8] = include_bytes!("../../res/test/straight_tree.tar");
    #[test]
    fn straight_tree() -> Result<()> {
        let tmp_dir = TempDir::new()?;
        tar::Archive::new(STRAIGHT_TREE)
            .unpack(tmp_dir.path())
            .unwrap();

        let mut repo = Repo::new(tmp_dir.path())?;
        let graph = build_git_history_graph(&mut repo)?;
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[0].position.x, 0);
        assert_eq!(graph.nodes[1].position.x, 0);
        assert_eq!(graph.nodes[2].position.x, 0);
        assert_eq!(graph.nodes[0].position.y, 0);
        assert_eq!(graph.nodes[1].position.y, 1);
        assert_eq!(graph.nodes[2].position.y, 2);

        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].a.x, 0);
        assert_eq!(graph.edges[0].b.x, 0);
        assert_eq!(graph.edges[0].a.y, 0);
        assert_eq!(graph.edges[0].b.y, 2);

        Ok(())
    }

    const SINGLE_FORK: &[u8] = include_bytes!("../../res/test/single_fork.tar");
    #[test]
    fn single_fork() -> Result<()> {
        let tmp_dir = TempDir::new()?;
        tar::Archive::new(SINGLE_FORK)
            .unpack(tmp_dir.path())
            .unwrap();

        let mut repo = Repo::new(tmp_dir.path())?;
        let graph = build_git_history_graph(&mut repo)?;
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.nodes[0].position.x, 0);
        assert_eq!(graph.nodes[1].position.x, 1);
        assert_eq!(graph.nodes[2].position.x, 0);
        assert_eq!(graph.nodes[3].position.x, 0);
        assert_eq!(graph.nodes[0].position.y, 0);
        assert_eq!(graph.nodes[1].position.y, 1);
        assert_eq!(graph.nodes[2].position.y, 2);
        assert_eq!(graph.nodes[3].position.y, 3);

        assert_eq!(graph.edges.len(), 3);
        assert!(find_edge(0, 0, 0, 3, &graph.edges));
        assert!(find_edge(1, 1, 1, 2, &graph.edges));
        assert!(find_edge(1, 2, 0, 3, &graph.edges));
        Ok(())
    }

    const SIMPLE_MERGE: &[u8] = include_bytes!("../../res/test/simple_merge.tar");

    #[test]
    fn simple_merge() -> Result<()> {
        let tmp_dir = TempDir::new()?;
        tar::Archive::new(SIMPLE_MERGE)
            .unpack(tmp_dir.path())
            .unwrap();

        let mut repo = Repo::new(tmp_dir.path())?;
        let graph = build_git_history_graph(&mut repo)?;
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.nodes[0].position.x, 0);
        assert_eq!(graph.nodes[1].position.x, 0);
        assert_eq!(graph.nodes[2].position.x, 1);
        assert_eq!(graph.nodes[3].position.x, 0);
        assert_eq!(graph.nodes[0].position.y, 0);
        assert_eq!(graph.nodes[1].position.y, 1);
        assert_eq!(graph.nodes[2].position.y, 2);
        assert_eq!(graph.nodes[3].position.y, 3);

        assert_eq!(graph.edges.len(), 4);
        assert!(find_edge(0, 0, 0, 3, &graph.edges));
        assert!(find_edge(0, 0, 1, 1, &graph.edges));
        assert!(find_edge(1, 1, 1, 2, &graph.edges));
        assert!(find_edge(1, 2, 0, 3, &graph.edges));
        Ok(())
    }

    const UNSEEN_PARENT: &[u8] = include_bytes!("../../res/test/merge_from_unseen_parent.tar");

    #[test]
    fn merge_from_unseen_parent() -> Result<()> {
        let tmp_dir = TempDir::new()?;
        tar::Archive::new(UNSEEN_PARENT)
            .unpack(tmp_dir.path())
            .unwrap();

        let mut repo = Repo::new(tmp_dir.path())?;
        let graph = build_git_history_graph(&mut repo)?;
        assert_eq!(graph.nodes.len(), 6);
        assert_eq!(graph.nodes[0].position.x, 0);
        assert_eq!(graph.nodes[1].position.x, 1);
        assert_eq!(graph.nodes[2].position.x, 1);
        assert_eq!(graph.nodes[3].position.x, 0);
        assert_eq!(graph.nodes[4].position.x, 1);
        assert_eq!(graph.nodes[5].position.x, 0);

        assert_eq!(graph.nodes[0].position.y, 0);
        assert_eq!(graph.nodes[1].position.y, 1);
        assert_eq!(graph.nodes[2].position.y, 2);
        assert_eq!(graph.nodes[3].position.y, 3);
        assert_eq!(graph.nodes[3].position.y, 3);
        assert_eq!(graph.nodes[4].position.y, 4);
        assert_eq!(graph.nodes[5].position.y, 5);

        assert_eq!(graph.edges.len(), 9);
        assert!(find_edge(0, 0, 0, 3, &graph.edges));
        assert!(find_edge(1, 1, 2, 2, &graph.edges));
        assert!(find_edge(1, 1, 1, 3, &graph.edges));
        assert!(find_edge(2, 2, 2, 3, &graph.edges));
        assert!(find_edge(1, 3, 0, 4, &graph.edges));
        assert!(find_edge(2, 3, 1, 4, &graph.edges));
        assert!(find_edge(0, 3, 1, 4, &graph.edges));
        assert!(find_edge(0, 4, 0, 5, &graph.edges));
        assert!(find_edge(1, 4, 0, 5, &graph.edges));
        Ok(())
    }
}
