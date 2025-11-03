//! Simplify Solana program testing with Mollusk by automatically fetching mainnet accounts.
//!
//! This crate provides utilities for fetching accounts from Solana RPC endpoints and
//! automatically loading programs into [Mollusk](https://github.com/buffalojoec/mollusk),
//! eliminating the manual setup typically required for integration testing.
//!
//! # Why use this crate?
//!
//! Testing Solana programs with real mainnet state usually requires:
//! - Manually fetching accounts via RPC
//! - Parsing BPF Loader program structures
//! - Extracting ELF data from ProgramData accounts
//! - Adding programs to Mollusk individually
//!
//! This crate automates all of that into a clean builder API.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use mollusk_on_demand::RpcAccountStore;
//! use mollusk_svm::Mollusk;
//!
//! #[tokio::test]
//! async fn test_with_mainnet_accounts() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut mollusk = Mollusk::new(&program_id, "program_name");
//!
//!     // Fetch accounts from instruction and auto-load programs
//!     RpcAccountStore::new("https://api.mainnet-beta.solana.com")
//!         .from_instruction(&instruction)
//!         .await?
//!         .add_programs(&mut mollusk)
//!         .await?;
//!
//!     // Run your test with mollusk.process_instruction(...)
//!     Ok(())
//! }
//! ```
//!
//! # Features
//!
//! - **Automatic program loading**: Handles both BPF Loader v2 and v3 programs
//! - **Efficient batching**: Uses `getMultipleAccounts` for fast RPC fetching
//! - **Smart caching**: Avoids redundant fetches across multiple instructions
//! - **Error handling**: Configurable behavior for missing accounts and validation
//! - **Slot synchronization**: Sync Mollusk to mainnet's current slot
//!
//! # Examples
//!
//! ## Fetch accounts from multiple instructions
//!
//! ```rust,ignore
//! let store = RpcAccountStore::new(rpc_url)
//!     .from_instructions(&[ix1, ix2, ix3])  // Deduplicates pubkeys
//!     .await?;
//! ```
//!
//! ## Mix real accounts with mocked accounts
//!
//! ```rust,ignore
//! RpcAccountStore::new(rpc_url)
//!     .with_accounts(&[(user_pubkey, mock_account)])  // Add mocks first
//!     .from_instruction(&instruction)
//!     .await?
//!     .add_programs(&mut mollusk)
//!     .await?;
//! ```
//!
//! ## Handle missing accounts gracefully
//!
//! ```rust,ignore
//! // Default: errors if accounts don't exist
//! let store = RpcAccountStore::new(rpc_url)
//!     .from_instruction(&instruction)
//!     .await?;
//!
//! // Permissive: creates empty accounts for missing pubkeys
//! let store = RpcAccountStore::new(rpc_url)
//!     .allow_missing_accounts()
//!     .from_instruction(&instruction)
//!     .await?;
//! ```
//!
//! ## Sync to mainnet slot
//!
//! ```rust,ignore
//! RpcAccountStore::new(rpc_url)
//!     .from_instruction(&instruction)
//!     .await?
//!     .with_synced_slot(&mut mollusk)  // Useful for oracles
//!     .await?;
//! ```
//!
//! # Error Handling
//!
//! The crate provides detailed errors for common failure cases:
//! - `RpcError::AccountNotFound`: Account doesn't exist (unless `allow_missing_accounts` is set)
//! - `RpcError::MalformedProgram`: Program account structure is invalid
//! - `RpcError::InvalidProgramData`: Program data account is missing or malformed
//! - `RpcError::Client`: RPC request failed
//!
//! # Performance Considerations
//!
//! - Uses `getMultipleAccounts` to minimize RPC calls
//! - Caches accounts to avoid redundant fetches
//! - Fetches program data accounts in batches
//! - Consider using a private RPC endpoint for heavy testing to avoid rate limits

