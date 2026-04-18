use dex_arbitrage::{
    rpc::{adapt_log_chunk_size, max_log_range_for_chain, rpc_compute_units},
    types::Chain,
};

#[test]
fn alchemy_payg_log_ranges_are_chain_capped() {
    assert_eq!(max_log_range_for_chain(Chain::Polygon), 2_000);
    assert_eq!(adapt_log_chunk_size(Chain::Polygon, "payg", 50_000), 2_000);
    assert_eq!(max_log_range_for_chain(Chain::Base), 10_000);
    assert_eq!(adapt_log_chunk_size(Chain::Base, "payg", 100_000), 10_000);
}

#[test]
fn rpc_compute_units_cover_hot_methods() {
    assert_eq!(rpc_compute_units("eth_getLogs"), 60);
    assert_eq!(rpc_compute_units("eth_call"), 26);
    assert_eq!(rpc_compute_units("eth_sendRawTransaction"), 40);
    assert_eq!(rpc_compute_units("eth_blockNumber"), 10);
}
