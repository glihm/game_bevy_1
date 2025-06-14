//! Dojo related resources.
//!
//! The Dojo resource is a wrapper around the Starknet and Torii connection resources.
//! It also embeds a tokio runtime, since Bevy is by default single-threaded.
//!
//! This resources aims at providing a single point of access to interact with Dojo.

use bevy::ecs::world::CommandQueue;
use bevy::input::ButtonState;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures_lite::future};
use bevy::{input::keyboard::KeyboardInput, prelude::*};
use dojo_types::primitive::Primitive;
use dojo_types::schema::{Enum, EnumOption, Member, Struct, Ty};
use futures::StreamExt;
use starknet::accounts::single_owner::SignError;
use starknet::accounts::{Account, AccountError, ExecutionEncoding, SingleOwnerAccount};
use starknet::core::types::{Call, InvokeTransactionResult};
use starknet::macros::selector;
use starknet::providers::jsonrpc::HttpTransport;
use starknet::providers::{JsonRpcClient, Provider, ProviderError};
use starknet::signers::local_wallet::SignError as LocalWalletSignError;
use starknet::signers::{LocalWallet, SigningKey};
use starknet::{core::types::Felt, providers::AnyProvider};
use std::collections::VecDeque;
use std::os::unix::process;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;
use torii_grpc_client::WorldClient;
use torii_grpc_client::types::{
    Clause, KeysClause, Pagination, PaginationDirection, PatternMatching, Query as ToriiQuery,
};

/// Resource to store the Tokio runtime, required by starknet-rs.
///
/// This resource will never be used as mut, this is why it is not embedded in the Dojo resource.
#[derive(Resource)]
pub struct TokioRuntime {
    pub runtime: Runtime,
}

impl Default for TokioRuntime {
    fn default() -> Self {
        Self {
            runtime: Runtime::new().expect("Failed to create Tokio runtime"),
        }
    }
}

/// Starknet connection state.
#[derive(Default)]
pub struct StarknetConnection {
    pub connecting_task: Option<JoinHandle<Arc<SingleOwnerAccount<AnyProvider, LocalWallet>>>>,
    pub account: Option<Arc<SingleOwnerAccount<AnyProvider, LocalWallet>>>,
    pub pending_txs: VecDeque<
        JoinHandle<Result<InvokeTransactionResult, AccountError<SignError<LocalWalletSignError>>>>,
    >,
}

/// Torii connection state.
#[derive(Default)]
pub struct ToriiConnection {
    pub init_task: Option<JoinHandle<Result<WorldClient, torii_grpc_client::Error>>>,
    pub client: Option<WorldClient>,
    pub subscription_task: Option<JoinHandle<()>>,
    pub is_subscribed: bool,
}

/// Dojo resource that embeds Starknet and Torii connection.
#[derive(Resource, Default)]
pub struct Dojo {
    pub sn: StarknetConnection,
    pub torii: ToriiConnection,
}
