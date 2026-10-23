//! Browser resource adapter over the native model decoders/baker. No game
//! data is bundled: the user imports an exported, version-checked local ZIP.
use horizon_assets::{
    animation::AnimationMetadata,
    bake::{BlockModelTransform, ModelQuad, bake_block_model},
    blockstate::{
        BlockStateDefinition, Variant, VariantSet,
        bind::{Properties, PropertySchema, PropertyValues, SelectedModels},
        choice::CompiledChoices,
    },
    model::CuboidModel,
    resolve::ModelLibrary,
};
use horizon_core::{
    geometry::Direction,
    position::BlockPos,
    random::{LegacyRandom, position_seed},
};
use horizon_world::{
    block_render::{FaceVisibility, RenderDefinition, SkipRendering},
    collision::CachedCollisionRegistry,
    registry::{BlockRegistry, BlockStateId, BlockTableFile},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

pub const MAX_ARCHIVE: usize = 128 << 20;
const MAX_EXPANDED: u64 = 256 << 20;
const TILE: u32 = 16;
type Files = BTreeMap<String, Vec<u8>>;
type Key = (String, [u8; 3], bool);
type Result<T> = std::result::Result<T, String>;

pub struct Atlas {
    pub side: u32,
    pub columns: u32,
    pub pixels: Vec<u8>,
}

impl Atlas {
    pub fn fallback() -> Self {
        Self {
            side: TILE,
            columns: 1,
            pixels: vec![255; (TILE * TILE * 4) as usize],
        }
    }
}

pub struct Face {
    pub quad: ModelQuad,
    pub tile: u32,
    pub translucent: bool,
}
struct Model {
    faces: Vec<Face>,
    full: [bool; 6],
}

pub struct Resources {
    pub name: String,
    pub registry: BlockRegistry,
    pub atlas: Atlas,
    flags: Vec<(bool, bool)>,
    bindings: Vec<Option<(usize, SelectedModels)>>,
    choices: Vec<CompiledChoices>,
    models: BTreeMap<Key, Model>,
    visibility: Option<RefCell<FaceVisibility>>,
    fallback_occluders: Vec<[bool; 6]>,
}

fn key(v: &Variant) -> Key {
    (v.model.clone(), v.rotations, v.uv_lock)
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn json(files: &Files, name: &str) -> Result<Value> {
    serde_json::from_slice(
        files
            .get(name)
            .ok_or_else(|| format!("Resource bundle is missing {name}."))?,
    )
    .map_err(error)
}

pub fn allowed_path(name: &str) -> bool {
    if name.contains('\\')
        || name.starts_with('/')
        || name.split('/').any(|p| p == ".." || p.is_empty())
    {
        return false;
    }
    matches!(
        name,
        "horizon-resources.json"
            | "facts.json"
            | "model-states.json"
            | "occlusion.nbt"
            | "render-definitions.json"
    ) || (name.starts_with("assets/")
        && (name.contains("/models/")
            || name.contains("/blockstates/")
            || name.contains("/textures/"))
        && (name.ends_with(".json") || name.ends_with(".png") || name.ends_with(".png.mcmeta")))
}

fn read_archive(bytes: &[u8]) -> Result<Files> {
    if bytes.len() > MAX_ARCHIVE {
        return Err("The resource bundle exceeds 128 MiB.".into());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(error)?;
    if zip.len() > 25000 {
        return Err("The resource bundle contains too many entries.".into());
    }
    let mut files = BTreeMap::new();
    let mut expanded = 0u64;
    for i in 0..zip.len() {
        let file = zip.by_index(i).map_err(error)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_owned();
        if !allowed_path(&name) {
            return Err(format!("Unsupported resource path: {name}"));
        }
        expanded = expanded
            .checked_add(file.size())
            .ok_or("Resource size overflow.")?;
        if expanded > MAX_EXPANDED || file.size() > 32 << 20 {
            return Err("Expanded resource bundle exceeds its size limit.".into());
        }
        let expected = file.size();
        let mut data = Vec::new();
        file.take(expected + 1)
            .read_to_end(&mut data)
            .map_err(error)?;
        if data.len() as u64 != expected {
            return Err("A resource entry has an invalid length.".into());
        }
        if files.insert(name, data).is_some() {
            return Err("The resource bundle contains duplicate paths.".into());
        }
    }
    Ok(files)
}

impl Resources {
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let files = read_archive(bytes)?;
        let manifest = json(&files, "horizon-resources.json")?;
        if manifest["data_version"].as_i64() != Some(horizon_world::DATA_VERSION as i64) {
            return Err(format!(
                "This build requires Minecraft {} resources (data version {}).",
                horizon_core::VANILLA_VERSION,
                horizon_world::DATA_VERSION
            ));
        }
        let (registry, flags) = BlockRegistry::from_facts_json_with_flags(Cursor::new(
            files.get("facts.json").ok_or("Missing block facts.")?,
        ))
        .map_err(error)?;
        if registry.len() > 65536 {
            return Err("This browser build supports at most 65,536 block states.".into());
        }
        let facts = json(&files, "model-states.json")?;
        let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for i in 0..registry.len() {
            by_name
                .entry(&registry.state(BlockStateId(i as u32)).unwrap().name)
                .or_default()
                .push(i);
        }
        let mut bindings = vec![None; registry.len()];
        let mut choices = vec![];
        let mut variants = BTreeSet::new();
        let mut block_variants = vec![];
        // Same binding/order validation as horizon-client/models/catalog.rs;
        // the platform adapter supplies bytes rather than filesystem paths.
        for (name, ids) in &by_name {
            if ids.iter().all(|i| flags[*i].0 || flags[*i].1) {
                continue;
            }
            let (ns, path) = name.split_once(':').ok_or("Invalid block identifier.")?;
            let data = files
                .get(&format!("assets/{ns}/blockstates/{path}.json"))
                .ok_or_else(|| format!("Missing blockstate resource: {name}"))?;
            let definition =
                BlockStateDefinition::from_json(std::str::from_utf8(data).map_err(error)?)
                    .map_err(error)?;
            let fact = &facts[*name];
            let schema = schema(&fact["schema"])?;
            let possible: Vec<Properties> =
                serde_json::from_value(fact["states"].clone()).map_err(error)?;
            if possible.len() != ids.len()
                || ids.iter().zip(&possible).any(|(id, props)| {
                    &registry.state(BlockStateId(*id as u32)).unwrap().properties != props
                })
            {
                return Err(format!(
                    "Model schema disagrees with the block registry: {name}"
                ));
            }
            for (id, bound) in ids
                .iter()
                .zip(definition.bind(&schema, &possible).map_err(error)?)
            {
                if let Some(bound) = bound {
                    bindings[*id] = Some((choices.len(), bound));
                }
            }
            let mut block_keys = BTreeSet::new();
            for set in definition
                .variants
                .iter()
                .flatten()
                .map(|(_, v)| v)
                .chain(definition.multipart.iter().flatten().map(|v| &v.variant))
            {
                match set {
                    VariantSet::Single(v) => {
                        block_keys.insert(key(v));
                    }
                    VariantSet::Weighted(v) => block_keys.extend(v.iter().map(|(v, _)| key(v))),
                }
            }
            variants.extend(block_keys.iter().cloned());
            block_variants.push((ids.clone(), block_keys));
            choices.push(CompiledChoices::new(&definition).map_err(error)?);
        }
        let mut decoded = BTreeMap::new();
        let mut pending: Vec<_> = variants.iter().map(|(name, _, _)| name.clone()).collect();
        while let Some(name) = pending.pop() {
            if decoded.contains_key(&name) {
                continue;
            }
            let (ns, path) = name.split_once(':').ok_or("Invalid model identifier.")?;
            let bytes = files
                .get(&format!("assets/{ns}/models/{path}.json"))
                .ok_or_else(|| format!("Missing model: {name}"))?;
            let model = CuboidModel::from_json(std::str::from_utf8(bytes).map_err(error)?)
                .map_err(error)?;
            if let Some(parent) = &model.parent {
                pending.push(parent.clone());
            }
            decoded.insert(name, model);
            if decoded.len() > 32768 {
                return Err("The resource bundle contains too many models.".into());
            }
        }
        let library = ModelLibrary::new(decoded).map_err(error)?;
        let mut sprites = Sprites::new();
        let mut models = BTreeMap::new();
        let mut quads = 0;
        for (name, turns, uv_lock) in variants {
            let resolved = library.resolve(&name).map_err(error)?;
            let baked = bake_block_model(&resolved, BlockModelTransform::new(turns, uv_lock))
                .map_err(error)?;
            quads += baked.len();
            if quads > 1_000_000 {
                return Err("The resource bundle contains too much model geometry.".into());
            }
            let mut full = [false; 6];
            let mut faces = vec![];
            for quad in baked {
                let tile = sprites.insert(&files, &quad.material.sprite)?;
                let translucent = quad.material.force_translucent || sprites.partial[tile as usize];
                if let Some(cull) = quad.cull {
                    full[cull as usize] |= sprites.opaque[tile as usize] && full_face(&quad, cull);
                }
                faces.push(Face {
                    quad,
                    tile,
                    translucent,
                });
            }
            models.insert((name, turns, uv_lock), Model { faces, full });
        }
        let visibility = load_visibility(&files, &registry)?.map(RefCell::new);
        let mut out = Self {
            name: manifest["web_name"]
                .as_str()
                .unwrap_or("Minecraft resources")
                .to_owned(),
            registry,
            atlas: sprites.finish(),
            flags,
            bindings,
            choices,
            models,
            visibility,
            fallback_occluders: vec![],
        };
        // Without prepared occlusion data, only certainly-full opaque model
        // faces occlude. Partial blocks are conservatively kept visible.
        out.fallback_occluders = vec![[false; 6]; out.registry.len()];
        for (ids, keys) in block_variants {
            let mask = std::array::from_fn(|face| {
                !keys.is_empty() && keys.iter().all(|key| out.models[key].full[face])
            });
            for id in ids {
                out.fallback_occluders[id] = mask;
            }
        }
        Ok(out)
    }

    fn models_at(&self, state: i32, position: BlockPos) -> Vec<&Model> {
        let Some(Some((index, bound))) = usize::try_from(state)
            .ok()
            .and_then(|id| self.bindings.get(id))
        else {
            return vec![];
        };
        let mut selected = vec![];
        if self.choices[*index]
            .collect(
                bound,
                &mut LegacyRandom::new(position_seed(position)),
                &mut selected,
            )
            .is_err()
        {
            return vec![];
        }
        selected
            .into_iter()
            .filter_map(|v| self.models.get(&key(v)))
            .collect()
    }

    pub fn faces(&self, state: i32, position: BlockPos) -> Vec<&Face> {
        self.models_at(state, position)
            .into_iter()
            .flat_map(|m| m.faces.iter())
            .collect()
    }
    pub fn is_air(&self, state: i32) -> bool {
        usize::try_from(state)
            .ok()
            .and_then(|id| self.flags.get(id))
            .is_none_or(|v| v.0)
    }
    pub fn visible(&self, state: i32, neighbor: i32, direction: Direction) -> bool {
        if self.is_air(neighbor) {
            return true;
        }
        if let Some(visibility) = &self.visibility {
            return visibility
                .borrow_mut()
                .should_render_face(
                    BlockStateId(state as u32),
                    BlockStateId(neighbor as u32),
                    direction,
                    true,
                )
                .unwrap_or(true);
        }
        !self
            .fallback_occluders
            .get(neighbor as usize)
            .is_some_and(|faces| faces[direction.opposite() as usize])
    }
}

fn schema(value: &Value) -> Result<PropertySchema> {
    value
        .as_object()
        .ok_or("Missing native model property schema.")?
        .iter()
        .map(|(name, v)| {
            let property = if let Some(range) = v.get("integer") {
                PropertyValues::Integer {
                    minimum: range[0]
                        .as_i64()
                        .and_then(|v| v.try_into().ok())
                        .ok_or("Invalid integer property.")?,
                    maximum: range[1]
                        .as_i64()
                        .and_then(|v| v.try_into().ok())
                        .ok_or("Invalid integer property.")?,
                }
            } else {
                PropertyValues::Named(
                    v["named"]
                        .as_array()
                        .ok_or("Invalid named property.")?
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| "Invalid property value.".to_owned())
                        })
                        .collect::<Result<_>>()?,
                )
            };
            Ok((name.clone(), property))
        })
        .collect()
}

