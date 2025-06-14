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

use url::Url;

const WORLD_ADDRESS: Felt =
    Felt::from_hex_unchecked("0x04d9778a74d2c9e6e7e4a24cbe913998a80de217c66ee173a604d06dea5469c3");

const ACTION_ADDRESS: Felt =
    Felt::from_hex_unchecked("0x00b056c9813fdc442118bdfead6fda526e5daa5fd7d543304117ed80154ea752");

const SPAWN_SELECTOR: Felt = selector!("spawn");
const MOVE_SELECTOR: Felt = selector!("move");

#[derive(Component, Debug)]
pub struct Position {
    pub player: Felt,
    pub x: u32,
    pub y: u32,
}

impl From<Struct> for Position {
    fn from(struct_value: Struct) -> Self {
        let player = struct_value
            .get("player")
            .unwrap()
            .as_primitive()
            .unwrap()
            .as_contract_address()
            .unwrap();
        let x = struct_value
            .get("x")
            .unwrap()
            .as_primitive()
            .unwrap()
            .as_u32()
            .unwrap();
        let y = struct_value
            .get("y")
            .unwrap()
            .as_primitive()
            .unwrap()
            .as_u32()
            .unwrap();

        Position { player, x, y }
    }
}

#[derive(Event)]
struct PositionUpdatedEvent(Position);

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
    subscription_task: Option<JoinHandle<()>>,
    is_subscribed: bool,
}

#[derive(Resource)]
struct PositionUpdateChannel {
    pub sender: Sender<Position>,
    pub receiver: Receiver<Position>,
}

fn main() {
    let (sender, receiver) = channel(1);

    App::new()
        .add_plugins(DefaultPlugins)
        .init_resource::<TokioRuntime>()
        .init_resource::<StarknetConnection>()
        .init_resource::<ToriiConnection>()
        .insert_resource(PositionUpdateChannel { sender, receiver })
        .add_event::<PositionUpdatedEvent>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                handle_keyboard_input,
                check_sn_task,
                check_torii_task,
                handle_torii_subscription,
                process_position_updates,

                // Ensure the cube position is updated after the position updates are processed.
                // This avoids the 1-frame overhead if the event is missed in the `update_cube_position` system.
                update_cube_position.after(process_position_updates),
            )
        )
        .run();
}

fn handle_keyboard_input(
    runtime: Res<TokioRuntime>,
    mut sn: ResMut<StarknetConnection>,
    mut torii: ResMut<ToriiConnection>,
    mut keyboard_input_events: EventReader<KeyboardInput>,
    position_channel: Res<PositionUpdateChannel>,
) {
    for event in keyboard_input_events.read() {
        let key_code = event.key_code;

        if key_code == KeyCode::KeyI && torii.init_task.is_none() {
            let task = runtime.runtime.spawn(async move {
                WorldClient::new("http://localhost:8080".to_string(), WORLD_ADDRESS).await
            });
            torii.init_task = Some(task);
        } else if key_code == KeyCode::KeyS && !torii.is_subscribed {
            dbg!("SETUP SUBSCRIPTION...");
            if let Some(mut client) = torii.torii.take() {
                let sender = position_channel.sender.clone();
                let task = runtime.runtime.spawn(async move {
                    let mut subscription = client
                        .subscribe_entities(None)
                        .await
                        .expect("Failed to subscribe");

                    dbg!("SUBSCRIPTION CREATED...");

                    while let Some(Ok((n, e))) = subscription.next().await {
                        info!("Torii update: {} {:?}", n, e);

                        for m in e.models {
                            info!("model: {:?}", m);

                            match m.name {
                                ref name if name == "di-Position" => {
                                    let position: Position = m.into();
                                    println!("sending position: {:?}", position);
                                    let _ = sender.send(position).await;
                                }
                                name if name == "di-Moves".to_string() => {}
                                _ => panic!("skip"),
                            };
                        }
                    }
                });
                torii.subscription_task = Some(task);
                torii.is_subscribed = true;
                dbg!("IS SUBSCRIBED: {}", torii.is_subscribed);
            }
        } else if key_code == KeyCode::KeyC && sn.connecting_task.is_none() {
            info!("Starting connection...");
            let task = runtime
                .runtime
                .spawn(async move { connect_to_starknet().await });
            sn.connecting_task = Some(task);
        } else if key_code == KeyCode::KeyT && event.state == ButtonState::Pressed {
            println!("event: {:?}", event);
            if let Some(account) = sn.account.clone() {
                let calls = vec![Call {
                    to: ACTION_ADDRESS,
                    selector: SPAWN_SELECTOR,
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

        // Handles the arrows to send a transaction to move the cube.
        if vec![
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
        ]
        .contains(&key_code)
            && event.state == ButtonState::Pressed
        {
            let direction = match key_code {
                KeyCode::ArrowLeft => 0,
                KeyCode::ArrowRight => 1,
                KeyCode::ArrowUp => 2,
                KeyCode::ArrowDown => 3,
                _ => panic!("Invalid key code"),
            };

            if let Some(account) = sn.account.clone() {
                let calls = vec![Call {
                    to: ACTION_ADDRESS,
                    selector: MOVE_SELECTOR,
                    calldata: vec![Felt::from(direction)],
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
                let response = torii
                    .torii
                    .as_mut()
                    .unwrap()
                    .retrieve_entities(ToriiQuery {
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
                    })
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

fn handle_torii_subscription(runtime: Res<TokioRuntime>, mut torii: ResMut<ToriiConnection>) {
    if let Some(task) = &mut torii.subscription_task {
        // Check if the subscription task is still running
        if runtime.runtime.block_on(async { task.is_finished() }) {
            info!("Torii subscription ended");
            torii.subscription_task = None;
            torii.is_subscribed = false;
            // Note: The client is consumed by the subscription task
            // We'll need to reinitialize it if we want to use it again
            torii.torii = None;
        }
    }
}

#[derive(Component)]
struct Cube;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(0.5, 0.5, 0.5))),
        MeshMaterial3d(materials.add(Color::srgb(0.8, 0.2, 0.2))),
        Cube,
    ));

    commands.spawn((
        DirectionalLight::default(),
        Transform::from_xyz(0.0, 0.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 0.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// Updates the cube position by reacting to event notifying
/// a position change from Torii.
///
/// We would want to use systems ordering to ensure this system is run after the keyboard move system,
/// this will make sure we avoid the 1-frame overhead if the event is missed.
fn update_cube_position(
    mut ev_position_updated: EventReader<PositionUpdatedEvent>,
    mut query: Query<&mut Transform, With<Cube>>,
) {
    for ev in ev_position_updated.read() {
        let Position { x, y, .. } = ev.0;
        for mut transform in &mut query {
            transform.translation = Vec3::new(x as f32, y as f32, 0.0);
        }
    }
}

/// Processes the position updates from the channel.
///
/// Technically, other systems could just use the channel resource directly.
/// However, for decoupling and better observability, we use an event writer,
/// which keeps Bevy's principles in mind.
fn process_position_updates(
    mut position_channel: ResMut<PositionUpdateChannel>,
    mut ev_position_updated: EventWriter<PositionUpdatedEvent>,
) {
    while let Ok(position) = position_channel.receiver.try_recv() {
        ev_position_updated.write(PositionUpdatedEvent(position));
    }
}
