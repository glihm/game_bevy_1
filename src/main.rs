use bevy::ecs::world::CommandQueue;
use bevy::input::ButtonState;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures_lite::future};
use bevy::{input::keyboard::KeyboardInput, prelude::*};
use starknet::accounts::single_owner::SignError;
use starknet::accounts::{Account, AccountError, ExecutionEncoding, SingleOwnerAccount};
use starknet::core::types::{Call, InvokeTransactionResult};
use starknet::providers::jsonrpc::HttpTransport;
use starknet::providers::{JsonRpcClient, Provider, ProviderError};
use starknet::signers::local_wallet::SignError as LocalWalletSignError;
use starknet::signers::{LocalWallet, SigningKey};
use starknet::{core::types::Felt, providers::AnyProvider};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use torii_grpc_client::WorldClient;
use torii_grpc_client::types::{KeysClause, Pagination, PaginationDirection, PatternMatching, Query as ToriiQuery};

use url::Url;

const WORLD_ADDRESS: Felt =
    Felt::from_hex_unchecked("0x07cb912d0029e3799c4b8f2253b21481b2ec814c5daf72de75164ca82e7c42a5");

// Resource to store the Tokio runtime.
// This is a required resource since reqwest (used by starknet-rs) requires a runtime.
#[derive(Resource)]
struct TokioRuntime {
    runtime: Runtime,
}

impl Default for TokioRuntime {
    fn default() -> Self {
        Self {
            runtime: Runtime::new().expect("Failed to create Tokio runtime"),
        }
    }
}

// Resource to store our Starknet connection state
#[derive(Resource, Default)]
struct StarknetConnection {
    connecting_task: Option<JoinHandle<Arc<SingleOwnerAccount<AnyProvider, LocalWallet>>>>,
    account: Option<Arc<SingleOwnerAccount<AnyProvider, LocalWallet>>>,
    pending_txs: VecDeque<
        JoinHandle<Result<InvokeTransactionResult, AccountError<SignError<LocalWalletSignError>>>>,
    >,
}

#[derive(Resource, Default)]
struct ToriiConnection {
    init_task: Option<JoinHandle<Result<WorldClient, torii_grpc_client::Error>>>,
    torii: Option<WorldClient>,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .init_resource::<TokioRuntime>()
        .init_resource::<StarknetConnection>()
        .init_resource::<ToriiConnection>()
        .add_systems(
            Update,
            (handle_keyboard_input, check_sn_task, check_torii_task),
        )
        .run();
}

fn handle_keyboard_input(
    runtime: Res<TokioRuntime>,
    mut sn: ResMut<StarknetConnection>,
    mut keyboard_input_events: EventReader<KeyboardInput>,
    mut torii: ResMut<ToriiConnection>,
) {
    for event in keyboard_input_events.read() {
        if event.key_code == KeyCode::KeyI && torii.init_task.is_none() {
            let task = runtime.runtime.spawn(async move {
                WorldClient::new("http://localhost:8080".to_string(), WORLD_ADDRESS).await
            });
            torii.init_task = Some(task);
        } else if event.key_code == KeyCode::KeyC && sn.connecting_task.is_none() {
            info!("Starting connection...");
            let task = runtime
                .runtime
                .spawn(async move { connect_to_starknet().await });
            sn.connecting_task = Some(task);
        } else if event.key_code == KeyCode::KeyT && event.state == ButtonState::Pressed {
            println!("event: {:?}", event);
            if let Some(account) = sn.account.clone() {
                let calls = vec![Call {
                    to: Felt::from_hex_unchecked(
                        "0x00a92391c5bcde7af4bad5fd0fff3834395b1ab8055a9abb8387c0e050a34edf",
                    ),
                    selector: Felt::from_hex_unchecked(
                        "0x0217c73ea9ef26581623f20edd45571c1d024612b70d0af3e0842c5b0dc253cd",
                    ),
                    calldata: vec![],
                }];

                // Move both account and calls into the async block
                let task = runtime.runtime.spawn(async move {
                    // Create the transaction inside the async block where we own the account
                    let tx = account.execute_v3(calls);
                    tx.send().await
                });

                sn.pending_txs.push_back(task);
            }
        }
    }
}

fn check_torii_task(runtime: Res<TokioRuntime>, mut torii: ResMut<ToriiConnection>) {
    if let Some(task) = &mut torii.init_task {
        if let Ok(Ok(client)) = runtime.runtime.block_on(async { task.await }) {
            info!("Torii client initialized!");
            torii.torii = Some(client);
            torii.init_task = None;

            runtime.runtime.block_on(async move {
                let response = torii.torii.as_mut().unwrap()
                    .retrieve_entities(
                        ToriiQuery {
                            clause: None,
                            pagination: Pagination {
                                limit: 100,
                                cursor: None,
                                direction: PaginationDirection::Forward,
                                order_by: vec![],
                            },
                            no_hashed_keys: false,
                            models: vec![],
                            historical: false,
                        },
                    )
                    .await
                    .unwrap_or_default();

                println!("entities: {:?}", response);
            });
        }
    }
}
fn check_sn_task(runtime: Res<TokioRuntime>, mut sn: ResMut<StarknetConnection>) {
    // Check connection task
    if let Some(task) = &mut sn.connecting_task {
        if let Ok(account) = runtime.runtime.block_on(async { task.await }) {
            info!("Connected to Starknet!");
            sn.account = Some(account);
            sn.connecting_task = None;
        }
    }

    // Check pending transactions
    if !sn.pending_txs.is_empty() && sn.account.is_some() {
        if let Some(task) = sn.pending_txs.pop_front() {
            if let Ok(Ok(result)) = runtime.runtime.block_on(async { task.await }) {
                info!("Transaction completed: {:#x}", result.transaction_hash);
            }
        }
    }
}

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
    //let chain_id = Felt::from(123);

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
