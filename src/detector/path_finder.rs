use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use smallvec::SmallVec;

use crate::{
    config::Settings,
    detector::pruning,
    graph::{DistanceCache, GraphSnapshot},
    types::{CandidateHop, CandidatePath, EdgeRef},
};

const TOP_K_PATHS_PER_SIDE: usize = 8;
const MAX_VIRTUAL_BRANCHES_PER_NODE: usize = 16;
const PATH_BEAM_WIDTH: usize = 96;
const MAX_CANDIDATES_PER_REFRESH: usize = 512;

#[derive(Debug)]
pub struct PathFinder {
    max_hops: usize,
    screening_cutoff_q32: i64,
}

#[derive(Debug, Clone)]
struct SearchPath {
    edge_refs: SmallVec<[EdgeRef; 8]>,
    tokens: SmallVec<[usize; 8]>,
    score_q32: i64,
    capacity_hint: u128,
}

impl PathFinder {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            max_hops: settings.risk.max_hops,
            screening_cutoff_q32: screening_cutoff_q32(settings.risk.screening_margin_bps),
        }
    }

    pub fn find_candidates(
        &self,
        snapshot: &GraphSnapshot,
        changed_edges: &[EdgeRef],
        distance_cache: &DistanceCache,
    ) -> Vec<CandidatePath> {
        if self.max_hops < 2 || changed_edges.is_empty() {
            return Vec::new();
        }

        let changed_pairs = self.changed_virtual_pairs(snapshot, changed_edges);
        if changed_pairs.is_empty() {
            return Vec::new();
        }

        let mut candidates = Vec::new();
        let mut dedup = HashSet::<String>::new();

        for ((from, to), changed_edge_ref) in changed_pairs {
            if !distance_cache.reachable_from_anchor[from]
                || !distance_cache.reachable_to_anchor[to]
            {
                continue;
            }
            let Some(changed_best_ref) = best_virtual_edge(snapshot, from, to) else {
                continue;
            };

            for &anchor_idx in &snapshot.cycle_anchor_indices {
                if !anchor_can_participate(snapshot, anchor_idx, from, to) {
                    continue;
                }

                let max_side_hops = self.max_hops.saturating_sub(1);
                let prefixes = self.find_top_paths(snapshot, anchor_idx, from, max_side_hops);
                if prefixes.is_empty() {
                    continue;
                }
                let suffixes = self.find_top_paths(snapshot, to, anchor_idx, max_side_hops);
                if suffixes.is_empty() {
                    continue;
                }

                for prefix in &prefixes {
                    for suffix in &suffixes {
                        let total_hops = prefix.edge_refs.len() + 1 + suffix.edge_refs.len();
                        if total_hops > self.max_hops || total_hops < 2 {
                            continue;
                        }
                        if !forms_simple_cycle(prefix, to, suffix) {
                            continue;
                        }

                        let Some(changed_edge) = snapshot.edge(changed_best_ref) else {
                            continue;
                        };
                        let score_q32 = prefix
                            .score_q32
                            .saturating_add(changed_edge.weight_log_q32)
                            .saturating_add(suffix.score_q32);
                        if score_q32 >= self.screening_cutoff_q32 {
                            continue;
                        }

                        let mut edge_refs = prefix.edge_refs.clone();
                        edge_refs.push(changed_best_ref);
                        edge_refs.extend_from_slice(&suffix.edge_refs);

                        let key = token_cycle_key(snapshot, prefix, to, suffix);
                        if !dedup.insert(key.clone()) {
                            continue;
                        }

                        if let Some(candidate) = self.materialize_candidate(
                            snapshot,
                            anchor_idx,
                            key,
                            score_q32,
                            &edge_refs,
                            changed_edge_ref,
                        ) {
                            candidates.push(candidate);
                            if candidates.len() >= MAX_CANDIDATES_PER_REFRESH {
                                return finalize_candidates(candidates);
                            }
                        }
                    }
                }
            }
        }

        finalize_candidates(candidates)
    }

    fn changed_virtual_pairs(
        &self,
        snapshot: &GraphSnapshot,
        changed_edges: &[EdgeRef],
    ) -> Vec<((usize, usize), EdgeRef)> {
        let mut by_pair = HashMap::<(usize, usize), EdgeRef>::new();
        for &edge_ref in changed_edges {
            let Some(edge) = snapshot.edge(edge_ref) else {
                continue;
            };
            by_pair
                .entry((edge.from, edge.to))
                .and_modify(|current| {
                    if better_edge_ref(snapshot, edge_ref, *current) {
                        *current = edge_ref;
                    }
                })
                .or_insert(edge_ref);
        }

        let mut out = by_pair.into_iter().collect::<Vec<_>>();
        out.sort_by(|((af, at), a_ref), ((bf, bt), b_ref)| {
            let a_score = snapshot
                .edge(*a_ref)
                .map(|edge| edge.weight_log_q32)
                .unwrap_or(i64::MAX);
            let b_score = snapshot
                .edge(*b_ref)
                .map(|edge| edge.weight_log_q32)
                .unwrap_or(i64::MAX);
            a_score
                .cmp(&b_score)
                .then_with(|| af.cmp(bf))
                .then_with(|| at.cmp(bt))
        });
        out
    }

    fn find_top_paths(
        &self,
        snapshot: &GraphSnapshot,
        start: usize,
        target: usize,
        max_hops: usize,
    ) -> Vec<SearchPath> {
        if start == target {
            return vec![SearchPath {
                edge_refs: SmallVec::new(),
                tokens: SmallVec::from_slice(&[start]),
                score_q32: 0,
                capacity_hint: u128::MAX,
            }];
        }

        let initial = SearchPath {
            edge_refs: SmallVec::new(),
            tokens: SmallVec::from_slice(&[start]),
            score_q32: 0,
            capacity_hint: u128::MAX,
        };

        let mut frontier = vec![initial];
        let mut out = Vec::new();
        for _ in 0..max_hops {
            let mut next_frontier = Vec::new();
            for current in &frontier {
                let current_node = *current.tokens.last().expect("path always has a node");
                for edge_ref in virtual_outgoing(snapshot, current_node)
                    .into_iter()
                    .take(MAX_VIRTUAL_BRANCHES_PER_NODE)
                {
                    let Some(edge) = snapshot.edge(edge_ref) else {
                        continue;
                    };
                    if edge.pool_health.paused
                        || edge.pool_health.quarantined
                        || edge.pool_health.stale
                    {
                        continue;
                    }
                    if current.tokens.contains(&edge.to) {
                        continue;
                    }

                    let mut next = current.clone();
                    next.edge_refs.push(edge_ref);
                    next.tokens.push(edge.to);
                    next.score_q32 = next.score_q32.saturating_add(edge.weight_log_q32);
                    if edge.liquidity.safe_capacity_in > 0 {
                        next.capacity_hint =
                            next.capacity_hint.min(edge.liquidity.safe_capacity_in);
                    }

                    if edge.to == target {
                        out.push(next);
                    } else {
                        next_frontier.push(next);
                    }
                }
            }

            sort_paths(&mut out);
            out.truncate(TOP_K_PATHS_PER_SIDE);
            sort_paths(&mut next_frontier);
            next_frontier.truncate(PATH_BEAM_WIDTH);
            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }

        sort_paths(&mut out);
        out.truncate(TOP_K_PATHS_PER_SIDE);
        out
    }

    fn materialize_candidate(
        &self,
        snapshot: &GraphSnapshot,
        anchor_idx: usize,
        key: String,
        score_q32: i64,
        edge_refs: &[EdgeRef],
        changed_edge_ref: EdgeRef,
    ) -> Option<CandidatePath> {
        let mut path = SmallVec::<[CandidateHop; 8]>::new();
        let mut contains_changed_pair = false;
        let changed_edge = snapshot.edge(changed_edge_ref)?;

        for &edge_ref in edge_refs {
            let edge = snapshot.edge(edge_ref)?;
            if edge.from == changed_edge.from && edge.to == changed_edge.to {
                contains_changed_pair = true;
            }
            path.push(CandidateHop {
                from: snapshot.tokens[edge.from].address,
                to: snapshot.tokens[edge.to].address,
                pool_id: edge.pool_id,
                amm_kind: edge.amm_kind,
                dex_name: edge.dex_name.clone(),
            });
        }

        if !contains_changed_pair {
            return None;
        }

        Some(CandidatePath {
            snapshot_id: snapshot.snapshot_id,
            start_token: snapshot.tokens[anchor_idx].address,
            start_symbol: snapshot.tokens[anchor_idx].symbol.clone(),
            screening_score_q32: score_q32,
            cycle_key: key,
            path,
        })
    }
}

