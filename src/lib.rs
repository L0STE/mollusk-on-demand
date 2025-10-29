//! RPC utilities for fetching accounts from Solana RPC endpoints.
//!
//! This module provides a builder pattern for fetching accounts from Solana RPC
//! endpoints and returning them as a `HashMap<Pubkey, Account>`, which can be
//! directly used with `MolluskContext`.
//!
//! # Example
//!
//! ```rust,ignore
//! use mollusk_svm::rpc::RpcAccountFetcher;
//!
//! // Fetch and use use all accounts for an instruction
//! let accounts = RpcAccountFetcher::new("https://api.mainnet-beta.solana.com")
//!     .from_instruction(&instruction)
//!     .await?;
//! 
//! let context = mollusk.with_context(accounts);
//! let result = context.process_instruction(&instruction);
//! ```
//! 
//! If you want to mock some accounts, you can use the `with_accounts` method to add them 
//! to the store before fetching the accounts from the Endpoint.
//! 
//! ```rust,ignore
//! use mollusk_svm::rpc::RpcAccountFetcher;
//!
//! // Fetch and use use all accounts for an instruction
//! let accounts = RpcAccountFetcher::new("https://api.mainnet-beta.solana.com")
//!     .with_accounts(&[(pubkey1, account1), (pubkey2, account2)])
//!     .from_instruction(&instruction)
//!     .await?;
//! 
//! let context = mollusk.with_context(accounts);
//! let result = context.process_instruction(&instruction);
//! ```

use {
    solana_account::Account,
    solana_commitment_config::CommitmentConfig,
    solana_instruction::Instruction,
    solana_pubkey::Pubkey,
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_rpc_client_api::client_error::Error as ClientError,
    std::collections::{HashMap, HashSet},
    thiserror::Error,
};

/// Error types for RPC operations.
#[derive(Debug, Error)]
pub enum RpcError {
    #[error("RPC client error: {0}")]
    Client(#[from] ClientError),
}

/// Utility for fetching accounts from Solana RPC endpoints.
///
/// Fetches accounts and returns a `HashMap<Pubkey, Account>` that implements
/// `AccountStore` and can be used directly with `MolluskContext`.
pub struct RpcAccountStore {
    client: RpcClient,
    cache: HashMap<Pubkey, Account>,
}

impl RpcAccountStore {
    /// Create a new account fetcher with the default commitment level (confirmed).
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self::new_with_commitment(rpc_url, CommitmentConfig::confirmed())
    }

    /// Create a new account fetcher with a specific commitment level.
    pub fn new_with_commitment(
        rpc_url: impl Into<String>,
        commitment: CommitmentConfig,
    ) -> Self {
        Self {
            client: RpcClient::new_with_commitment(rpc_url.into(), commitment),
            cache: HashMap::new(),
        }
    }

    /// Fetch accounts required by an instruction.
    ///
    /// Extracts all account pubkeys from the instruction's account metas
    /// and fetches them from the RPC endpoint using getMultipleAccounts.
    pub async fn from_instruction(
        mut self,
        instruction: &Instruction,
    ) -> Result<HashMap<Pubkey, Account>, RpcError> {
        let pubkeys: Vec<_> = instruction.accounts.iter().map(|m| m.pubkey).collect();
        self.fetch_accounts(&pubkeys).await?;
        Ok(self.cache)
    }

    /// Fetch accounts for multiple instructions.
    ///
    /// Collects all unique pubkeys across all instructions and fetches them
    /// efficiently in a batch using getMultipleAccounts..
    pub async fn from_instructions(
        mut self,
        instructions: &[Instruction],
    ) -> Result<HashMap<Pubkey, Account>, RpcError> {
        let pubkeys: HashSet<Pubkey> = instructions
            .iter()
            .flat_map(|ix| ix.accounts.iter().map(|m| m.pubkey))
            .collect();

        self.fetch_accounts(&pubkeys.into_iter().collect::<Vec<_>>())
            .await?;
        Ok(self.cache)
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
        // Accounts that don't exist on-chain are stored as default (empty) accounts
        for (pubkey, account_opt) in missing_pubkeys.iter().zip(accounts) {
            let account = account_opt.unwrap_or_default();
            self.cache.insert(*pubkey, account);
        }

        Ok(())
    }
}