//! Setup related components and systems.
use bevy::prelude::*;

#[derive(Component)]
pub struct Cube;

/// Setups the scene with basic light and one cube.
/// The cube is expected to move only on X and Y axis (even if we still use 3D for now).
pub fn setup(
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