fn finalize_candidates(mut candidates: Vec<CandidatePath>) -> Vec<CandidatePath> {
    candidates.sort_by(|a, b| a.screening_score_q32.cmp(&b.screening_score_q32));
    candidates.truncate(MAX_CANDIDATES_PER_REFRESH);
    candidates
}

fn sort_paths(paths: &mut [SearchPath]) {
    paths.sort_by(|a, b| {
        a.score_q32
            .cmp(&b.score_q32)
            .then_with(|| b.capacity_hint.cmp(&a.capacity_hint))
            .then_with(|| a.edge_refs.len().cmp(&b.edge_refs.len()))
    });
}

fn screening_cutoff_q32(screening_margin_bps: u32) -> i64 {
    let margin = screening_margin_bps as f64 / 10_000.0;
    (-(1.0 + margin).ln() * ((1u64 << 32) as f64)) as i64
}

fn anchor_can_participate(
    snapshot: &GraphSnapshot,
    anchor_idx: usize,
    changed_from: usize,
    changed_to: usize,
) -> bool {
    anchor_idx == changed_from
        || anchor_idx == changed_to
        || snapshot
            .adjacency
            .get(anchor_idx)
            .map(|edges| !edges.is_empty())
            .unwrap_or(false)
}

fn virtual_outgoing(snapshot: &GraphSnapshot, vertex: usize) -> Vec<EdgeRef> {
    let mut by_to = HashMap::<usize, EdgeRef>::new();
    for edge_ref in pruning::ranked_outgoing(snapshot, vertex) {
        let Some(edge) = snapshot.edge(edge_ref) else {
            continue;
        };
        by_to.entry(edge.to).or_insert(edge_ref);
    }

    let mut out = by_to.into_values().collect::<Vec<_>>();
    out.sort_by(|a, b| compare_edge_refs(snapshot, *a, *b));
    out
}