use {
    mollusk_svm::Mollusk,
    solana_account::Account,
    solana_commitment_config::CommitmentConfig,
    solana_instruction::Instruction,
    solana_pubkey::Pubkey,
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_rpc_client_api::client_error::Error as ClientError,
    std::collections::{HashMap, HashSet},
    std::fmt,
    thiserror::Error,
};

/// Validates that the given data contains a valid ELF header.
///
/// This performs basic validation to ensure the data is likely a valid ELF binary.
fn validate_elf(data: &[u8]) -> Result<(), String> {
    // ELF magic number: 0x7F 'E' 'L' 'F'
    const ELF_MAGIC: &[u8] = &[0x7F, 0x45, 0x4C, 0x46];

    if data.len() < 52 {
        return Err(format!(
            "Data too small to be a valid ELF file: {} bytes (expected at least 52)",
            data.len()
        ));
    }

    if !data.starts_with(ELF_MAGIC) {
        return Err(format!(
            "Invalid ELF magic number: expected {:?}, got {:?}",
            ELF_MAGIC,
            &data[..4.min(data.len())]
        ));
    }

    // Check ELF class (32-bit or 64-bit)
    // 1 = 32-bit, 2 = 64-bit
    if data[4] != 1 && data[4] != 2 {
        return Err(format!("Invalid ELF class: {}", data[4]));
    }

    Ok(())
}

/// Error types for RPC operations.
#[derive(Debug, Error)]
pub enum RpcError {
    #[error("RPC client error: {0}")]
    Client(#[from] ClientError),

    #[error("Account not found: {0}")]
    AccountNotFound(Pubkey),

    #[error("Invalid program data account for program {program}: {reason}")]
    InvalidProgramData { program: Pubkey, reason: String },

    #[error("Malformed program account {program}: {reason}")]
    MalformedProgram { program: Pubkey, reason: String },
}

/// Utility for fetching accounts from Solana RPC endpoints.
///
/// Fetches accounts and stores them internally in a `HashMap<Pubkey, Account>`.
///
/// # Cache Access
///
/// The `cache` field is publicly accessible for advanced use cases where you need
/// direct access to fetched accounts (e.g., for use with MolluskContext or custom
/// account manipulation). For normal usage, prefer the builder methods.
pub struct RpcAccountStore {
    client: RpcClient,
    /// Publicly accessible cache of fetched accounts.
    ///
    /// Use this when you need direct access to accounts for custom operations.
    /// Most users should rely on the builder methods instead.
    pub cache: HashMap<Pubkey, Account>,
    /// If true, fetching non-existent accounts will create default (empty) accounts.
    /// If false, will return an error when accounts don't exist.
    allow_missing_accounts: bool,
    /// If true, validates program ELF headers before adding to Mollusk.
    validate_programs: bool,
}

impl fmt::Debug for RpcAccountStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcAccountStore")
            .field("accounts_cached", &self.cache.len())
            .field("allow_missing_accounts", &self.allow_missing_accounts)
            .field("validate_programs", &self.validate_programs)
            .finish_non_exhaustive()
    }
}

