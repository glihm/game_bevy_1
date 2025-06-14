//! Dojo related resources.
//!
//! The Dojo resource is a wrapper around the Starknet and Torii connection resources.
//! It also embeds a tokio runtime, since Bevy is by default single-threaded.
//!
//! This resources aims at providing a single point of access to interact with Dojo.

use bevy::ecs::world::CommandQueue;
use bevy::input::ButtonState;
use bevy::platform::collections::HashMap;
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
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;
use torii_grpc_client::WorldClient;
use torii_grpc_client::types::proto::world::{RetrieveEntitiesResponse, SubscribeEntityResponse};
use torii_grpc_client::types::{
    Clause, KeysClause, Pagination, PaginationDirection, PatternMatching, Query as ToriiQuery,
};
use url::Url;

/// The Dojo plugin to connect Bevy to Torii and Starknet.
pub struct DojoPlugin;

impl Plugin for DojoPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<DojoInitializedEvent>();
        app.add_event::<DojoEntityUpdated>();
        app.add_systems(Update, (check_torii_task, check_sn_task));
    }
}

/// This event is emitted when the Dojo is initialized.
#[derive(Event)]
pub struct DojoInitializedEvent;

/// This event is emitted everytime we receive an entity update from Torii.
/// This could be done by fetching entities, or by subscribing to entities updates.
#[derive(Event, Debug)]
pub struct DojoEntityUpdated {
    pub entity_id: Felt,
    pub models: Vec<Struct>,
}

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
    pub client: Option<Arc<Mutex<WorldClient>>>,
    pub pending_retrieve_entities:
        VecDeque<JoinHandle<Result<RetrieveEntitiesResponse, torii_grpc_client::Error>>>,
    pub subscriptions: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub subscription_sender: Option<Arc<Mutex<Sender<(Felt, Vec<Struct>)>>>>,
    pub subscription_receiver: Option<Arc<Mutex<Receiver<(Felt, Vec<Struct>)>>>>,
}

/// Dojo resource that embeds Starknet and Torii connection.
#[derive(Resource, Default)]
pub struct DojoResource {
    pub sn: StarknetConnection,
    pub torii: ToriiConnection,
}

impl DojoResource {
    /// Connects to Torii and Starknet local account.
    ///
    /// Since Bevy is by default single-threaded, this function is not async
    /// to be callable from Bevy systems.
    /// The Dojo plugin will then register some systems to check the state
    /// of the connection and spawn the tasks if needed: `check_torii_task` and
    /// `check_sn_task`.
    pub fn connect(&mut self, tokio: &TokioRuntime, torii_url: String, world_address: Felt) {
        info!("Connecting to Starknet and Torii.");
        let task = tokio
            .runtime
            .spawn(async move { connect_to_starknet().await });
        self.sn.connecting_task = Some(task);

        let task = tokio
            .runtime
            .spawn(async move { WorldClient::new(torii_url, world_address).await });
        self.torii.init_task = Some(task);

        let (sender, receiver) = channel(100);
        self.torii.subscription_sender = Some(Arc::new(Mutex::new(sender)));
        self.torii.subscription_receiver = Some(Arc::new(Mutex::new(receiver)));
    }

    /// Queues a transaction to be sent to the Starknet account.
    ///
    /// This function is not async to be callable from Bevy systems.
    /// The Dojo plugin will then register a system to check the state of the
    /// transaction and send it to the Starknet account if needed in an asynchronous
    /// way (`check_sn_task`).
    pub fn queue_tx(&mut self, tokio: &TokioRuntime, calls: Vec<Call>) {
        if let Some(account) = self.sn.account.clone() {
            let task = tokio.runtime.spawn(async move {
                let tx = account.execute_v3(calls);
                tx.send().await
            });

            self.sn.pending_txs.push_back(task);
        } else {
            warn!("No Starknet account initialized, skipping transaction.");
        }
    }

    /// Queues a retrieve entities query to be sent to Torii.
    ///
    /// For the async nature of the Dojo plugin, we need to queue the query
    /// to be sent to Torii in an asynchronous way.
    /// The Dojo plugin will then register a system to check for all the queries
    /// to send, and will emit an event when a query response is available.
    pub fn queue_retrieve_entities(&mut self, tokio: &TokioRuntime, query: ToriiQuery) {
        if let Some(client) = self.torii.client.clone() {
            let task = tokio.runtime.spawn(async move {
                let mut client = client.lock().await;
                client.retrieve_entities(query).await
            });

            self.torii.pending_retrieve_entities.push_back(task);
        } else {
            warn!("No Torii client initialized, skipping query.");
        }
    }

