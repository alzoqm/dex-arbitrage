pub fn should_split(parallel_pool_count: usize, total_input: u128, min_slice: u128) -> bool {
    parallel_pool_count > 1 && total_input >= min_slice.saturating_mul(2)
}
