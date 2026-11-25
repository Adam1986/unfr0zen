//! Developer harness for the same in-memory builder recovery uses.
//! cargo run -p unfr0zen-core --bin nlclient -- STOCK OBJECT OUTPUT
//! --report STOCK prints what discovery finds, and any objection to the build.
//! --golden STOCK OBJECT TOOLCHAIN writes the sample's golden under
//! platform/common/nlclient/tests/golden, for an intentional screen revision.
use std::{io::Read, path::PathBuf};
use unfr0zen_core::nlclient::{builder, Patch};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Host-only real-image audit; never writes an executable or journal to disk.
    if args.len() == 5 && args[0] == "--audit-supply" {
        let source = std::fs::read(&args[1])?;
        let sha = unfr0zen_core::shell::sha256_hex(&source);
        if sha != "d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52" {
            return Err("audit requires the reviewed stock firmware (not a patched executable)".into());
        }
        let sections = &unfr0zen_core::nlclient::menu::Section::ALL;
        let mut reports = Vec::new();
        for (mode, arg) in ["normal", "dry-run", "isolated-write"].into_iter().zip(&args[2..]) {
            let object = std::fs::read(arg)?;
            let has = |name: &[u8]| object.windows(name.len()).any(|w| w == name);
            let heat = has(b"\0native_ot_heat_get\0");
            let get = has(b"\0native_ot_supply_get\0");
            let set = has(b"\0native_ot_supply_set\0");
            if (heat, get, set) != (mode != "normal", mode != "normal", mode == "isolated-write") {
                return Err(format!("{mode}: unexpected experimental symbols").into());
            }
            let built = builder::build(&source, &object, sections)?;
            let patch = Patch::parse(&built.journal)?;
            if !patch.reproduces(&source, &built.output)
                || patch.apply(&source, false)? != built.output
                || patch.apply(&built.output, true)? != source
            {
                return Err(format!("{mode}: patch roundtrip failed").into());
            }
            let symbols = &built.manifest["symbols"];
            if mode != "normal" && (symbols["native_ot_heat_get"].as_u64() != Some(0xa73ac)
                || symbols["native_ot_supply_get"].as_u64() != Some(0xa73e8)
                || symbols["native_ot_supply_set"].as_u64() != Some(0xbaab8))
            {
                return Err(format!("{mode}: unexpected native bindings").into());
            }
            if mode != "normal" {
                let mut modified = source.clone();
                let last = modified.len() - 1;
                modified[last] ^= 1;
                if builder::build(&modified, &object, sections).is_ok() {
                    return Err(format!("{mode}: changed stock unexpectedly accepted").into());
                }
            }
            reports.push(serde_json::json!({
                "mode":mode, "output_sha256":patch.output_sha, "output_size":built.output.len(),
                "payload_size":built.manifest["payload_size"], "payload_address":built.manifest["payload_address"],
                "device_commit":symbols["custom_device_commit"], "setter_reference":set,
                "rollback":"byte-identical", "altered_stock_rejected":mode != "normal"
            }));
        }
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "stock_sha256":sha,"results":reports,
            "scope":"static build and rollback only; no native execution or boiler test"
        }))?);
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--report" {
        let source = std::fs::read(&args[1])?;
        let (found, objection) = unfr0zen_core::nlclient::discovery::survey(&source)?;
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"discovery": found, "objection": objection}))?);
        return Ok(());
    }
    if args.len() == 4 && args[0] == "--golden" {
        let source = std::fs::read(&args[1])?;
        let object = std::fs::read(&args[2])?;
        let found = builder::build(&source, &object, &unfr0zen_core::nlclient::menu::Section::ALL)?;
        let manifest = &found.manifest;
        let offset = manifest["payload_offset"].as_u64().unwrap() as usize;
        let size = manifest["payload_size"].as_u64().unwrap() as usize;
        let mut journal = Vec::new();
        flate2::read::GzDecoder::new(found.journal.as_slice()).read_to_end(&mut journal)?;
        let patch: serde_json::Map<String, serde_json::Value> = [
            "factories",
            "image",
            "original_program_headers",
            "payload_address",
            "payload_offset",
            "program_header_offset",
            "runtime_directory",
            "settings_menu",
            "source_size",
        ]
        .into_iter()
        .map(|key| (key.to_owned(), manifest[key].clone()))
        .collect();
        let golden = serde_json::json!({
            "discovery": manifest["discovery"],
            "native_api": manifest["native_api"],
            "output": {"journal_sha256": unfr0zen_core::shell::sha256_hex(&journal),
                       "sha256": unfr0zen_core::shell::sha256_hex(&found.output), "size": found.output.len()},
            "patch": patch,
            "payload": {"entry": manifest["entry"], "sha256": unfr0zen_core::shell::sha256_hex(&found.output[offset..offset + size]),
                        "size": size, "symbols": manifest["symbols"]},
            "toolchain": args[3].to_string_lossy(),
        });
        let path = PathBuf::from(env!("UNFR0ZEN_ROOT"))
            .join("platform/common/nlclient/tests/golden")
            .join(format!("{}.json", unfr0zen_core::shell::sha256_hex(&source)));
        std::fs::write(&path, serde_json::to_vec_pretty(&golden)?)?;
        println!("{}", path.display());
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: nlclient STOCK OBJECT OUTPUT | --report STOCK | --golden STOCK OBJECT TOOLCHAIN | --audit-supply STOCK NORMAL DRY WRITE".into());
    }
    let source = std::fs::read(&args[0])?;
    let object = std::fs::read(&args[1])?;
    let found = builder::build(&source, &object, &unfr0zen_core::nlclient::menu::Section::ALL)?;
    let out = PathBuf::from(&args[2]);
    std::fs::write(&out, &found.output)?;
    std::fs::write(out.with_extension("json"), serde_json::to_vec_pretty(&found.manifest)?)?;
    std::fs::write(out.with_extension("undo.gz"), &found.journal)?;
    let patch = Patch::parse(&found.journal)?;
    let mut journal = Vec::new();
    flate2::read::GzDecoder::new(found.journal.as_slice()).read_to_end(&mut journal)?;
    println!(
        "{} -> {} ({} bytes); journal {}",
        patch.stock_sha,
        patch.output_sha,
        found.output.len(),
        unfr0zen_core::shell::sha256_hex(&journal)
    );
    Ok(())
}
