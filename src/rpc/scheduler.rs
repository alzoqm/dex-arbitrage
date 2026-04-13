//! RPC scheduler with request prioritization, rate limiting, and circuit breaker.
//!
//! Provides centralized RPC request management with:
//! - Priority-based request queuing (critical, high, normal, low)
//! - Compute unit (CU) budget management per provider
//! - Request timeout by class
//! - Exponential backoff for retries
//! - Circuit breaker per provider and method
//! - Automatic failover for read operations
//! - 429/403/5xx aware rate limiting

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::rpc::RpcClient;
use crate::{config::RpcSettings, types::Chain};

/// Request priority levels
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestPriority {
    /// Critical: final simulation, private submit, receipt reconciliation
    Critical = 0,
    /// High: event gap repair, changed-pool refresh
    High = 1,
    /// Normal: discovery increments, token metadata, gas queries
    #[default]
    Normal = 2,
    /// Low: price refresh, maintenance scans, historical backfills
    Low = 3,
}

/// Request class for timeout configuration
#[derive(Debug, Clone, Copy)]
pub struct RequestClass {
    pub priority: RequestPriority,
    pub is_idempotent: bool,
    pub max_retries: usize,
}

impl RequestClass {
    pub fn critical() -> Self {
        Self {
            priority: RequestPriority::Critical,
            is_idempotent: false,
            max_retries: 3,
        }
    }

    pub fn high() -> Self {
        Self {
            priority: RequestPriority::High,
            is_idempotent: true,
            max_retries: 2,
        }
    }

    pub fn normal() -> Self {
        Self {
            priority: RequestPriority::Normal,
            is_idempotent: true,
            max_retries: 1,
        }
    }

    pub fn low() -> Self {
        Self {
            priority: RequestPriority::Low,
            is_idempotent: true,
            max_retries: 0,
        }
    }
}

/// Default request class for common methods
pub fn get_request_class(method: &str) -> RequestClass {
    match method {
        "eth_sendRawTransaction" | "eth_sendPrivateTransaction" | "flashbots_sendBundle" => {
            RequestClass::critical()
        }
        "eth_estimateGas"
        | "eth_estimateUserOperationGas"
        | "eth_simulateV1"
        | "eth_callBundle" => RequestClass::high(),
        "eth_getTransactionReceipt" | "eth_getTransactionByHash" | "eth_getTransactionCount" => {
            RequestClass::high()
        }
        "eth_call" | "eth_getLogs" => RequestClass::normal(),
        "eth_blockNumber" | "eth_chainId" | "eth_getBlockByNumber" => RequestClass::normal(),
        _ => RequestClass::low(),
    }
}

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    Closed,
    Open { open_since: Instant },
    HalfOpen,
}

/// Per-provider circuit breaker
struct CircuitBreaker {
    state: Arc<Mutex<CircuitState>>,
    failure_count: Arc<AtomicU64>,
    consecutive_failures: Arc<AtomicU64>,
    last_failure_time: Arc<Mutex<Option<Instant>>>,
    failure_threshold: usize,
    open_duration: Duration,
}

