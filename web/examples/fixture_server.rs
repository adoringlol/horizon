//! Local-only browser integration fixture. Generates original synthetic blocks;
//! contains no Minecraft assets or prepared resources. Not a game server.
use horizon_protocol::{
    PROTOCOL_VERSION, Result,
    chunk::{ChunkData, LightData, Paletted, Section, Strategy},
    connection::Connection,
    packets::{Empty, configuration as cfg, handshake, login, play, status},
};
use std::{
    net::{TcpListener, TcpStream},
    time::Duration,
};

fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:25579")?;
    println!("Horizon browser fixture on 127.0.0.1:25579 (synthetic terrain, offline mode)");
    for stream in listener.incoming() {
        let stream = stream?;
        std::thread::spawn(move || {
            if let Err(e) = serve(stream) {
                eprintln!("Fixture connection ended: {e}");
            }
        });
    }
    Ok(())
}

fn serve(stream: TcpStream) -> Result<()> {
    let mut c = Connection::from_stream(stream)?;
    c.reader.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (id, bytes) = c.recv()?;
    let handshake::Serverbound::Intention(intent) = handshake::Serverbound::decode(id, &bytes)?
    else {
        return Ok(());
    };
    if intent.intent == handshake::STATUS {
        c.recv()?;
        return c.send(&status::Clientbound::StatusResponse(status::StatusResponse {
            json: serde_json::json!({"version":{"name":"Horizon browser fixture","protocol":PROTOCOL_VERSION}, "players":{"online":0,"max":10},"description":{"text":"Original synthetic terrain for browser testing"}}).to_string(),
        }).encode());
    }
    let (id, bytes) = c.recv()?;
    let login::Serverbound::Hello(hello) = login::Serverbound::decode(id, &bytes)? else {
        return Ok(());
    };
    c.send(&login::Clientbound::Compression(login::Compression { threshold: 256 }).encode())?;
    c.set_compression(256);
    c.send(
        &login::Clientbound::LoginFinished(login::LoginFinished {
            profile: login::GameProfile {
                name: hello.name.clone(),
                uuid: hello.uuid,
                properties: vec![],
            },
            session: 1,
        })
        .encode(),
    )?;
    let (id, bytes) = c.recv()?;
    if !matches!(
        login::Serverbound::decode(id, &bytes)?,
        login::Serverbound::LoginAcknowledged(_)
    ) {
        return Ok(());
    }
    c.send(
        &cfg::Clientbound::RegistryData(cfg::RegistryData {
            registry: "minecraft:dimension_type".into(),
            entries: vec![cfg::RegistryEntry {
                id: "minecraft:overworld".into(),
                data: None,
            }],
        })
        .encode(),
    )?;
    c.send(
        &cfg::Clientbound::RegistryData(cfg::RegistryData {
            registry: "minecraft:worldgen/biome".into(),
            entries: vec![cfg::RegistryEntry {
                id: "minecraft:plains".into(),
                data: None,
            }],
        })
        .encode(),
    )?;
    c.send(&cfg::Clientbound::SelectKnownPacks(cfg::SelectKnownPacks { packs: vec![] }).encode())?;
    c.send(&cfg::Clientbound::FinishConfiguration(Empty).encode())?;
    loop {
        let (id, bytes) = c.recv()?;
        if matches!(
            cfg::Serverbound::decode(id, &bytes)?,
            cfg::Serverbound::FinishConfiguration(_)
        ) {
            break;
        }
    }
    c.send(
        &play::Clientbound::Login(play::Login {
            player_id: 1,
            hardcore: false,
            levels: vec!["minecraft:overworld".into()],
            max_players: 10,
            chunk_radius: 2,
            simulation_distance: 2,
            reduced_debug_info: false,
            show_death_screen: true,
            limited_crafting: false,
            spawn: play::SpawnInfo {
                dimension_type: 0,
                dimension: "minecraft:overworld".into(),
                seed: 0,
                game_type: 3,
                previous_game_type: None,
                debug: false,
                flat: true,
                last_death: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            online_mode: false,
            enforces_secure_chat: false,
        })
        .encode(),
    )?;
    c.send(
        &play::Clientbound::PlayerPosition(play::PlayerPosition {
            teleport: 1,
            position: [8.5, 65.0, 8.5],
            delta: [0.; 3],
            yaw: -25.,
            pitch: 12.,
            relatives: 0,
        })
        .encode(),
    )?;
    c.send(&play::Clientbound::ChunkBatchStart(Empty).encode())?;
    for cx in -1..=1 {
        for cz in -1..=1 {
            c.send(
                &play::Clientbound::LevelChunkWithLight(play::LevelChunkWithLight {
                    x: cx,
                    z: cz,
                    chunk: ChunkData {
                        heightmaps: vec![],
                        buffer: ChunkData::from_sections(&terrain(cx, cz)),
                        block_entities: vec![],
                    },
                    light: LightData {
                        sky_mask: vec![],
                        block_mask: vec![],
                        empty_sky_mask: vec![],
                        empty_block_mask: vec![],
                        sky: vec![],
                        block: vec![],
                    },
                })
                .encode(),
            )?;
        }
    }
    c.send(&play::Clientbound::ChunkBatchFinished(play::ChunkBatchFinished { size: 9 }).encode())?;
    c.send(&play::Clientbound::SystemChat(play::SystemChat { content: horizon_protocol::text::literal("Local fixture connected. This is real Horizon protocol traffic over the selected gateway."), overlay: false }).encode())?;
    println!("Browser logged in; sent nine compressed terrain chunks.");
    loop {
        let (id, bytes) = c.recv()?;
        match play::Serverbound::decode(id, &bytes)? {
            play::Serverbound::AcceptTeleportation(_) => println!("Browser acknowledged teleport."),
            play::Serverbound::ChunkBatchReceived(_) => {
                println!("Browser acknowledged chunk batch.")
            }
            _ => {}
        }
    }
}

fn terrain(cx: i32, cz: i32) -> Vec<Section> {
    (0..24)
        .map(|sy| {
            let mut values = vec![0; 4096];
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let wy = -64 + sy * 16 + y;
                        let wx = cx * 16 + x;
                        let wz = cz * 16 + z;
                        let ground = (60..=63).contains(&wy);
                        let tower = (20..25).contains(&wx)
                            && (19..24).contains(&wz)
                            && (64..76).contains(&wy);
                        let steps = (2..6).contains(&wx)
                            && (18..29).contains(&wz)
                            && wy >= 64
                            && wy < 64 + (wz - 17) / 2;
                        let leaves = (12..16).contains(&wx)
                            && (18..22).contains(&wz)
                            && (64..67).contains(&wy);
                        let glass = wx == 18 && (17..23).contains(&wz) && (64..69).contains(&wy);
                        if ground || tower || steps || leaves || glass {
                            values[(y << 8 | z << 4 | x) as usize] = if glass {
                                6
                            } else if leaves {
                                5
                            } else if tower {
                                2
                            } else if steps {
                                if wy == 63 + (wz - 17) / 2 { 7 } else { 3 }
                            } else {
                                1
                            };
                        }
                    }
                }
            }
            Section {
                non_empty: values.iter().filter(|v| **v != 0).count() as i16,
                fluids: 0,
                states: Paletted::from_values(&values, Strategy::Blocks, 16),
                biomes: Paletted::from_values(&[0; 64], Strategy::Biomes, 1),
            }
        })
        .collect()
}