fn load_visibility(files: &Files, registry: &BlockRegistry) -> Result<Option<FaceVisibility>> {
    let Some(bytes) = files.get("occlusion.nbt") else {
        return Ok(None);
    };
    let file = BlockTableFile::read(bytes, registry).map_err(error)?;
    let occlusion = CachedCollisionRegistry::from_table(file.table("occlusion").map_err(error)?)
        .map_err(error)?;
    let values = json(files, "render-definitions.json")?;
    let definitions = values
        .as_object()
        .ok_or("Invalid render definitions.")?
        .iter()
        .map(|(name, v)| {
            let skip = match v["rule"].as_str() {
                Some("never") => SkipRendering::Never,
                Some("same_block") => SkipRendering::SameBlock,
                Some("bars") => SkipRendering::Bars,
                Some("leaves") => SkipRendering::Leaves,
                Some("liquid") => SkipRendering::Liquid,
                Some("mangrove_roots") => SkipRendering::MangroveRoots,
                _ => return Err("Unknown native rendering rule.".to_owned()),
            };
            Ok((
                name.clone(),
                RenderDefinition {
                    skip,
                    can_occlude: v["can_occlude"]
                        .as_bool()
                        .ok_or("Missing occlusion flag.")?,
                    bars_tag: v["bars_tag"].as_bool().ok_or("Missing bars flag.")?,
                },
            ))
        })
        .collect::<Result<_>>()?;
    FaceVisibility::new(&occlusion, &definitions)
        .map(Some)
        .map_err(error)
}

