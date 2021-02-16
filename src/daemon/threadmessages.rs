use crate::revaultd::VaultStatus;
use revault_tx::{
    bitcoin::{util::bip32::ChildNumber, Address, Amount, OutPoint, Txid},
    transactions::{
        CancelTransaction, EmergencyTransaction, SpendTransaction, UnvaultEmergencyTransaction,
        UnvaultTransaction,
    },
};

use std::sync::mpsc::SyncSender;

/// Incoming from RPC server thread
#[derive(Debug)]
pub enum RpcMessageIn {
    Shutdown,
    // Network, blockheight, sync progress
    GetInfo(SyncSender<(String, u32, f64)>),
    ListVaults(
        (Option<Vec<VaultStatus>>, Option<Vec<OutPoint>>),
        SyncSender<Vec<ListVaultsEntry>>,
    ),
    DepositAddr(SyncSender<Address>),
    GetRevocationTxs(
        OutPoint,
        // None if the deposit does not exist
        // FIXME: use a Result with RpcControlError!
        SyncSender<
            Option<(
                CancelTransaction,
                EmergencyTransaction,
                UnvaultEmergencyTransaction,
            )>,
        >,
    ),
    // Returns None if the transactions could all be stored succesfully
    // FIXME: use a Result with RpcControlError!
    RevocationTxs(
        (
            OutPoint,
            CancelTransaction,
            EmergencyTransaction,
            UnvaultEmergencyTransaction,
        ),
        SyncSender<Option<String>>,
    ),
    GetUnvaultTx(
        OutPoint,
        SyncSender<Result<UnvaultTransaction, RpcControlError>>,
    ),
    ListTransactions(
        Option<Vec<OutPoint>>,
        SyncSender<
            // None if the deposit does not exist
            Vec<VaultTransactions>,
        >,
    ),
}

/// Outgoing to the bitcoind poller thread
#[derive(Debug)]
pub enum BitcoindMessageOut {
    Shutdown,
    SyncProgress(SyncSender<f64>),
    WalletTransaction(Txid, SyncSender<Option<WalletTransaction>>),
}

/// Outgoing to the signature fetcher thread
#[derive(Debug)]
pub enum SigFetcherMessageOut {
    Shutdown,
}

#[derive(Debug)]
pub struct WalletTransaction {
    pub hex: String,
    // None if unconfirmed
    pub blockheight: Option<u32>,
    pub received_time: u32,
}

#[derive(Debug)]
pub struct TransactionResource<T> {
    // None if not broadcast
    pub wallet_tx: Option<WalletTransaction>,
    pub tx: T,
    pub is_signed: bool,
}

#[derive(Debug)]
pub struct VaultTransactions {
    pub outpoint: OutPoint,
    pub deposit: WalletTransaction,
    pub unvault: TransactionResource<UnvaultTransaction>,
    // None if not spending
    pub spend: Option<TransactionResource<SpendTransaction>>,
    pub cancel: TransactionResource<CancelTransaction>,
    // None if not stakeholder
    pub emergency: Option<TransactionResource<EmergencyTransaction>>,
    pub unvault_emergency: Option<TransactionResource<UnvaultEmergencyTransaction>>,
}

#[derive(Debug)]
pub struct ListVaultsEntry {
    pub amount: Amount,
    pub status: VaultStatus,
    pub deposit_outpoint: OutPoint,
    pub derivation_index: ChildNumber,
    pub address: Address,
    pub updated_at: u32,
}

/// An error that occured during RPC message handling
#[derive(Debug)]
pub enum RpcControlError {
    UnknownOutpoint(OutPoint),
    // .0 is current status, .1 is required status
    InvalidStatus((VaultStatus, VaultStatus)),
}

impl std::fmt::Display for RpcControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOutpoint(ref o) => write!(f, "No vault at '{}'", o),
            Self::InvalidStatus((current, required)) => write!(
                f,
                "Invalid vault status: '{}'. Need '{}'",
                current, required
            ),
        }
    }
}

impl std::error::Error for RpcControlError {}
