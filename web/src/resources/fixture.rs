//! Original synthetic resources, matching examples/fixture_server.rs. No
//! Minecraft assets or derived game exports are part of this fixture.
use serde_json::json;
use std::io::{Cursor, Write};

pub fn bundle() -> Vec<u8> {
    let mut files = Vec::new();
    files.push((
        "horizon-resources.json".to_owned(),
        json!({"data_version":horizon_world::DATA_VERSION,"web_name":"Horizon original demo"})
            .to_string()
            .into_bytes(),
    ));
    let names = [
        "air", "stone", "brick", "slab", "cave_air", "leaves", "glass", "steps",
    ];
    let states: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            json!([
                format!("horizon:{n}"),
                i == 0 || i == 4,
                false,
                "fixture",
                format!("horizon:{n}")
            ])
        })
        .collect();
    let defaults: serde_json::Map<_, _> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (format!("horizon:{n}"), json!(i)))
        .collect();
    files.push((
        "facts.json".into(),
        json!({"states":states,"defaults":defaults})
            .to_string()
            .into_bytes(),
    ));
    let schemas: serde_json::Map<_, _> = names
        .iter()
        .map(|n| (format!("horizon:{n}"), json!({"schema":{},"states":[{}]})))
        .collect();
    files.push((
        "model-states.json".into(),
        serde_json::Value::Object(schemas).to_string().into_bytes(),
    ));
    for (i, name) in names.iter().enumerate() {
        if i == 0 || i == 4 {
            continue;
        }
        let definition = if *name == "steps" {
            json!({"variants":{"":{"model":"horizon:block/steps","y":90,"uvlock":true}}})
        } else {
            json!({"variants":{"":{"model":format!("horizon:block/{name}")}}})
        };
        files.push((
            format!("assets/horizon/blockstates/{name}.json"),
            definition.to_string().into_bytes(),
        ));
        let model = if *name == "slab" {
            json!({"textures":{"all":"horizon:block/stone"},"elements":[element([0,0,0],[16,8,16])]})
        } else if *name == "steps" {
            json!({"textures":{"all":"horizon:block/brick"},"elements":[element([0,0,0],[16,8,16]),element([0,8,8],[16,16,16])]})
        } else {
            json!({"parent":"horizon:block/cube","textures":{"all":format!("horizon:block/{name}")}})
        };
        files.push((
            format!("assets/horizon/models/block/{name}.json"),
            model.to_string().into_bytes(),
        ));
        let color = match *name {
            "brick" => [180, 76, 43],
            "leaves" => [55, 150, 76],
            "glass" => [65, 175, 210],
            _ => [145, 150, 162],
        };
        files.push((
            format!("assets/horizon/textures/block/{name}.png"),
            texture(color, name),
        ));
    }
    files.push((
        "assets/horizon/models/block/cube.json".into(),
        json!({"textures":{"all":"horizon:block/stone"},"elements":[element([0,0,0],[16,16,16])]})
            .to_string()
            .into_bytes(),
    ));
    zip(files)
}

fn element(from: [i32; 3], to: [i32; 3]) -> serde_json::Value {
    let faces: serde_json::Map<_, _> = ["up", "down", "north", "south", "east", "west"]
        .iter()
        .map(|d| (d.to_string(), json!({"texture":"#all","cullface":d})))
        .collect();
    json!({"from":from,"to":to,"faces":faces})
}

fn texture(color: [u8; 3], name: &str) -> Vec<u8> {
    let mut bytes = vec![];
    for y in 0..16 {
        for x in 0..16 {
            let line = if name == "brick" {
                y % 5 == 0 || (x + (y / 5 % 2) * 4) % 8 == 0
            } else {
                (x / 4 + y / 4) % 2 == 0
            };
            let rgb = color.map(|v| if line { v.saturating_sub(35) } else { v });
            let alpha = match name {
                "leaves" if x % 4 == 0 && y % 4 == 0 => 0,
                "glass" if x != 0 && y != 0 => 110,
                _ => 255,
            };
            bytes.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }
    let mut out = vec![];
    {
        let mut encoder = png::Encoder::new(&mut out, 16, 16);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&bytes)
            .unwrap();
    }
    out
}

pub fn zip(files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
