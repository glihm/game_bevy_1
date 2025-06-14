//! Position components, which is mapping the data from the Position model in Cairo.

use bevy::prelude::*;
use dojo_types::schema::Struct;
use starknet::core::types::Felt;
use tokio::sync::mpsc::{Receiver, Sender};

/// This event will be triggered every time the position is updated.
#[derive(Event)]
pub struct PositionUpdatedEvent(pub Position);

/// Since the position will be updated by the Torii subscription, we need a channel to send the position to the main thread.
/// We can't directly emit the Bevy event, which is not thread-safe, that's why we use a channel.
#[derive(Resource)]
pub struct PositionUpdateChannel {
    pub sender: Sender<Position>,
    pub receiver: Receiver<Position>,
}

/// The position of the player in the game.
#[derive(Component, Debug)]
pub struct Position {
    pub player: Felt,
    pub x: u32,
    pub y: u32,
}

/// This implementation shows a manual way to map data from the Position model in Cairo.
/// Ideally, we want a binding generation to do that for us.
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
