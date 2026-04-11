pub mod path_finder;
pub mod pruning;

use std::sync::Arc;

use crate::{
    config::Settings,
    graph::{DistanceCache, GraphSnapshot},
    types::{CandidatePath, EdgeRef},
};

use self::path_finder::PathFinder;

#[derive(Debug)]
pub struct Detector {
    inner: PathFinder,
}

impl Detector {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            inner: PathFinder::new(settings),
        }
    }

    pub fn detect(
        &self,
        snapshot: &GraphSnapshot,
        changed_edges: &[EdgeRef],
        distance_cache: &DistanceCache,
    ) -> Vec<CandidatePath> {
        self.inner
            .find_candidates(snapshot, changed_edges, distance_cache)
    }
}