fn full_face(quad: &ModelQuad, direction: horizon_assets::model::Direction) -> bool {
    let axis = match direction {
        horizon_assets::model::Direction::Down | horizon_assets::model::Direction::Up => 1,
        horizon_assets::model::Direction::North | horizon_assets::model::Direction::South => 2,
        _ => 0,
    };
    let boundary = if matches!(
        direction,
        horizon_assets::model::Direction::Up
            | horizon_assets::model::Direction::South
            | horizon_assets::model::Direction::East
    ) {
        1.0
    } else {
        0.0
    };
    let axes: Vec<_> = (0..3).filter(|a| *a != axis).collect();
    quad.positions
        .iter()
        .all(|p| p[axis] == boundary && axes.iter().all(|a| p[*a] == 0.0 || p[*a] == 1.0))
        && quad
            .positions
            .iter()
            .map(|p| (p[axes[0]] as i32, p[axes[1]] as i32))
            .collect::<BTreeSet<_>>()
            .len()
            == 4
}

struct Sprites {
    names: BTreeMap<String, u32>,
    frames: Vec<Vec<u8>>,
    opaque: Vec<bool>,
    partial: Vec<bool>,
}
impl Sprites {
    fn new() -> Self {
        Self {
            names: BTreeMap::new(),
            frames: vec![vec![255; (TILE * TILE * 4) as usize]],
            opaque: vec![true],
            partial: vec![false],
        }
    }
    fn insert(&mut self, files: &Files, name: &str) -> Result<u32> {
        if let Some(id) = self.names.get(name) {
            return Ok(*id);
        }
        if self.frames.len() >= 4096 {
            return Err("The browser atlas supports at most 4,095 sprites.".into());
        }
        let (ns, path) = name.split_once(':').ok_or("Invalid sprite identifier.")?;
        let path = format!("assets/{ns}/textures/{path}.png");
        let source = files
            .get(&path)
            .ok_or_else(|| format!("Missing texture: {name}"))?;
        let mut decoder = png::Decoder::new(Cursor::new(source));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(error)?;
        let size = reader
            .output_buffer_size()
            .filter(|s| *s <= 64 << 20)
            .ok_or("Texture exceeds decoder limits.")?;
        let mut bytes = vec![0; size];
        let info = reader.next_frame(&mut bytes).map_err(error)?;
        let metadata = files
            .get(&format!("{path}.mcmeta"))
            .map(|b| serde_json::from_slice::<Value>(b).map_err(error))
            .transpose()?;
        let animation = metadata
            .as_ref()
            .and_then(|v| v.get("animation"))
            .map(|v| AnimationMetadata::from_json(&v.to_string()).map_err(error))
            .transpose()?;
        let [w, h] = animation
            .as_ref()
            .map(|a| a.frame_size(info.width as i32, info.height as i32))
            .unwrap_or([info.width as i32, info.height as i32]);
        let (w, h) = (
            u32::try_from(w).map_err(error)?,
            u32::try_from(h).map_err(error)?,
        );
        if w == 0 || h == 0 || info.width % w != 0 || info.height % h != 0 {
            return Err("Texture has invalid animation frame dimensions.".into());
        }
        let count = (info.width / w) * (info.height / h);
        let initial = animation
            .as_ref()
            .map(|a| a.clip(count).map_err(error))
            .transpose()?
            .flatten()
            .map_or(0, |a| a.sample(0).frame);
        let x0 = (initial % (info.width / w)) * w;
        let y0 = (initial / (info.width / w)) * h;
        let channels = info.color_type.samples();
        let mut frame = vec![];
        for y in 0..TILE {
            for x in 0..TILE {
                let at =
                    (((y0 + y * h / TILE) * info.width + x0 + x * w / TILE) as usize) * channels;
                let p = &bytes[at..at + channels];
                let rgba = match info.color_type {
                    png::ColorType::Rgba => [p[0], p[1], p[2], p[3]],
                    png::ColorType::Rgb => [p[0], p[1], p[2], 255],
                    png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
                    png::ColorType::GrayscaleAlpha => [p[0], p[0], p[0], p[1]],
                    _ => return Err("Unsupported expanded texture format.".into()),
                };
                frame.extend_from_slice(&rgba);
            }
        }
        let id = self.frames.len() as u32;
        self.opaque
            .push(frame.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        self.partial.push(
            frame
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[3] < 255),
        );
        self.names.insert(name.to_owned(), id);
        self.frames.push(frame);
        Ok(id)
    }
    fn finish(self) -> Atlas {
        let columns = (self.frames.len() as u32)
            .isqrt()
            .next_power_of_two()
            .max(1);
        let columns = if columns * columns < self.frames.len() as u32 {
            columns * 2
        } else {
            columns
        };
        let side = columns * TILE;
        let mut pixels = vec![0; (side * side * 4) as usize];
        for (i, frame) in self.frames.iter().enumerate() {
            let x = i as u32 % columns * TILE;
            let y = i as u32 / columns * TILE;
            for row in 0..TILE {
                let at = (((y + row) * side + x) * 4) as usize;
                let start = (row * TILE * 4) as usize;
                pixels[at..at + (TILE * 4) as usize]
                    .copy_from_slice(&frame[start..start + (TILE * 4) as usize]);
            }
        }
        Atlas {
            side,
            columns,
            pixels,
        }
    }
}

pub fn direction(d: horizon_assets::model::Direction) -> Direction {
    Direction::ALL[d as usize]
}

pub mod fixture;

#[cfg(test)]
mod tests;
