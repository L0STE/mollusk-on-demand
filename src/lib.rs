 //! RPC utilities for fetching accounts from Solana RPC endpoints.
//!
//! This module provides a builder pattern for fetching accounts from Solana RPC
//! endpoints and returning them as a `HashMap<Pubkey, Account>`, which can be
//! directly used with `MolluskContext`.
//!
//! # Example
//!
//! ```rust,ignore
//! use mollusk_svm::rpc::RpcAccountStore;
//!
//! // Fetch and use use all accounts for an instruction
//! let accounts = RpcAccountStore::new("https://api.mainnet-beta.solana.com")
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
//! use mollusk_svm::rpc::RpcAccountStore;
//!
//! // Fetch and use use all accounts for an instruction
//! let accounts = RpcAccountStore::new("https://api.mainnet-beta.solana.com")
//!     .with_accounts(&[(pubkey1, account1), (pubkey2, account2)])
//!     .from_instruction(&instruction)
//!     .await?;
//! 
//! let context = mollusk.with_context(accounts);
//! let result = context.process_instruction(&instruction);
//! ```

use {
    serde::{Deserialize, Serialize},
    solana_account::Account,
    solana_commitment_config::CommitmentConfig,
    solana_instruction::Instruction,
    solana_pubkey::Pubkey,
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_rpc_client_api::client_error::Error as ClientError,
    std::{
        collections::{HashMap, HashSet},
        fs,
        path::Path,
    },
    thiserror::Error,
};

/// Error types for RPC operations.
#[derive(Debug, Error)]
pub enum RpcError {
    #[error("RPC client error: {0}")]
    Client(#[from] ClientError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Invalid fixture format: {0}")]
    InvalidFixture(String),
}

/// Serializable account format for fixtures.
///
/// This is a simplified representation that can be easily serialized to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableAccount {
    lamports: u64,
    #[serde(with = "serde_bytes_as_base64")]
    data: Vec<u8>,
    owner: String,
    executable: bool,
    rent_epoch: u64,
}

/// Helper module for serializing bytes as base64 in JSON.
mod serde_bytes_as_base64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        base64::decode(&s).map_err(serde::de::Error::custom)
    }
}

impl From<Account> for SerializableAccount {
    fn from(account: Account) -> Self {
        Self {
            lamports: account.lamports,
            data: account.data,
            owner: account.owner.to_string(),
            executable: account.executable,
            rent_epoch: account.rent_epoch,
        }
    }
}

impl TryFrom<SerializableAccount> for Account {
    type Error = RpcError;

    fn try_from(acc: SerializableAccount) -> Result<Self, Self::Error> {
        let owner = acc
            .owner
            .parse::<Pubkey>()
            .map_err(|e| RpcError::InvalidFixture(format!("Invalid owner pubkey: {}", e)))?;

        Ok(Account {
            lamports: acc.lamports,
            data: acc.data,
            owner,
            executable: acc.executable,
            rent_epoch: acc.rent_epoch,
        })
    }
}

/// Fixture file format for account snapshots.
#[derive(Debug, Serialize, Deserialize)]
struct AccountFixture {
    /// Format version for future compatibility
    version: u8,
    /// Metadata about when/where the fixture was captured
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<FixtureMetadata>,
    /// Map of pubkey strings to account data
    accounts: HashMap<String, SerializableAccount>,
}

/// Optional metadata about the fixture.
#[derive(Debug, Serialize, Deserialize)]
struct FixtureMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_url: Option<String>,
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

    /// Fetch specific accounts by pubkey.
    ///
    /// This is useful when you know exactly which accounts you need to fetch.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use solana_pubkey::Pubkey;
    /// use mollusk_on_demand::RpcAccountStore;
    ///
    /// let pubkeys = vec![
    ///     "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".parse::<Pubkey>()?,
    ///     "11111111111111111111111111111111".parse::<Pubkey>()?,
    /// ];
    ///
    /// let accounts = RpcAccountStore::new("https://api.mainnet-beta.solana.com")
    ///     .from_pubkeys(&pubkeys)
    ///     .await?;
    /// ```
    pub async fn from_pubkeys(
        mut self,
        pubkeys: &[Pubkey],
    ) -> Result<HashMap<Pubkey, Account>, RpcError> {
        self.fetch_accounts(pubkeys).await?;
        Ok(self.cache)
    }

    /// Load accounts from a fixture file.
    ///
    /// This is useful for testing with pre-fetched accounts without needing
    /// to make RPC calls. The fixture file should be in JSON format created
    /// by `save_to_fixture`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use mollusk_on_demand::RpcAccountStore;
    ///
    /// let accounts = RpcAccountStore::from_fixture("fixtures/swap_accounts.json")?;
    /// ```
    pub fn from_fixture(path: impl AsRef<Path>) -> Result<HashMap<Pubkey, Account>, RpcError> {
        let content = fs::read_to_string(path)?;
        let fixture: AccountFixture = serde_json::from_str(&content)?;

        if fixture.version != 1 {
            return Err(RpcError::InvalidFixture(format!(
                "Unsupported fixture version: {}",
                fixture.version
            )));
        }

        let mut accounts = HashMap::new();
        for (pubkey_str, serializable_account) in fixture.accounts {
            let pubkey = pubkey_str
                .parse::<Pubkey>()
                .map_err(|e| RpcError::InvalidFixture(format!("Invalid pubkey: {}", e)))?;

            let account = Account::try_from(serializable_account)?;
            accounts.insert(pubkey, account);
        }

        Ok(accounts)
    }

    /// Save the current account cache to a fixture file.
    ///
    /// This creates a JSON file that can be loaded later with `from_fixture`,
    /// allowing you to capture mainnet state once and reuse it for testing.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use mollusk_on_demand::RpcAccountStore;
    ///
    /// // Fetch accounts from mainnet
    /// let accounts = RpcAccountStore::new("https://api.mainnet-beta.solana.com")
    ///     .from_instruction(&instruction)
    ///     .await?;
    ///
    /// // Save to fixture for later use
    /// RpcAccountStore::save_to_fixture(&accounts, "fixtures/swap_accounts.json")?;
    /// ```
    pub fn save_to_fixture(
        accounts: &HashMap<Pubkey, Account>,
        path: impl AsRef<Path>,
    ) -> Result<(), RpcError> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }

        let serializable_accounts: HashMap<String, SerializableAccount> = accounts
            .iter()
            .map(|(pubkey, account)| (pubkey.to_string(), SerializableAccount::from(account.clone())))
            .collect();

        let fixture = AccountFixture {
            version: 1,
            metadata: Some(FixtureMetadata {
                slot: None,
                timestamp: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .to_string(),
                ),
                rpc_url: None,
            }),
            accounts: serializable_accounts,
        };

        let json = serde_json::to_string_pretty(&fixture)?;
        fs::write(path, json)?;

        Ok(())
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