    pub fn subscribe_entities(&mut self, tokio: &TokioRuntime, id: String, clause: Option<Clause>) {
        if let Some(client) = self.torii.client.clone() {
            let sender = self.torii.subscription_sender.clone();
            let task = tokio.runtime.spawn(async move {
                let mut subscription = client
                    .lock()
                    .await
                    .subscribe_entities(None)
                    .await
                    .expect("Failed to subscribe");

                while let Some(Ok((n, e))) = subscription.next().await {
                    debug!("Torii subscribe entities update: {} {:?}", n, e);
                    if let Some(ref sender) = sender {
                        let _ = sender.lock().await.send((e.hashed_keys, e.models)).await;
                    }
                }
            });

            // If the id already exists, we should replace the existing one.
            tokio.runtime.block_on(async {
                let mut subscriptions = self.torii.subscriptions.lock().await;
                subscriptions.insert(id, task);
            });
        } else {
            warn!("No Torii client initialized, skipping subscription.");
        }
    }
}

/// This task is responsible for checking if the Torii client needs to be initialized.
fn check_torii_task(
    tokio: Res<TokioRuntime>,
    mut dojo: ResMut<DojoResource>,
    mut ev_retrieve_entities: EventWriter<DojoEntityUpdated>,
    mut ev_initialized: EventWriter<DojoInitializedEvent>,
) {
    if let Some(task) = &mut dojo.torii.init_task {
        if let Ok(Ok(client)) = tokio.runtime.block_on(async { task.await }) {
            info!("Torii client initialized.");
            dojo.torii.client = Some(Arc::new(Mutex::new(client)));
            dojo.torii.init_task = None;
            ev_initialized.write(DojoInitializedEvent);
        }
    }

    if !dojo.torii.pending_retrieve_entities.is_empty() {
        if let Some(task) = dojo.torii.pending_retrieve_entities.pop_front() {
            if let Ok(Ok(response)) = tokio.runtime.block_on(async { task.await }) {
                debug!("Retrieve entities response: {:?}", response);
                for e in response.entities {
                    ev_retrieve_entities.write(DojoEntityUpdated {
                        entity_id: Felt::from_bytes_be_slice(&e.hashed_keys),
                        models: e
                            .models
                            .into_iter()
                            .map(|m| m.try_into().unwrap())
                            .collect(),
                    });
                }
            }
        }
    }

    // Pushing the subscription update to the event writer for other systems to use.
    if let Some(receiver) = &mut dojo.torii.subscription_receiver {
        if let Ok((entity_id, models)) = tokio.runtime.block_on(async {
            let mut receiver = receiver.lock().await;
            receiver.try_recv()
        }) {
            debug!("Torii subscription update: {:?}", (entity_id, &models));
            ev_retrieve_entities.write(DojoEntityUpdated { entity_id, models });
        }
    }
}

/// This task is responsible for checking the Starknet connection and transactions
/// that have been queued to be sent to the blockchain.
fn check_sn_task(tokio: Res<TokioRuntime>, mut dojo: ResMut<DojoResource>) {
    if let Some(task) = &mut dojo.sn.connecting_task {
        if let Ok(account) = tokio.runtime.block_on(async { task.await }) {
            info!("Connected to Starknet.");
            dojo.sn.account = Some(account);
            dojo.sn.connecting_task = None;
        }
    }

    if !dojo.sn.pending_txs.is_empty() && dojo.sn.account.is_some() {
        if let Some(task) = dojo.sn.pending_txs.pop_front() {
            if let Ok(Ok(result)) = tokio.runtime.block_on(async { task.await }) {
                info!("Transaction completed: {:#x}", result.transaction_hash);
            }
        }
    }
}

/// Connects to Starknet by creating a single owner account.
async fn connect_to_starknet() -> Arc<SingleOwnerAccount<AnyProvider, LocalWallet>> {
    let account_addr = Felt::from_hex_unchecked(
        "0x127fd5f1fe78a71f8bcd1fec63e3fe2f0486b6ecd5c86a0466c3a21fa5cfcec",
    );
    let private_key = Felt::from_hex_unchecked(
        "0xc5b2fcab997346f3ea1c00b002ecf6f382c5f9c9659a3894eb783c5320f912",
    );

    let rpc_url = Url::parse("http://0.0.0.0:5050").expect("Expecting Starknet RPC URL");
    let provider =
        AnyProvider::JsonRpcHttp(JsonRpcClient::new(HttpTransport::new(rpc_url.clone())));

    let chain_id = provider.chain_id().await.unwrap();

    let signer = LocalWallet::from(SigningKey::from_secret_scalar(private_key));
    let address = account_addr;

    Arc::new(SingleOwnerAccount::new(
        provider,
        signer,
        address,
        chain_id,
        ExecutionEncoding::New,
    ))
}
