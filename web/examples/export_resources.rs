//! Package the user's prepared Horizon resources for private browser import.
//! No downloading, Java execution, or publication is performed.
use horizon_web::resources::{MAX_ARCHIVE, Resources, allowed_path};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: cargo run --example export_resources -- <prepared-resource-folder|--fixture> <output.horizon.zip>".into());
    }
    let output = PathBuf::from(&args[1]);
    if !output.to_string_lossy().ends_with(".horizon.zip") {
        return Err("Use the .horizon.zip extension so resource bundles stay Git-ignored.".into());
    }
    let parent = fs::canonicalize(
        output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let public = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("public"))?;
    if parent.starts_with(public) {
        return Err(
            "Keep resource bundles outside web/public; they are for local import only.".into(),
        );
    }
    if output.exists() {
        return Err("The output file already exists. Choose a new filename.".into());
    }
    if args[0] == "--fixture" {
        let bytes = horizon_web::resources::fixture::bundle();
        fs::write(&output, bytes)?;
    } else {
        let root = fs::canonicalize(&args[0])?;
        let mut names: Vec<String> = [
            "horizon-resources.json",
            "facts.json",
            "model-states.json",
            "occlusion.nbt",
            "render-definitions.json",
        ]
        .iter()
        .filter(|n| root.join(n).is_file())
        .map(|s| s.to_string())
        .collect();
        for namespace in fs::read_dir(root.join("assets"))? {
            let namespace = namespace?;
            if !namespace.file_type()?.is_dir() {
                continue;
            }
            for directory in ["models", "blockstates", "textures"] {
                walk(&root, &namespace.path().join(directory), &mut names)?;
            }
        }
        names.sort();
        names.dedup();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let mut total = 0u64;
        for name in names {
            let path = root.join(&name);
            let meta = fs::symlink_metadata(&path)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("Only regular resource files can be exported.".into());
            }
            total += meta.len();
            if total > 256 << 20 || meta.len() > 32 << 20 {
                return Err("The resources exceed the browser bundle size limit.".into());
            }
            zip.start_file(
                &name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )?;
            let mut data = vec![];
            fs::File::open(path)?
                .take(meta.len() + 1)
                .read_to_end(&mut data)?;
            zip.write_all(&data)?;
        }
        let bytes = zip.finish()?.into_inner();
        if bytes.len() > MAX_ARCHIVE {
            return Err("The compressed resource bundle exceeds 128 MiB.".into());
        }
        Resources::load(&bytes)?;
        fs::write(&output, bytes)?;
    }
    println!(
        "Wrote {}. Import it under Resources in the browser; keep game assets private.",
        output.display()
    );
    Ok(())
}

fn walk(root: &Path, directory: &Path, names: &mut Vec<String>) -> std::io::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(root, &entry.path(), names)?;
        } else if kind.is_file() {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if allowed_path(&name) {
                names.push(name);
            }
        }
    }
    Ok(())
}