fn best_virtual_edge(snapshot: &GraphSnapshot, from: usize, to: usize) -> Option<EdgeRef> {
    snapshot
        .pair_to_edges
        .get(&(from, to))?
        .iter()
        .copied()
        .min_by(|a, b| compare_edge_refs(snapshot, *a, *b))
}

fn better_edge_ref(snapshot: &GraphSnapshot, left: EdgeRef, right: EdgeRef) -> bool {
    compare_edge_refs(snapshot, left, right).is_lt()
}

fn compare_edge_refs(
    snapshot: &GraphSnapshot,
    left: EdgeRef,
    right: EdgeRef,
) -> std::cmp::Ordering {
    let Some(a) = snapshot.edge(left) else {
        return std::cmp::Ordering::Greater;
    };
    let Some(b) = snapshot.edge(right) else {
        return std::cmp::Ordering::Less;
    };

    a.weight_log_q32
        .cmp(&b.weight_log_q32)
        .then_with(|| {
            b.liquidity
                .safe_capacity_in
                .cmp(&a.liquidity.safe_capacity_in)
        })
        .then_with(|| {
            b.pool_health
                .confidence_bps
                .cmp(&a.pool_health.confidence_bps)
        })
        .then_with(|| a.pool_id.cmp(&b.pool_id))
}

fn forms_simple_cycle(prefix: &SearchPath, changed_to: usize, suffix: &SearchPath) -> bool {
    let mut tokens = prefix.tokens.clone();
    tokens.push(changed_to);
    tokens.extend(suffix.tokens.iter().copied().skip(1));

    if tokens.len() < 3 || tokens.first() != tokens.last() {
        return false;
    }

    let mut seen = HashSet::new();
    for token in tokens.iter().take(tokens.len() - 1) {
        if !seen.insert(*token) {
            return false;
        }
    }
    true
}

fn token_cycle_key(
    snapshot: &GraphSnapshot,
    prefix: &SearchPath,
    changed_to: usize,
    suffix: &SearchPath,
) -> String {
    let mut tokens = prefix.tokens.clone();
    tokens.push(changed_to);
    tokens.extend(suffix.tokens.iter().copied().skip(1));
    tokens
        .into_iter()
        .map(|idx| snapshot.tokens[idx].address.to_string())
        .collect::<Vec<_>>()
        .join(">")
}