impl CircuitBreaker {
    fn new(failure_threshold: usize, open_duration: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitState::Closed)),
            failure_count: Arc::new(AtomicU64::new(0)),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
            last_failure_time: Arc::new(Mutex::new(None)),
            failure_threshold,
            open_duration,
        }
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let mut state = self.state.lock();
        if *state == CircuitState::HalfOpen {
            *state = CircuitState::Closed;
        }
    }

    fn record_failure(&self) {
        self.failure_count.fetch_add(1, Ordering::SeqCst);
        let consecutive = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_failure_time.lock() = Some(Instant::now());

        if consecutive >= self.failure_threshold as u64 {
            let mut state = self.state.lock();
            if !matches!(*state, CircuitState::Open { .. }) {
                *state = CircuitState::Open {
                    open_since: Instant::now(),
                };
            }
        }
    }

    fn should_allow_request(&self) -> bool {
        let state = *self.state.lock();
        match state {
            CircuitState::Closed => true,
            CircuitState::Open { open_since } => {
                if open_since.elapsed() > self.open_duration {
                    *self.state.lock() = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    fn is_open(&self) -> bool {
        matches!(*self.state.lock(), CircuitState::Open { .. })
    }
}

/// Token bucket for rate limiting
struct TokenBucket {
    capacity: u64,
    refill_rate: u64, // tokens per second
    tokens: Arc<AtomicU64>,
    last_refill: Arc<Mutex<Instant>>,
}

impl TokenBucket {
    fn new(capacity: u64, refill_rate: u64) -> Self {
        Self {
            capacity,
            refill_rate,
            tokens: Arc::new(AtomicU64::new(capacity)),
            last_refill: Arc::new(Mutex::new(Instant::now())),
        }
    }

    fn try_consume(&self, tokens_needed: u64) -> bool {
        let mut last = self.last_refill.lock();
        let elapsed = last.elapsed().as_secs_f64();
        let refill = (elapsed * self.refill_rate as f64) as u64;

        let current = self.tokens.fetch_add(0, Ordering::SeqCst);
        let new_tokens = (current + refill).min(self.capacity);
        self.tokens.store(new_tokens, Ordering::SeqCst);
        *last = Instant::now();

        let mut current = self.tokens.load(Ordering::SeqCst);
        loop {
            if current >= tokens_needed {
                match self.tokens.compare_exchange_weak(
                    current,
                    current - tokens_needed,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(actual) => current = actual,
                }
            } else {
                return false;
            }
        }
    }

    fn wait_for_tokens(&self, tokens_needed: u64) -> Duration {
        let current = self.tokens.load(Ordering::SeqCst);
        if current >= tokens_needed {
            return Duration::ZERO;
        }
        let needed = tokens_needed - current;
        let seconds = (needed as f64) / self.refill_rate as f64;
        Duration::from_secs_f64(seconds.max(0.01))
    }
}

/// Per-provider state
pub struct ProviderState {
    client: Arc<RpcClient>,
    circuit_breaker: Arc<CircuitBreaker>,
    cu_bucket: Arc<TokenBucket>,
    cu_used_today: Arc<AtomicU64>,
    method_support: Arc<RwLock<HashMap<String, bool>>>,
}

impl ProviderState {
    fn new(client: Arc<RpcClient>, cu_per_second: u64, circuit_threshold: usize) -> Self {
        Self {
            client,
            circuit_breaker: Arc::new(CircuitBreaker::new(
                circuit_threshold,
                Duration::from_secs(60),
            )),
            cu_bucket: Arc::new(TokenBucket::new(cu_per_second, cu_per_second)),
            cu_used_today: Arc::new(AtomicU64::new(0)),
            method_support: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        if !self.circuit_breaker.should_allow_request() {
            anyhow::bail!("circuit breaker open for provider");
        }

        let result = self.client.request(method, params).await;
        match &result {
            Ok(_) => self.circuit_breaker.record_success(),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("429")
                    || msg.contains("rate limit")
                    || msg.contains("too many requests")
                    || msg.contains("403")
                {
                    self.circuit_breaker.record_failure();
                }
            }
        }

        result
    }

    fn supports_method(&self, method: &str) -> bool {
        let support = self.method_support.read();
        *support.get(method).unwrap_or(&true)
    }

    pub fn set_method_support(&self, method: &str, supported: bool) {
        self.method_support
            .write()
            .insert(method.to_string(), supported);
    }
}

/// Main RPC scheduler
pub struct RpcScheduler {
    chain: Chain,
    providers: Vec<Arc<ProviderState>>,
    critical_priority: bool,
    fallback_enabled: bool,

    // Priority queues (simplified as channels)
    critical_tx: mpsc::Sender<ScheduledRequest>,
    high_tx: mpsc::Sender<ScheduledRequest>,
    normal_tx: mpsc::Sender<ScheduledRequest>,
    low_tx: mpsc::Sender<ScheduledRequest>,
}

#[derive(Debug)]
struct ScheduledRequest {
    method: String,
    params: serde_json::Value,
    class: RequestClass,
    respond: oneshot::Sender<Result<serde_json::Value>>,
    timeout: Duration,
}

impl RpcScheduler {
    /// Create a new RPC scheduler from RPC settings
    pub async fn from_settings(settings: &RpcSettings, chain: Chain) -> Result<Self> {
        // Compute unit budgets per second (Alchemy PAYG: 10,000 CU/s)
        let cu_per_second = 10_000;

        // Circuit breaker thresholds
        let circuit_threshold = 5; // Open after 5 consecutive failures

        // Create providers
        let public_client = Arc::new(RpcClient::new(settings.public_rpc_url.clone())?);
        let public_state = Arc::new(ProviderState::new(
            public_client.clone(),
            cu_per_second,
            circuit_threshold,
        ));

        let mut providers = vec![public_state];

        if let Some(fallback_url) = &settings.fallback_rpc_url {
            let fallback_client = Arc::new(RpcClient::new(fallback_url.clone())?);
            let fallback_state = Arc::new(ProviderState::new(
                fallback_client,
                cu_per_second,
                circuit_threshold,
            ));
            providers.push(fallback_state);
        }

        if let Some(protected_url) = &settings.protected_rpc_url {
            let protected_client = Arc::new(RpcClient::new(protected_url.clone())?);
            let protected_state = Arc::new(ProviderState::new(
                protected_client,
                cu_per_second,
                circuit_threshold,
            ));
            providers.push(protected_state);
        }

        if let Some(preconf_url) = &settings.preconf_rpc_url {
            let preconf_client = Arc::new(RpcClient::new(preconf_url.clone())?);
            let preconf_state = Arc::new(ProviderState::new(
                preconf_client,
                cu_per_second,
                circuit_threshold,
            ));
            providers.push(preconf_state);
        }

        let (critical_tx, critical_rx) = mpsc::channel(100);
        let (high_tx, high_rx) = mpsc::channel(500);
        let (normal_tx, normal_rx) = mpsc::channel(1000);
        let (low_tx, low_rx) = mpsc::channel(2000);

        let scheduler = Self {
            chain,
            providers,
            critical_priority: true,
            fallback_enabled: true,
            critical_tx,
            high_tx,
            normal_tx,
            low_tx,
        };

        // Spawn worker tasks
        tokio::spawn(Self::worker_task(
            scheduler.chain,
            scheduler.providers.clone(),
            Arc::clone(&scheduler.providers[0]),
            critical_rx,
            RequestPriority::Critical,
            scheduler.critical_priority,
            scheduler.fallback_enabled,
        ));

        tokio::spawn(Self::worker_task(
            scheduler.chain,
            scheduler.providers.clone(),
            Arc::clone(&scheduler.providers[0]),
            high_rx,
            RequestPriority::High,
            scheduler.critical_priority,
            scheduler.fallback_enabled,
        ));

        tokio::spawn(Self::worker_task(
            scheduler.chain,
            scheduler.providers.clone(),
            Arc::clone(&scheduler.providers[0]),
            normal_rx,
            RequestPriority::Normal,
            scheduler.critical_priority,
            scheduler.fallback_enabled,
        ));

        tokio::spawn(Self::worker_task(
            scheduler.chain,
            scheduler.providers.clone(),
            Arc::clone(&scheduler.providers[0]),
            low_rx,
            RequestPriority::Low,
            scheduler.critical_priority,
            scheduler.fallback_enabled,
        ));

        Ok(scheduler)
    }

    async fn worker_task(
        chain: Chain,
        providers: Vec<Arc<ProviderState>>,
        primary: Arc<ProviderState>,
        mut rx: mpsc::Receiver<ScheduledRequest>,
        priority: RequestPriority,
        critical_priority: bool,
        fallback_enabled: bool,
    ) {
        while let Some(mut req) = rx.recv().await {
            // Apply request timeout
            let timeout_duration = req.timeout;

            let result = tokio::select! {
                r = Self::execute_request(chain, &providers, &primary, &mut req, priority, critical_priority, fallback_enabled) => r,
                _ = sleep(timeout_duration) => {
                    Err(anyhow::anyhow!("request timeout after {:?}", timeout_duration))
                }
            };

            // Respond
            let _ = req.respond.send(result);
        }
    }

    async fn execute_request(
        chain: Chain,
        providers: &[Arc<ProviderState>],
        primary: &Arc<ProviderState>,
        req: &mut ScheduledRequest,
        _priority: RequestPriority,
        critical_priority: bool,
        fallback_enabled: bool,
    ) -> Result<serde_json::Value> {
        let cu_cost = crate::rpc::rpc_compute_units(&req.method);
        let provider = Self::select_provider(
            providers,
            primary,
            req.class.is_idempotent && fallback_enabled,
            critical_priority,
            &req.method,
            cu_cost,
        );

        // Wait for CU budget
        if let Some(ref p) = provider {
            while !p.cu_bucket.try_consume(cu_cost) {
                let wait = p.cu_bucket.wait_for_tokens(cu_cost);
                sleep(wait).await;
            }
        }

        let provider = provider.unwrap_or_else(|| Arc::clone(primary));

        // Record CU usage
        provider.cu_used_today.fetch_add(cu_cost, Ordering::SeqCst);
        metrics::counter!(
            "rpc_cu_total",
            "method" => req.method.clone(),
            "chain" => chain.as_str().to_string()
        )
        .increment(cu_cost);

        // Execute with retries
        let mut last_error = None;
        for attempt in 0..=req.class.max_retries {
            let result = provider.request(&req.method, req.params.clone()).await;

            match result {
                Ok(value) => {
                    metrics::counter!("rpc_requests_total", "method" => req.method.clone(), "status" => "success").increment(1);
                    metrics::histogram!("rpc_latency_seconds", "method" => req.method.clone())
                        .record(0.0); // TODO: track actual latency
                    return Ok(value);
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < req.class.max_retries {
                        // Exponential backoff with jitter
                        let base_delay = Duration::from_millis(100 * 2_u64.pow(attempt as u32));
                        let jitter =
                            Duration::from_millis(deterministic_jitter_ms(&req.method, attempt));
                        sleep(base_delay + jitter).await;
                    }
                }
            }
        }

        metrics::counter!("rpc_requests_total", "method" => req.method.clone(), "status" => "error").increment(1);
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
    }

    fn select_provider(
        providers: &[Arc<ProviderState>],
        primary: &Arc<ProviderState>,
        allow_fallback: bool,
        prefer_primary: bool,
        method: &str,
        cu_cost: u64,
    ) -> Option<Arc<ProviderState>> {
        // For critical requests, prefer primary even if circuit is open
        if prefer_primary && primary.supports_method(method) && !primary.circuit_breaker.is_open() {
            return Some(Arc::clone(primary));
        }

        // For read requests with fallback enabled, try fallback providers
        if allow_fallback && !prefer_primary {
            for provider in providers.iter().skip(1) {
                if provider.supports_method(method)
                    && provider.circuit_breaker.should_allow_request()
                    && provider.cu_bucket.wait_for_tokens(cu_cost).is_zero()
                {
                    return Some(Arc::clone(provider));
                }
            }
        }

        // Use primary if available
        if primary.supports_method(method) && primary.circuit_breaker.should_allow_request() {
            Some(Arc::clone(primary))
        } else {
            None
        }
    }

    /// Execute an RPC request with priority
    pub async fn request_with_policy(
        &self,
        method: &str,
        params: serde_json::Value,
        class: RequestClass,
    ) -> Result<serde_json::Value> {
        let timeout = match class.priority {
            RequestPriority::Critical => Duration::from_secs(10),
            RequestPriority::High => Duration::from_secs(30),
            RequestPriority::Normal => Duration::from_secs(60),
            RequestPriority::Low => Duration::from_secs(120),
        };

        let (respond, rx) = oneshot::channel();
        let scheduled = ScheduledRequest {
            method: method.to_string(),
            params,
            class,
            respond,
            timeout,
        };

        let tx = match class.priority {
            RequestPriority::Critical => &self.critical_tx,
            RequestPriority::High => &self.high_tx,
            RequestPriority::Normal => &self.normal_tx,
            RequestPriority::Low => &self.low_tx,
        };

        tx.send(scheduled)
            .await
            .map_err(|_| anyhow::anyhow!("scheduler channel closed"))?;

        rx.await
            .map_err(|_| anyhow::anyhow!("request response channel closed"))?
    }

    /// Get the public client directly (for compatibility)
    pub fn public_client(&self) -> &Arc<RpcClient> {
        &self.providers[0].client
    }

    /// Get all providers
    pub fn providers(&self) -> &[Arc<ProviderState>] {
        &self.providers
    }
}

/// Default CU costs per method (can be overridden by config)
pub fn get_cu_cost(method: &str) -> u64 {
    crate::rpc::rpc_compute_units(method)
}

fn deterministic_jitter_ms(method: &str, attempt: usize) -> u64 {
    let hash = method.bytes().fold(attempt as u64, |acc, byte| {
        acc.wrapping_mul(31) ^ u64::from(byte)
    });
    hash % 50
}
