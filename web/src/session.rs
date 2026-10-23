//! Event-driven counterpart of horizon-client::net. WebSocket messages are
//! arbitrary TCP fragments, never assumed to be complete Minecraft packets.
use horizon_protocol::{
    Error, PROTOCOL_VERSION, Result,
    buf::{Reader, Writer},
    crypto::{self, Cfb8},
    frame,
    packets::{Empty, configuration as cfg, handshake, login, play, status},
    registry::{DimensionType, Dynamic},
    rsa::RsaPublicKey,
    text,
};
use std::collections::{HashMap, VecDeque};

pub enum Effect {
    Send(Vec<u8>),
    Status(String),
    ServerStatus(String),
    Authenticate(String),
    Chat(String),
    WorldChanged,
    Disconnected(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Status,
    Login,
    Configuration,
    Play,
    Closed,
}

struct Encryption {
    secret: [u8; 16],
    key: RsaPublicKey,
    challenge: Vec<u8>,
}

/// One connection owns all framing, registry and world state. Replacing this
/// value when changing routes prevents a late packet mutating the new world.
pub struct Session {
    pub phase: Phase,
    buffer: Vec<u8>,
    compression: Option<usize>,
    decrypt: Option<Cfb8>,
    encrypt: Option<Cfb8>,
    pending: Option<Encryption>,
    effects: VecDeque<Effect>,
    pub registries: Dynamic,
    pub dimension: DimensionType,
    pub chunks: HashMap<(i32, i32), Vec<horizon_protocol::chunk::Section>>,
    overrides: HashMap<[i32; 3], i32>,
    pub position: [f64; 3],
    pub rotation: (f32, f32),
    pub game_type: i32,
    pub health: f32,
    pub food: i32,
    pub received_packets: u64,
    pub positioned: bool,
    signed_in: bool,
    block_bits: u8,
    sender: crate::player_sync::PositionSender,
}

impl Session {
    pub fn new(host: &str, port: u16, name: &str, uuid: Option<u128>, ping: bool) -> Result<Self> {
        if name.is_empty()
            || name.len() > 16
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error::new(
                "Use a Minecraft name of 1–16 letters, digits or underscores.",
            ));
        }
        let mut s = Self {
            phase: if ping { Phase::Status } else { Phase::Login },
            buffer: vec![],
            compression: None,
            decrypt: None,
            encrypt: None,
            pending: None,
            effects: VecDeque::new(),
            registries: Dynamic::default(),
            dimension: DimensionType::vanilla("minecraft:overworld"),
            chunks: HashMap::new(),
            overrides: HashMap::new(),
            position: [0.0; 3],
            rotation: (0.0, 0.0),
            game_type: 0,
            health: 20.0,
            food: 20,
            received_packets: 0,
            positioned: false,
            signed_in: uuid.is_some(),
            block_bits: 16,
            sender: Default::default(),
        };
        s.send(
            handshake::Serverbound::Intention(handshake::Intention {
                protocol: PROTOCOL_VERSION,
                host: host.to_owned(),
                port,
                intent: if ping {
                    handshake::STATUS
                } else {
                    handshake::LOGIN
                },
            })
            .encode(),
        )?;
        if ping {
            s.send(status::Serverbound::StatusRequest(Empty).encode())?;
        } else {
            s.send(
                login::Serverbound::Hello(login::ServerHello {
                    name: name.to_owned(),
                    uuid: uuid.unwrap_or_else(|| crypto::offline_uuid(name)),
                })
                .encode(),
            )?;
        }
        Ok(s)
    }

    pub fn drain(&mut self) -> Vec<Effect> {
        self.effects.drain(..).collect()
    }

    fn send(&mut self, body: Vec<u8>) -> Result<()> {
        let mut wire = frame::encode(&body, self.compression)?;
        if let Some(cipher) = &mut self.encrypt {
            cipher.encrypt(&mut wire);
        }
        self.effects.push_back(Effect::Send(wire));
        Ok(())
    }

    pub fn receive(&mut self, mut bytes: Vec<u8>) -> Result<()> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        if self.buffer.len() + bytes.len() > frame::MAX_UNCOMPRESSED + frame::MAX_FRAME {
            return Err(Error::new("Server receive buffer limit exceeded."));
        }
        if let Some(cipher) = &mut self.decrypt {
            cipher.decrypt(&mut bytes);
        }
        self.buffer.extend(bytes);
        while self.pending.is_none() && self.phase != Phase::Closed {
            let Some(frame) = frame::split(&mut self.buffer)? else {
                break;
            };
            let body = frame::decompress(frame, self.compression)?;
            let mut r = Reader::new(&body);
            let id = r.varint()?;
            let payload = r.rest();
            self.received_packets += 1;
            match self.phase {
                Phase::Status => {
                    if let status::Clientbound::StatusResponse(p) =
                        status::Clientbound::decode(id, payload)?
                    {
                        self.effects.push_back(Effect::ServerStatus(p.json));
                        self.phase = Phase::Closed;
                    }
                }
                Phase::Login => self.login(login::Clientbound::decode(id, payload)?)?,
                Phase::Configuration => self.configure(cfg::Clientbound::decode(id, payload)?)?,
                Phase::Play => self.play(play::Clientbound::decode(id, payload)?)?,
                Phase::Closed => break,
            }
        }
        Ok(())
    }

    fn close(&mut self, reason: String) {
        self.phase = Phase::Closed;
        self.effects.push_back(Effect::Disconnected(reason));
    }

    fn login(&mut self, p: login::Clientbound) -> Result<()> {
        use login::Clientbound as C;
        match p {
            C::Disconnect(p) => self.close(p.reason_json),
            C::Compression(p) => {
                self.compression = Some(
                    usize::try_from(p.threshold)
                        .map_err(|_| Error::new("Invalid compression threshold"))?,
                );
            }
            C::Hello(p) => {
                let secret = crypto::generate_secret()?;
                let key = RsaPublicKey::from_der(&p.public_key)?;
                let hash = crypto::server_hash(&p.server_id, &secret, &p.public_key);
                self.pending = Some(Encryption {
                    secret,
                    key,
                    challenge: p.challenge,
                });
                if p.should_authenticate {
                    if !self.signed_in {
                        return Err(Error::new(
                            "Sign in with Microsoft before joining an online-mode server.",
                        ));
                    }
                    self.effects.push_back(Effect::Authenticate(hash));
                } else {
                    self.authenticated()?;
                }
            }
            C::LoginFinished(p) => {
                self.send(login::Serverbound::LoginAcknowledged(Empty).encode())?;
                self.phase = Phase::Configuration;
                self.effects.push_back(Effect::Status(format!(
                    "Configuring world as {}…",
                    p.profile.name
                )));
                self.send(
                    cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(6)).encode(),
                )?;
                let mut brand = Writer::new();
                brand.string("Horizon Web");
                self.send(
                    cfg::Serverbound::CustomPayload(cfg::CustomPayload {
                        channel: "minecraft:brand".into(),
                        data: brand.data,
                    })
                    .encode(),
                )?;
            }
            C::CustomQuery(p) => self.send(
                login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                    transaction: p.transaction,
                    payload: None,
                })
                .encode(),
            )?,
            C::CookieRequest(p) => self.send(
                login::Serverbound::CookieResponse(login::CookieResponse {
                    key: p.key,
                    payload: None,
                })
                .encode(),
            )?,
            C::Unknown(..) => {}
        }
        Ok(())
    }

    /// Called only after the gateway successfully joined the account to this
    /// server hash. The RSA response itself is sent before enabling AES.
    pub fn authenticated(&mut self) -> Result<()> {
        let p = self
            .pending
            .take()
            .ok_or_else(|| Error::new("No login challenge is pending"))?;
        self.send(
            login::Serverbound::Key(login::Key {
                key: p.key.encrypt(&p.secret)?,
                challenge: p.key.encrypt(&p.challenge)?,
            })
            .encode(),
        )?;
        self.encrypt = Some(Cfb8::new(&p.secret, &p.secret));
        self.decrypt = Some(Cfb8::new(&p.secret, &p.secret));
        Ok(())
    }

    fn configure(&mut self, p: cfg::Clientbound) -> Result<()> {
        use cfg::{Clientbound as C, Serverbound as S};
        let reply = match p {
            C::Disconnect(p) => {
                self.close(text::plain(&p.reason, &Default::default()));
                None
            }
            C::KeepAlive(p) => Some(S::KeepAlive(p)),
            C::Ping(p) => Some(S::Pong(p)),
            C::CookieRequest(p) => Some(S::CookieResponse(cfg::CookieResponse {
                key: p.key,
                payload: None,
            })),
            C::SelectKnownPacks(_) => {
                Some(S::SelectKnownPacks(cfg::SelectKnownPacks { packs: vec![] }))
            }
            C::ResourcePackPush(p) => Some(S::ResourcePack(cfg::ResourcePackResponse {
                id: p.id,
                action: 1,
            })),
            C::CodeOfConduct(_) => Some(S::AcceptCodeOfConduct(Empty)),
            C::RegistryData(p) => {
                self.registries.add(p);
                None
            }
            C::FinishConfiguration(_) => {
                self.phase = Phase::Play;
                self.effects
                    .push_back(Effect::Status("Receiving world…".into()));
                Some(S::FinishConfiguration(Empty))
            }
            _ => None,
        };
        if let Some(p) = reply {
            self.send(p.encode())?;
        }
        Ok(())
    }

    fn world(&mut self, spawn: play::SpawnInfo) {
        self.dimension = self.registries.dimension(spawn.dimension_type);
        self.game_type = spawn.game_type;
        self.chunks.clear();
        self.overrides.clear();
        self.positioned = false;
        self.sender = Default::default();
        self.effects.push_back(Effect::WorldChanged);
    }

    fn play(&mut self, p: play::Clientbound) -> Result<()> {
        use play::{Clientbound as C, Serverbound as S};
        match p {
            C::Disconnect(p) => self.close(text::plain(&p.reason, &Default::default())),
            C::KeepAlive(p) => self.send(S::KeepAlive(p).encode())?,
            C::Ping(p) => self.send(S::Pong(p).encode())?,
            C::StartConfiguration(_) => {
                self.send(S::ConfigurationAcknowledged(Empty).encode())?;
                self.phase = Phase::Configuration;
                self.registries = Dynamic::default();
            }
            C::Login(p) => self.world(p.spawn),
            C::Respawn(p) => self.world(p.spawn),
            C::PlayerPosition(p) => {
                let flags = [play::relative::X, play::relative::Y, play::relative::Z];
                for (i, flag) in flags.iter().enumerate() {
                    self.position[i] = p.position[i]
                        + if p.relatives & flag != 0 {
                            self.position[i]
                        } else {
                            0.0
                        };
                }
                self.rotation.0 = p.yaw
                    + if p.relatives & play::relative::Y_ROT != 0 {
                        self.rotation.0
                    } else {
                        0.0
                    };
                self.rotation.1 = p.pitch
                    + if p.relatives & play::relative::X_ROT != 0 {
                        self.rotation.1
                    } else {
                        0.0
                    };
                self.send(
                    S::AcceptTeleportation(play::AcceptTeleportation {
                        id: p.teleport,
                        x: self.position[0],
                        y: self.position[1],
                        z: self.position[2],
                        yaw: self.rotation.0,
                        pitch: self.rotation.1,
                    })
                    .encode(),
                )?;
                if !self.positioned {
                    self.send(S::PlayerLoaded(Empty).encode())?;
                }
                self.positioned = true;
                self.effects.push_back(Effect::Status(
                    "Connected · experimental terrain viewer".into(),
                ));
            }
            C::PlayerRotation(p) => {
                self.rotation.0 = p.yaw + if p.relative_yaw { self.rotation.0 } else { 0.0 };
                self.rotation.1 = p.pitch
                    + if p.relative_pitch {
                        self.rotation.1
                    } else {
                        0.0
                    };
            }
            C::ChunkBatchFinished(_) => self.send(
                S::ChunkBatchReceived(play::ChunkBatchReceived {
                    chunks_per_tick: 2.0,
                })
                .encode(),
            )?,
            C::LevelChunkWithLight(p) => {
                let biome_bits = horizon_protocol::registry::ceil_log2(
                    self.registries.len("minecraft:worldgen/biome"),
                )
                .max(1);
                let sections =
                    p.chunk
                        .sections(self.dimension.sections(), self.block_bits, biome_bits)?;
                // Bounded storage even when a broken or hostile server never unloads.
                if self.chunks.len() >= 512 && !self.chunks.contains_key(&(p.x, p.z)) {
                    return Err(Error::new("World exceeds the browser chunk limit"));
                }
                self.chunks.insert((p.x, p.z), sections);
                self.effects.push_back(Effect::WorldChanged);
            }
            C::ForgetLevelChunk(p) => {
                self.chunks.remove(&(p.pos[0], p.pos[1]));
                self.overrides.retain(|pos, _| {
                    pos[0].div_euclid(16) != p.pos[0] || pos[2].div_euclid(16) != p.pos[1]
                });
                self.effects.push_back(Effect::WorldChanged);
            }
            C::BlockUpdate(p) => {
                if self.overrides.len() < 65536 {
                    self.overrides.insert(p.pos, p.state);
                }
                self.effects.push_back(Effect::WorldChanged);
            }
            C::SectionBlocksUpdate(p) => {
                for change in p.changes {
                    let pos = [
                        p.section[0] * 16 + ((change >> 8) & 15) as i32,
                        p.section[1] * 16 + (change & 15) as i32,
                        p.section[2] * 16 + ((change >> 4) & 15) as i32,
                    ];
                    if self.overrides.len() < 65536 {
                        self.overrides.insert(pos, (change >> 12) as i32);
                    }
                }
                self.effects.push_back(Effect::WorldChanged);
            }
            C::SystemChat(p) => self
                .effects
                .push_back(Effect::Chat(text::plain(&p.content, &Default::default()))),
            C::SetHealth(p) => {
                self.health = p.health;
                self.food = p.food;
            }
            C::ResourcePackPush(p) => self.send(
                S::ResourcePack(cfg::ResourcePackResponse {
                    id: p.id,
                    action: 1,
                })
                .encode(),
            )?,
            _ => {}
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        if self.phase == Phase::Play && self.positioned {
            if let Some(p) = self
                .sender
                .update(self.position, self.rotation, false, false)
            {
                self.send(p.encode())?;
            }
            self.send(play::Serverbound::ClientTickEnd(Empty).encode())?;
        }
        Ok(())
    }

    pub fn block(&self, p: [i32; 3]) -> i32 {
        if let Some(state) = self.overrides.get(&p) {
            return *state;
        }
        let Some(sections) = self.chunks.get(&(p[0].div_euclid(16), p[2].div_euclid(16))) else {
            return 0;
        };
        let y = p[1] - self.dimension.min_y;
        if y < 0 {
            return 0;
        }
        sections.get(y as usize / 16).map_or(0, |s| {
            s.states
                .get((((y & 15) << 8) | ((p[2] & 15) << 4) | (p[0] & 15)) as usize)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(ping: bool) -> Session {
        Session::new("localhost", 25565, "Horizon", None, ping).unwrap()
    }

    #[test]
    fn status_accepts_fragmented_tcp_frames() {
        let mut s = session(true);
        s.drain();
        let bytes = frame::encode(
            &status::Clientbound::StatusResponse(status::StatusResponse {
                json: "{\"version\":{\"protocol\":777}}".into(),
            })
            .encode(),
            None,
        )
        .unwrap();
        for b in bytes {
            s.receive(vec![b]).unwrap();
        }
        assert!(matches!(&s.drain()[0], Effect::ServerStatus(v) if v.contains("777")));
        assert_eq!(s.phase, Phase::Closed);
    }

    #[test]
    fn compression_switch_applies_to_next_packet_in_same_websocket_message() {
        let mut s = session(false);
        s.drain();
        let mut wire = frame::encode(
            &login::Clientbound::Compression(login::Compression { threshold: 1 }).encode(),
            None,
        )
        .unwrap();
        wire.extend(
            frame::encode(
                &login::Clientbound::Disconnect(login::Disconnect {
                    reason_json: "test disconnect".into(),
                })
                .encode(),
                Some(1),
            )
            .unwrap(),
        );
        s.receive(wire).unwrap();
        assert_eq!(s.phase, Phase::Closed);
        assert!(matches!(&s.drain()[0], Effect::Disconnected(v) if v == "test disconnect"));
    }

    #[test]
    fn fresh_route_has_no_world_or_cipher_from_previous_connection() {
        let mut old = session(false);
        old.overrides.insert([0, 0, 0], 12);
        old.phase = Phase::Play;
        let fresh = session(false);
        assert_eq!(old.block([0, 0, 0]), 12);
        assert_eq!(fresh.block([0, 0, 0]), 0);
        assert!(!fresh.positioned);
        assert!(fresh.encrypt.is_none());
    }

    #[test]
    fn rejects_unbounded_and_invalid_wire_data() {
        let mut s = session(false);
        assert!(s.receive(vec![0xff, 0xff, 0xff]).is_err());
        assert!(Session::new("host", 25565, "bad name", None, false).is_err());
    }

    #[test]
    fn online_login_waits_for_session_join_then_encrypts_incremental_frames() {
        let private = horizon_protocol::rsa::RsaPrivateKey::generate(512).unwrap();
        let mut s = Session::new("server", 25565, "Horizon", Some(1), false).unwrap();
        s.drain();
        let hello = login::Clientbound::Hello(login::Hello {
            server_id: String::new(),
            public_key: private.public_der(),
            challenge: vec![1, 2, 3, 4],
            should_authenticate: true,
        });
        s.receive(frame::encode(&hello.encode(), None).unwrap())
            .unwrap();
        let events = s.drain();
        assert!(matches!(&events[..], [Effect::Authenticate(_)]));
        assert!(s.encrypt.is_none());
        s.authenticated().unwrap();
        let wire = match s.drain().pop().unwrap() {
            Effect::Send(wire) => wire,
            _ => panic!("expected RSA response"),
        };
        let body = frame::split(&mut wire.clone()).unwrap().unwrap();
        let mut r = Reader::new(&body);
        let id = r.varint().unwrap();
        let login::Serverbound::Key(response) = login::Serverbound::decode(id, r.rest()).unwrap()
        else {
            panic!("expected key")
        };
        let secret: [u8; 16] = private.decrypt(&response.key).unwrap().try_into().unwrap();
        assert_eq!(
            private.decrypt(&response.challenge).unwrap(),
            vec![1, 2, 3, 4]
        );
        let mut encrypted = frame::encode(
            &login::Clientbound::Compression(login::Compression { threshold: 1 }).encode(),
            None,
        )
        .unwrap();
        encrypted.extend(
            frame::encode(
                &login::Clientbound::Disconnect(login::Disconnect {
                    reason_json: "encrypted disconnect".into(),
                })
                .encode(),
                Some(1),
            )
            .unwrap(),
        );
        Cfb8::new(&secret, &secret).encrypt(&mut encrypted);
        for b in encrypted {
            s.receive(vec![b]).unwrap();
        }
        assert_eq!(s.phase, Phase::Closed);
        assert!(
            matches!(&s.drain()[0], Effect::Disconnected(reason) if reason == "encrypted disconnect")
        );
    }
}