impl RpcAccountStore {
    /// Create a new account fetcher with the default commitment level (confirmed).
    ///
    /// By default:
    /// - Missing accounts will cause an error (use `allow_missing_accounts()` to change)
    /// - Program validation is enabled (use `skip_program_validation()` to disable)
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self::new_with_commitment(rpc_url, CommitmentConfig::confirmed())
    }

    /// Create a new account fetcher with a specific commitment level.
    ///
    /// By default:
    /// - Missing accounts will cause an error (use `allow_missing_accounts()` to change)
    /// - Program validation is enabled (use `skip_program_validation()` to disable)
    pub fn new_with_commitment(
        rpc_url: impl Into<String>,
        commitment: CommitmentConfig,
    ) -> Self {
        Self {
            client: RpcClient::new_with_commitment(rpc_url.into(), commitment),
            cache: HashMap::new(),
            allow_missing_accounts: false,
            validate_programs: true,
        }
    }

    /// Allow missing accounts to be treated as default (empty) accounts.
    ///
    /// By default, fetching non-existent accounts returns an error. Use this
    /// method when you want to test with accounts that may not exist on-chain.
    pub fn allow_missing_accounts(mut self) -> Self {
        self.allow_missing_accounts = true;
        self
    }

    /// Skip ELF validation when adding programs to Mollusk.
    ///
    /// By default, program ELF headers are validated before adding to Mollusk.
    /// Use this to disable validation if you're confident in your program data.
    pub fn skip_program_validation(mut self) -> Self {
        self.validate_programs = false;
        self
    }

    /// Fetch accounts required by an instruction.
    ///
    /// Extracts all account pubkeys from the instruction's account metas
    /// and fetches them from the RPC endpoint using getMultipleAccounts.
    pub async fn from_instruction(
        mut self,
        instruction: &Instruction,
    ) -> Result<Self, RpcError> {
        let pubkeys: Vec<_> = instruction.accounts.iter().map(|m| m.pubkey).collect();
        self.fetch_accounts(&pubkeys).await?;
        Ok(self)
    }

    /// Fetch accounts for multiple instructions.
    ///
    /// Collects all unique pubkeys across all instructions and fetches them
    /// efficiently in a batch using getMultipleAccounts.
    pub async fn from_instructions(
        mut self,
        instructions: &[Instruction],
    ) -> Result<Self, RpcError> {
        let pubkeys: HashSet<Pubkey> = instructions
            .iter()
            .flat_map(|ix| ix.accounts.iter().map(|m| m.pubkey))
            .collect();

        self.fetch_accounts(&pubkeys.into_iter().collect::<Vec<_>>())
            .await?;
        Ok(self)
    }

    /// Add accounts to the store.
    pub fn with_accounts(mut self, accounts: &[(Pubkey, Account)]) -> Self {
        for (pubkey, account) in accounts {
            self.cache.insert(*pubkey, account.clone());
        }
        self
    }

    /// Internal method to fetch accounts from RPC using `getMultipleAccounts`.
    ///
    /// Only fetches accounts that aren't already in the cache, allowing for
    /// efficient incremental fetching.
    async fn fetch_accounts(&mut self, pubkeys: &[Pubkey]) -> Result<(), RpcError> {
        // Filter out already cached accounts
        let missing_pubkeys: Vec<Pubkey> = pubkeys
            .iter()
            .filter(|pubkey| !self.cache.contains_key(pubkey))
            .copied()
            .collect();

        if missing_pubkeys.is_empty() {
            return Ok(());
        }

        let accounts = self.client.get_multiple_accounts(&missing_pubkeys).await?;

        // Store fetched accounts in cache
        for (pubkey, account_opt) in missing_pubkeys.iter().zip(accounts) {
            match account_opt {
                Some(account) => {
                    self.cache.insert(*pubkey, account);
                }
                None => {
                    if self.allow_missing_accounts {
                        // Create a default (empty) account for missing accounts
                        self.cache.insert(*pubkey, Account::default());
                    } else {
                        // Return an error if the account doesn't exist
                        return Err(RpcError::AccountNotFound(*pubkey));
                    }
                }
            }
        }

        Ok(())
    }

    /// Add programs to the Mollusk environment.
    ///
    /// This function fetches the program data accounts for all programs that are
    /// stored in the cache and adds them to the Mollusk environment.
    ///
    /// Note: This is needed because mollusk-svm doesn't load the programs for CPIs directly from the accounts.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Program account data is malformed
    /// - Program data account is invalid or missing
    /// - ELF validation fails (if enabled)
    pub async fn add_programs(mut self, mollusk: &mut Mollusk) -> Result<Self, RpcError> {
        // First pass: collect program data pubkeys that need to be fetched
        let mut program_data_pubkeys = Vec::new();
        for (pubkey, account) in self.cache.iter() {
            if account.executable && account.owner == mollusk_svm::program::loader_keys::LOADER_V3 {
                if account.data.len() < 36 {
                    return Err(RpcError::MalformedProgram {
                        program: *pubkey,
                        reason: format!(
                            "BPF Loader v3 program account too small: {} bytes (expected at least 36)",
                            account.data.len()
                        ),
                    });
                }

                let program_data_pubkey = Pubkey::try_from(&account.data[4..36]).map_err(|e| {
                    RpcError::MalformedProgram {
                        program: *pubkey,
                        reason: format!("Invalid program data pubkey: {}", e),
                    }
                })?;

                if !self.cache.contains_key(&program_data_pubkey) {
                    program_data_pubkeys.push(program_data_pubkey);
                }
            }
        }

        // Fetch all program data accounts at once
        if !program_data_pubkeys.is_empty() {
            self.fetch_accounts(&program_data_pubkeys).await?;
        }

        // Second pass: add programs to mollusk
        for (pubkey, account) in self.cache.iter() {
            if account.executable {
                // For BPF Loader v2 programs, the ELF is directly in the account data
                if account.owner == mollusk_svm::program::loader_keys::LOADER_V2 {
                    if self.validate_programs {
                        validate_elf(&account.data).map_err(|reason| {
                            RpcError::InvalidProgramData {
                                program: *pubkey,
                                reason,
                            }
                        })?;
                    }

                    mollusk.add_program_with_elf_and_loader(
                        pubkey,
                        &account.data,
                        &account.owner,
                    );
                }
                // For BPF Loader v3
                else if account.owner == mollusk_svm::program::loader_keys::LOADER_V3 {
                    if account.data.len() < 36 {
                        return Err(RpcError::MalformedProgram {
                            program: *pubkey,
                            reason: format!(
                                "BPF Loader v3 program account too small: {} bytes (expected at least 36)",
                                account.data.len()
                            ),
                        });
                    }

                    let program_data_pubkey = Pubkey::try_from(&account.data[4..36]).map_err(|e| {
                        RpcError::MalformedProgram {
                            program: *pubkey,
                            reason: format!("Invalid program data pubkey: {}", e),
                        }
                    })?;

                    let program_data_account = self.cache.get(&program_data_pubkey).ok_or_else(|| {
                        RpcError::InvalidProgramData {
                            program: *pubkey,
                            reason: format!("Program data account not found: {}", program_data_pubkey),
                        }
                    })?;

                    // The ELF starts at offset 45 in the program data account
                    // (first 45 bytes are the ProgramData header)
                    if program_data_account.data.len() <= 45 {
                        return Err(RpcError::InvalidProgramData {
                            program: *pubkey,
                            reason: format!(
                                "Program data account too small: {} bytes (expected > 45)",
                                program_data_account.data.len()
                            ),
                        });
                    }

                    let elf_data = &program_data_account.data[45..];

                    if self.validate_programs {
                        validate_elf(elf_data).map_err(|reason| {
                            RpcError::InvalidProgramData {
                                program: *pubkey,
                                reason,
                            }
                        })?;
                    }

                    mollusk.add_program_with_elf_and_loader(
                        pubkey,
                        elf_data,
                        &account.owner,
                    );
                }
            }
        }

        Ok(self)
    }


    /// Sync the Mollusk environment to the current mainnet slot.
    ///
    /// This function fetches the current slot from the RPC endpoint and updates
    /// the Mollusk instance to use that slot by calling `warp_to_slot`.
    ///
    /// Note: This is useful for oracles that need to be synced to the current mainnet slot.
    pub async fn with_synced_slot(self, mollusk: &mut Mollusk) -> Result<Self, RpcError> {
        let slot = self.client.get_slot().await?;
        mollusk.warp_to_slot(slot);
        Ok(self)
    }

    /// Deprecated: Use `with_synced_slot` instead.
    ///
    /// This method will be removed in a future version.
    #[deprecated(since = "0.0.1", note = "Use `with_synced_slot` instead")]
    pub async fn sync_slot(self, mollusk: &mut Mollusk) -> Result<Self, RpcError> {
        self.with_synced_slot(mollusk).await
    }
}