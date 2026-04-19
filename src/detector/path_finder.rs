use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use smallvec::SmallVec;

use crate::{
    config::Settings,
    detector::pruning,
    graph::{DistanceCache, GraphSnapshot},
    risk::valuation::usd_e8_to_amount,
    types::{CandidateHop, CandidatePath, Edge, EdgeRef},
};

#[derive(Debug)]
pub struct PathFinder {
    max_hops: usize,
    screening_cutoff_q32: i64,
    min_pool_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_usd_e8: u128,
    tuning: SearchTuning,
}

#[derive(Debug, Clone, Copy)]
struct SearchTuning {
    top_k_paths_per_side: usize,
    max_virtual_branches_per_node: usize,
    path_beam_width: usize,
    max_candidates_per_refresh: usize,
    candidate_selection_buffer_multiplier: usize,
    dedup_token_paths: bool,
    max_pair_edges_per_pair: usize,
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
        let search = &settings.search;
        Self {
            max_hops: settings.risk.max_hops,
            screening_cutoff_q32: screening_cutoff_q32(settings.risk.screening_margin_bps),
            min_pool_confidence_bps: settings.risk.pool_health_min_bps,
            staleness_timeout: Duration::from_millis(settings.risk.staleness_timeout_ms),
            min_trade_usd_e8: settings.risk.min_trade_usd_e8,
            tuning: SearchTuning {
                top_k_paths_per_side: search.top_k_paths_per_side,
                max_virtual_branches_per_node: search.max_virtual_branches_per_node,
                path_beam_width: search.path_beam_width,
                max_candidates_per_refresh: search.max_candidates_per_refresh,
                candidate_selection_buffer_multiplier: search
                    .candidate_selection_buffer_multiplier
                    .max(1),
                dedup_token_paths: search.dedup_token_paths,
                max_pair_edges_per_pair: search.max_pair_edges_per_pair,
            },
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
        let mut token_path_candidates = HashMap::<String, CandidatePath>::new();
        let mut dedup = HashSet::<String>::new();
        let mut top_path_cache = HashMap::<(usize, usize, usize), Vec<SearchPath>>::new();
        let mut outgoing_cache = HashMap::<usize, Vec<EdgeRef>>::new();
        let mut pair_edges_cache = HashMap::<(usize, usize), Vec<EdgeRef>>::new();
        let min_trade_raw_by_token = min_trade_raw_by_token(snapshot, self.min_trade_usd_e8);
        let use_route_capacity_filter =
            changed_edges.len() <= env_usize("ROUTE_CAPACITY_PREFILTER_MAX_CHANGED_EDGES", 8_192);
        let selection_multiplier = if changed_edges.len()
            > env_usize("CANDIDATE_SELECTION_LARGE_REFRESH_THRESHOLD", 8_192)
        {
            env_usize("CANDIDATE_SELECTION_LARGE_REFRESH_MULTIPLIER", 1)
        } else {
            self.tuning.candidate_selection_buffer_multiplier
        };
        let selection_budget = candidate_selection_budget(
            self.tuning.max_candidates_per_refresh,
            selection_multiplier,
        );

        for ((from, to), changed_edge_ref) in changed_pairs {
            if !distance_cache.reachable_from_anchor[from]
                || !distance_cache.reachable_to_anchor[to]
            {
                continue;
            }
            let changed_candidate_edges = candidate_pair_edges_cached(
                snapshot,
                &mut pair_edges_cache,
                from,
                to,
                self.tuning.max_pair_edges_per_pair,
                self.min_pool_confidence_bps,
                self.staleness_timeout,
                &min_trade_raw_by_token,
            );
            if changed_candidate_edges.is_empty() {
                continue;
            }

            for &anchor_idx in &snapshot.cycle_anchor_indices {
                if !anchor_can_participate(snapshot, anchor_idx, from, to) {
                    continue;
                }

                let max_side_hops = self.max_hops.saturating_sub(1);
                let prefixes = self.find_top_paths_cached(
                    snapshot,
                    &mut top_path_cache,
                    &mut outgoing_cache,
                    anchor_idx,
                    from,
                    max_side_hops,
                    &min_trade_raw_by_token,
                );
                if prefixes.is_empty() {
                    continue;
                }
                let suffixes = self.find_top_paths_cached(
                    snapshot,
                    &mut top_path_cache,
                    &mut outgoing_cache,
                    to,
                    anchor_idx,
                    max_side_hops,
                    &min_trade_raw_by_token,
                );
                if suffixes.is_empty() {
                    continue;
                }

                for &changed_path_ref in &changed_candidate_edges {
                    for prefix in &prefixes {
                        for suffix in &suffixes {
                            let total_hops = prefix.edge_refs.len() + 1 + suffix.edge_refs.len();
                            if total_hops > self.max_hops || total_hops < 2 {
                                continue;
                            }
                            if !forms_simple_cycle(prefix, to, suffix) {
                                continue;
                            }

                            let Some(changed_edge) = snapshot.edge(changed_path_ref) else {
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
                            edge_refs.push(changed_path_ref);
                            edge_refs.extend_from_slice(&suffix.edge_refs);

                            let key = route_cycle_key(snapshot, prefix, changed_path_ref, suffix);
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
                                &min_trade_raw_by_token,
                                use_route_capacity_filter,
                            ) {
                                if self.tuning.dedup_token_paths {
                                    let token_key = candidate_token_cycle_key(&candidate);
                                    match token_path_candidates.get_mut(&token_key) {
                                        Some(existing)
                                            if candidate_precedes(&candidate, existing) =>
                                        {
                                            *existing = candidate;
                                        }
                                        Some(_) => {}
                                        None => {
                                            token_path_candidates.insert(token_key, candidate);
                                        }
                                    }
                                    if token_path_candidates.len() >= selection_budget {
                                        return finalize_candidates(
                                            token_path_candidates.into_values().collect(),
                                            self.tuning.max_candidates_per_refresh,
                                        );
                                    }
                                } else {
                                    candidates.push(candidate);
                                    if candidates.len() >= selection_budget {
                                        return finalize_candidates(
                                            candidates,
                                            self.tuning.max_candidates_per_refresh,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if self.tuning.dedup_token_paths {
            finalize_candidates(
                token_path_candidates.into_values().collect(),
                self.tuning.max_candidates_per_refresh,
            )
        } else {
            finalize_candidates(candidates, self.tuning.max_candidates_per_refresh)
        }
    }

    fn find_top_paths_cached(
        &self,
        snapshot: &GraphSnapshot,
        cache: &mut HashMap<(usize, usize, usize), Vec<SearchPath>>,
        outgoing_cache: &mut HashMap<usize, Vec<EdgeRef>>,
        start: usize,
        target: usize,
        max_hops: usize,
        min_trade_raw_by_token: &[Option<u128>],
    ) -> Vec<SearchPath> {
        let key = (start, target, max_hops);
        if let Some(paths) = cache.get(&key) {
            return paths.clone();
        }
        let paths = self.find_top_paths(
            snapshot,
            outgoing_cache,
            start,
            target,
            max_hops,
            min_trade_raw_by_token,
        );
        cache.insert(key, paths.clone());
        paths
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
        outgoing_cache: &mut HashMap<usize, Vec<EdgeRef>>,
        start: usize,
        target: usize,
        max_hops: usize,
        min_trade_raw_by_token: &[Option<u128>],
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
                for edge_ref in virtual_outgoing_cached(
                    snapshot,
                    outgoing_cache,
                    current_node,
                    self.tuning.max_pair_edges_per_pair,
                    self.min_pool_confidence_bps,
                    self.staleness_timeout,
                    min_trade_raw_by_token,
                )
                .into_iter()
                .take(self.tuning.max_virtual_branches_per_node)
                {
                    let Some(edge) = snapshot.edge(edge_ref) else {
                        continue;
                    };
                    if !edge_is_usable(
                        edge,
                        self.min_pool_confidence_bps,
                        self.staleness_timeout,
                        min_trade_raw_by_token,
                    ) {
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
            out.truncate(self.tuning.top_k_paths_per_side);
            sort_paths(&mut next_frontier);
            next_frontier.truncate(self.tuning.path_beam_width);
            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }

        sort_paths(&mut out);
        out.truncate(self.tuning.top_k_paths_per_side);
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
        min_trade_raw_by_token: &[Option<u128>],
        use_route_capacity_filter: bool,
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

        if use_route_capacity_filter
            && !route_has_min_start_capacity(
                snapshot,
                anchor_idx,
                edge_refs,
                min_trade_raw_by_token,
            )
        {
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

fn finalize_candidates(
    mut candidates: Vec<CandidatePath>,
    max_candidates_per_refresh: usize,
) -> Vec<CandidatePath> {
    candidates.sort_by(|a, b| a.screening_score_q32.cmp(&b.screening_score_q32));
    if candidates.len() <= max_candidates_per_refresh {
        return candidates;
    }

    let mut selected = Vec::with_capacity(max_candidates_per_refresh);
    let mut selected_canonical_cycles = HashSet::<String>::new();
    for candidate in &candidates {
        let key = canonical_token_cycle_key(candidate);
        if selected_canonical_cycles.insert(key) {
            selected.push(candidate.clone());
            if selected.len() >= max_candidates_per_refresh {
                return selected;
            }
        }
    }

    let mut selected_token_paths = selected
        .iter()
        .map(candidate_token_cycle_key)
        .collect::<HashSet<_>>();
    for candidate in candidates {
        if selected.len() >= max_candidates_per_refresh {
            break;
        }
        if selected_token_paths.insert(candidate_token_cycle_key(&candidate)) {
            selected.push(candidate);
        }
    }
    selected
}

fn candidate_token_cycle_key(candidate: &CandidatePath) -> String {
    let mut parts = Vec::with_capacity(candidate.path.len() + 1);
    parts.push(candidate.start_token.to_string());
    parts.extend(candidate.path.iter().map(|hop| hop.to.to_string()));
    parts.join(">")
}

fn canonical_token_cycle_key(candidate: &CandidatePath) -> String {
    let mut tokens = Vec::with_capacity(candidate.path.len());
    tokens.push(candidate.start_token);
    tokens.extend(
        candidate
            .path
            .iter()
            .take(candidate.path.len().saturating_sub(1))
            .map(|hop| hop.to),
    );
    if tokens.is_empty() {
        return candidate.start_token.to_string();
    }

    let best_start = (0..tokens.len())
        .min_by(|left, right| compare_cycle_rotation(&tokens, *left, *right))
        .unwrap_or(0);
    let mut parts = Vec::with_capacity(tokens.len() + 1);
    for offset in 0..tokens.len() {
        parts.push(tokens[(best_start + offset) % tokens.len()].to_string());
    }
    parts.push(tokens[best_start].to_string());
    parts.join(">")
}

fn compare_cycle_rotation(
    tokens: &[alloy::primitives::Address],
    left: usize,
    right: usize,
) -> std::cmp::Ordering {
    for offset in 0..tokens.len() {
        let lhs = tokens[(left + offset) % tokens.len()];
        let rhs = tokens[(right + offset) % tokens.len()];
        let cmp = lhs.cmp(&rhs);
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    std::cmp::Ordering::Equal
}

fn candidate_precedes(candidate: &CandidatePath, existing: &CandidatePath) -> bool {
    candidate
        .screening_score_q32
        .cmp(&existing.screening_score_q32)
        .then_with(|| candidate.path.len().cmp(&existing.path.len()))
        .is_lt()
}

fn candidate_selection_budget(max_candidates_per_refresh: usize, multiplier: usize) -> usize {
    max_candidates_per_refresh.saturating_mul(multiplier.max(1))
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;

    use alloy::primitives::Address;

    use crate::types::{AmmKind, CandidateHop, CandidatePath};

    use super::{candidate_selection_budget, canonical_token_cycle_key, q128_to_f64};

    fn addr(byte: u8) -> Address {
        Address::from_slice(&[byte; 20])
    }

    #[test]
    fn selection_budget_uses_at_least_one_multiplier() {
        assert_eq!(candidate_selection_budget(32, 0), 32);
        assert_eq!(candidate_selection_budget(32, 4), 128);
    }

    #[test]
    fn canonical_token_cycle_key_deduplicates_rotated_cycles() {
        let candidate = |start, path| CandidatePath {
            snapshot_id: 1,
            start_token: start,
            start_symbol: "A".to_string(),
            screening_score_q32: 0,
            cycle_key: "cycle".to_string(),
            path,
        };
        let hop = |from, to| CandidateHop {
            from,
            to,
            pool_id: from,
            amm_kind: AmmKind::UniswapV2Like,
            dex_name: "dex".to_string(),
        };

        let a = addr(1);
        let b = addr(2);
        let c = addr(3);
        let first = candidate(a, smallvec![hop(a, b), hop(b, c), hop(c, a)]);
        let rotated = candidate(b, smallvec![hop(b, c), hop(c, a), hop(a, b)]);

        assert_eq!(
            canonical_token_cycle_key(&first),
            canonical_token_cycle_key(&rotated)
        );
    }

    #[test]
    fn q128_to_f64_uses_limb_math_without_decimal_parse() {
        let one = alloy::primitives::U256::from(1u8) << 128;
        let one_and_half = one + (alloy::primitives::U256::from(1u8) << 127);
        assert!((q128_to_f64(one) - 1.0).abs() < f64::EPSILON);
        assert!((q128_to_f64(one_and_half) - 1.5).abs() < f64::EPSILON);
    }
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

fn virtual_outgoing(
    snapshot: &GraphSnapshot,
    vertex: usize,
    max_pair_edges_per_pair: usize,
    min_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_raw_by_token: &[Option<u128>],
) -> Vec<EdgeRef> {
    let mut by_to = HashMap::<usize, Vec<EdgeRef>>::new();
    for edge_ref in pruning::ranked_outgoing(snapshot, vertex) {
        let Some(edge) = snapshot.edge(edge_ref) else {
            continue;
        };
        if !edge_is_usable(
            edge,
            min_confidence_bps,
            staleness_timeout,
            min_trade_raw_by_token,
        ) {
            continue;
        }
        by_to.entry(edge.to).or_default().push(edge_ref);
    }

    let mut out = Vec::new();
    for mut edge_refs in by_to.into_values() {
        edge_refs.sort_by(|a, b| compare_edge_refs(snapshot, *a, *b));
        edge_refs.truncate(max_pair_edges_per_pair);
        out.extend(edge_refs);
    }
    out.sort_by(|a, b| compare_edge_refs(snapshot, *a, *b));
    out
}

fn virtual_outgoing_cached(
    snapshot: &GraphSnapshot,
    cache: &mut HashMap<usize, Vec<EdgeRef>>,
    vertex: usize,
    max_pair_edges_per_pair: usize,
    min_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_raw_by_token: &[Option<u128>],
) -> Vec<EdgeRef> {
    if let Some(edge_refs) = cache.get(&vertex) {
        return edge_refs.clone();
    }

    let edge_refs = virtual_outgoing(
        snapshot,
        vertex,
        max_pair_edges_per_pair,
        min_confidence_bps,
        staleness_timeout,
        min_trade_raw_by_token,
    );
    cache.insert(vertex, edge_refs.clone());
    edge_refs
}

fn candidate_pair_edges(
    snapshot: &GraphSnapshot,
    from: usize,
    to: usize,
    max_pair_edges_per_pair: usize,
    min_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_raw_by_token: &[Option<u128>],
) -> Vec<EdgeRef> {
    let Some(edges) = snapshot.pair_to_edges.get(&(from, to)) else {
        return Vec::new();
    };
    let mut out = edges
        .iter()
        .copied()
        .filter(|edge_ref| {
            snapshot
                .edge(*edge_ref)
                .map(|edge| {
                    edge_is_usable(
                        edge,
                        min_confidence_bps,
                        staleness_timeout,
                        min_trade_raw_by_token,
                    )
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| compare_edge_refs(snapshot, *a, *b));
    out.truncate(max_pair_edges_per_pair);
    out
}

fn candidate_pair_edges_cached(
    snapshot: &GraphSnapshot,
    cache: &mut HashMap<(usize, usize), Vec<EdgeRef>>,
    from: usize,
    to: usize,
    max_pair_edges_per_pair: usize,
    min_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_raw_by_token: &[Option<u128>],
) -> Vec<EdgeRef> {
    let key = (from, to);
    if let Some(edge_refs) = cache.get(&key) {
        return edge_refs.clone();
    }

    let edge_refs = candidate_pair_edges(
        snapshot,
        from,
        to,
        max_pair_edges_per_pair,
        min_confidence_bps,
        staleness_timeout,
        min_trade_raw_by_token,
    );
    cache.insert(key, edge_refs.clone());
    edge_refs
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

fn edge_is_usable(
    edge: &Edge,
    min_confidence_bps: u16,
    staleness_timeout: Duration,
    min_trade_raw_by_token: &[Option<u128>],
) -> bool {
    if edge.spot_rate_q128.is_zero() || edge.liquidity.safe_capacity_in == 0 {
        return false;
    }
    if !edge
        .pool_health
        .healthy(min_confidence_bps, staleness_timeout)
    {
        return false;
    }
    if let Some(min_trade_raw) = min_trade_raw_by_token
        .get(edge.from)
        .and_then(|value| *value)
    {
        if edge.liquidity.safe_capacity_in < min_trade_raw {
            return false;
        }
    }
    true
}

fn route_has_min_start_capacity(
    snapshot: &GraphSnapshot,
    anchor_idx: usize,
    edge_refs: &[EdgeRef],
    min_trade_raw_by_token: &[Option<u128>],
) -> bool {
    let Some(min_trade_raw) = min_trade_raw_by_token
        .get(anchor_idx)
        .and_then(|value| *value)
    else {
        return true;
    };
    route_start_capacity_raw(snapshot, edge_refs)
        .map(|capacity| capacity >= min_trade_raw.max(1))
        .unwrap_or(false)
}

fn route_start_capacity_raw(snapshot: &GraphSnapshot, edge_refs: &[EdgeRef]) -> Option<u128> {
    if edge_refs.is_empty() {
        return None;
    }

    let mut cumulative_rate = 1.0f64;
    let mut max_start = f64::INFINITY;
    for &edge_ref in edge_refs {
        let edge = snapshot.edge(edge_ref)?;
        if edge.liquidity.safe_capacity_in == 0 || cumulative_rate <= 0.0 {
            return Some(0);
        }
        max_start = max_start.min(edge.liquidity.safe_capacity_in as f64 / cumulative_rate);
        let rate = q128_to_f64(edge.spot_rate_q128);
        if !rate.is_finite() || rate <= 0.0 {
            return Some(0);
        }
        cumulative_rate *= rate;
        if !cumulative_rate.is_finite() {
            return Some(0);
        }
    }

    Some(f64_to_u128(max_start))
}

fn min_trade_raw_by_token(snapshot: &GraphSnapshot, min_trade_usd_e8: u128) -> Vec<Option<u128>> {
    snapshot
        .tokens
        .iter()
        .map(|token| usd_e8_to_amount(min_trade_usd_e8, token))
        .collect()
}

fn q128_to_f64(value: alloy::primitives::U256) -> f64 {
    let limbs = value.as_limbs();
    limbs[0] as f64 * 2f64.powi(-128)
        + limbs[1] as f64 * 2f64.powi(-64)
        + limbs[2] as f64
        + limbs[3] as f64 * 2f64.powi(64)
}

fn f64_to_u128(value: f64) -> u128 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= u128::MAX as f64 {
        u128::MAX
    } else {
        value as u128
    }
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

fn route_cycle_key(
    snapshot: &GraphSnapshot,
    prefix: &SearchPath,
    changed_edge_ref: EdgeRef,
    suffix: &SearchPath,
) -> String {
    let mut edge_refs = prefix.edge_refs.clone();
    edge_refs.push(changed_edge_ref);
    edge_refs.extend_from_slice(&suffix.edge_refs);

    edge_refs
        .into_iter()
        .filter_map(|edge_ref| {
            let edge = snapshot.edge(edge_ref)?;
            let from = snapshot.tokens.get(edge.from)?.address;
            let to = snapshot.tokens.get(edge.to)?.address;
            Some(format!("{}>{}:{}", from, to, edge.pool_id))
        })
        .collect::<Vec<_>>()
        .join("|")
}
