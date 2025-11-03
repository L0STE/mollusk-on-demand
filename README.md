# Mollusk-on-demand

A Rust crate that simplifies testing Solana programs with [Mollusk](https://github.com/anza-xyz/mollusk) by automatically fetching mainnet accounts and programs on-demand.

## Why?

Testing Solana programs with Mollusk typically requires manually fetching and setting up accounts from mainnet. This crate automates that process:

- **Fetch accounts directly from RPC** - Pull real mainnet accounts for your tests
- **Automatic program loading** - BPF Loader v2 and v3 programs are automatically added to Mollusk
- **Builder pattern API** - Clean, ergonomic interface for test setup
- **Efficient batching** - Uses `getMultipleAccounts` for fast, rate-limit-friendly fetching
- **Smart caching** - Avoids redundant RPC calls across multiple instructions

## Installation

Add this to your `Cargo.toml`:

```toml
[dev-dependencies]
mollusk-on-demand = "0.0.1"
mollusk-svm = "0.7.0"
```

## Quick Start

```rust
use mollusk_on_demand::RpcAccountStore;
use mollusk_svm::Mollusk;
use solana_sdk::instruction::Instruction;

#[tokio::test]
async fn test_with_mainnet_accounts() -> Result<(), Box<dyn std::error::Error>> {
    let mut mollusk = Mollusk::new(&program_id, "program_name");

    // Fetch accounts from an instruction and add programs to Mollusk
    RpcAccountStore::new("https://api.mainnet-beta.solana.com")
        .from_instruction(&instruction)
        .await?
        .add_programs(&mut mollusk)
        .await?;

    // Run your test with mollusk.process_instruction(...)
    Ok(())
}
```

## Features

### Fetch from Instructions

Automatically extracts all account pubkeys from instruction metadata:

```rust
// Single instruction
let store = RpcAccountStore::new(rpc_url)
    .from_instruction(&instruction)
    .await?;

// Multiple instructions (deduplicates pubkeys)
let store = RpcAccountStore::new(rpc_url)
    .from_instructions(&[ix1, ix2, ix3])
    .await?;
```

### Mock Accounts

Pre-populate the cache with test accounts before fetching:

```rust
let store = RpcAccountStore::new(rpc_url)
    .with_accounts(&[
        (user_pubkey, custom_user_account),
        (vault_pubkey, custom_vault_account),
    ])
    .from_instruction(&instruction)
    .await?;
```

### Error Handling

By default, missing accounts return an error. Use `allow_missing_accounts()` to create default accounts instead:

```rust
// Strict mode (default) - errors if accounts don't exist
let store = RpcAccountStore::new(rpc_url)
    .from_instruction(&instruction)
    .await?;  // Returns Err if any account is missing

// Permissive mode - creates empty accounts for missing pubkeys
let store = RpcAccountStore::new(rpc_url)
    .allow_missing_accounts()
    .from_instruction(&instruction)
    .await?;  // Never errors for missing accounts
```

### Program Validation

By default, ELF headers are validated before adding programs. Disable for performance:

```rust
let store = RpcAccountStore::new(rpc_url)
    .skip_program_validation()  // Skip ELF validation
    .from_instruction(&instruction)
    .await?
    .add_programs(&mut mollusk)
    .await?;
```

### Slot Synchronization

Sync Mollusk to mainnet's current slot (useful for oracles and slot-dependent programs):

```rust
RpcAccountStore::new(rpc_url)
    .from_instruction(&instruction)
    .await?
    .with_synced_slot(&mut mollusk)  // Warps Mollusk to current mainnet slot
    .await?;
```

### Custom Commitment

Specify RPC commitment level:

```rust
use solana_commitment_config::CommitmentConfig;

let store = RpcAccountStore::new_with_commitment(
    rpc_url,
    CommitmentConfig::finalized(),
);
```

### Direct Cache Access

Access the account cache directly for advanced use cases:

```rust
let store = RpcAccountStore::new(rpc_url)
    .from_instruction(&instruction)
    .await?;

// Direct access to fetched accounts
for (pubkey, account) in &store.cache {
    println!("Account {}: {} lamports", pubkey, account.lamports);
}
```

## How It Works

1. **Account Fetching**: Collects pubkeys from instructions and fetches them in batches using `getMultipleAccounts`
2. **Program Detection**: Identifies executable accounts with BPF Loader v2 or v3 as owner
3. **Program Data Extraction**:
   - Loader v2: ELF data is directly in the program account
   - Loader v3: Fetches the separate ProgramData account and extracts ELF from offset 45
4. **Validation**: Checks ELF magic numbers and basic header validity
5. **Mollusk Integration**: Adds programs using `add_program_with_elf_and_loader`

## Error Types

```rust
pub enum RpcError {
    Client(ClientError),                    // RPC request failed
    AccountNotFound(Pubkey),                // Account doesn't exist (when not allowing missing)
    InvalidProgramData { program, reason }, // Program data account is malformed
    MalformedProgram { program, reason },   // Program account structure is invalid
}
```

## Performance Notes

- **RPC Rate Limits**: Uses `getMultipleAccounts` to minimize RPC calls. Consider using a private RPC endpoint for heavy testing.
- **Caching**: Accounts are cached per `RpcAccountStore` instance. Reuse instances when testing multiple similar instructions.
- **Parallel Fetching**: Program data accounts are fetched in a single batch after initial account fetch.

## Alternatives

Without this crate, you would need to:

1. Manually fetch each account via RPC
2. Parse BPF Loader v3 program structures yourself
3. Extract ELF data from ProgramData accounts
4. Add each program to Mollusk individually
5. Handle all error cases and validation

This crate automates all of that into a single builder chain.

## License

MIT

## Contributing

Issues and PRs welcome! This crate is experimental and feedback is appreciated.

## Acknowledgments

Built on top of [Mollusk](https://github.com/buffalojoec/mollusk) by buffalojoec.
