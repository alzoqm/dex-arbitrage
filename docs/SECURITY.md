# Security and Secrets Management

## Overview

This document describes the security patterns and secret management practices for the dex-arbitrage bot.

## Secret Management

### Production Deployment

**Never keep production private keys in `.env` files on disk.** Use one of the following approaches:

#### 1. Remote Signer (Recommended)

Use a remote signing service such as:
- **AWS KMS**: Store keys in AWS Key Management Service
- **Google Cloud KMS**: Store keys in Google Cloud Key Management
- **HashiCorp Vault**: Use Vault's Transit engine for signing
- **Azure Key Vault**: Store keys in Azure Key Vault

Benefits:
- Keys never leave the HSM/key service
- Centralized key rotation
- Audit logging of all signing operations
- Multi-region key availability

#### 2. Environment Variables with Secret Management

If using environment variables, ensure they are:
- Injected from a secure secret manager at runtime
- Never committed to version control
- Rotated regularly
- Accessible only to the process that needs them

Example with Docker secrets:
```yaml
services:
  dex-arbitrage:
    secrets:
      - operator_key
      - deployer_key

secrets:
  operator_key:
    file: ./secrets/operator_key.txt
  deployer_key:
    file: ./secrets/deployer_key.txt
```

### Development

For development only, you may use `.env` files with the following safeguards:
1. Add `.env` to `.gitignore`
2. Use `.env.example` as a template
3. Never commit actual keys

## Key Rotation

### Rotation Procedure

1. Deploy new operator key to KMS/HSM
2. Update configuration to use new key
3. Gracefully restart the bot
4. Monitor for successful operation
7. Revoke old key access after 24 hours of successful operation

### Emergency Revocation

In case of key compromise:
1. Immediately revoke compromised key in KMS/HSM
2. Deploy new key
3. Restart bot
4. Audit all transactions made with compromised key

## Operator Key Permissions

The operator key should have the **minimum required permissions**:
- Execute transactions on the ArbitrageExecutor contract
- Transfer tokens for self-funded routes
- No additional admin rights

Use a Safe multisig for contract admin functions:
- Upgrade executor
- Update allowlist
- Pause/resume
- Emergency rescue

## Contract Security

### Deployment Checklist

- [ ] Verify contract bytecode on Etherscan/Polygonscan
- [ ] Transfer ownership to Safe multisig
- [ ] Set target allowlist for all pools
- [ ] Enable strict mode (no external calls)
- [ ] Configure emergency pause
- [ ] Test on testnet first
- [ ] Audit contract before mainnet

### Allowlist Management

- Maintain a strict allowlist of allowed pool addresses
- Use canonical pool addresses only
- Verify pool factory and code hash
- Regular audit of allowlist entries

### Slippage Protection

The executor contract enforces:
- Minimum profit thresholds
- Maximum slippage per hop
- Deadline enforcement
- Flash fee caps

## Network Security

### RPC Endpoints

- Use authenticated endpoints where available
- Implement rate limiting
- Use private mempools for submission
- Have fallback providers for redundancy

### WebSocket Security

- Use WSS (secure) connections
- Validate all incoming data
- Implement reconnection with backoff
- Limit event processing to prevent DoS

## Monitoring and Alerting

### Critical Alerts

- Transaction reverted
- Gas price above ceiling
- Stable token depeg detected
- Key compromise suspected
- Unexpected balance changes

### Metrics to Monitor

- PnL per block/hour/day
- Transaction success rate
- Gas cost as % of profit
- RPC error rate
- Latency metrics
- Cache hit rates

## Risk Limits

Configure these limits appropriately for your capital:

- Maximum position size
- Maximum flash loan amount
- Daily loss limits
- Gas price ceiling
- Minimum profit thresholds

## Incident Response

### Transaction Reverted

1. Check revert reason in logs
2. Verify pool state is accurate
3. Check for slippage attacks
4. Verify gas estimation
5. Adjust risk limits if needed

### RPC Failure

1. Check provider status
2. Switch to fallback RPC
3. Monitor queue depth
4. Reduce request rate if 429
5. Contact provider if persistent

### Suspected Compromise

1. Immediately stop the bot
2. Revoke operator key access
3. Rotate to new key
4. Audit recent transactions
5. Investigate access logs
6. Report incident

## Compliance

- Follow applicable securities laws in your jurisdiction
- Maintain transaction logs
- Implement AML/KYC procedures if required
- Report suspicious activity
- Regular security audits

## References

- [Alchemy Security Best Practices](https://www.alchemy.com/docs/security-best-practices)
- [OWASP Cryptographic Storage](https://owasp.org/www-project-proactive-controls/v3/en/latest/c7-cryptographic-storage)
- [Safe Multisig Documentation](https://docs.safe.global)
