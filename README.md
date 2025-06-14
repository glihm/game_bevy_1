# Dojo Bevy Plugin

This repository contains a plugin for the [Dojo](https://github.com/dojoengine/dojo) framework,
which allows to connect to Torii and Starknet using Bevy.

## Setup

1. Install Dojo `1.5.1` by running:
```bash
dojoup install 1.5.1
```

2. Clone the [dojo-intro](https://github.com/dojoengine/dojo-intro) repository and compiles it.

```bash
git clone https://github.com/dojoengine/dojo-intro.git
cd dojo-intro/contracts
sozo build
```

3. Run Katana and migrate (still in the `dojo-intro/contracts` directory):

```bash
katana --config ./katana.toml
sozo migrate
```

4. Run Torii:

```bash
torii --config ./torii_dev.toml
```

5. Run this example:

```bash
cargo run --features bevy/dynamic_linking
```

## How to play

More is coming with better UI but currently you can:

1. Press `C` to connect to Torii and Starknet.
2. Press `S` to subscribe to Torii entities updates.
3. Press `Space` to spawn a cube.
4. Press the arrows to move the cube.
