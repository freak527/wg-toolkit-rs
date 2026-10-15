//! Decode a captured `(exposed id, value)` run -- an entity-creation payload's
//! `client_data`, or an `updateEntity` body -- against a chosen entity's property table,
//! and report every property it recovered plus why it stopped, if it did.
//!
//! This is what settles whether a seeding failure is a short property table or a
//! misaligned payload: a correct alignment yields strictly ascending ids and consumes the
//! bytes exactly, while being even one byte off produces a nonsense count straight away.
//!
//! usage: <res-dir> <entity> <hex> [<hex>...]

use std::env;
use std::path::Path;

use wgtk::res::fs::ResFilesystem;
use wgtk::script;
use wgtk::app::dispatch::ScriptDispatch;
use wgtk::app::client::element::UpdateEntity;
use wgtk::net::element::Element;

fn main() {
    let mut args = env::args().skip(1);
    let dir = args.next().expect("usage: <res-dir> <entity> <hex>...");
    let entity = args.next().expect("need entity name");

    let fs = ResFilesystem::new(Path::new(&dir)).unwrap();
    let dispatch = ScriptDispatch::new(script::load(&fs).unwrap());
    let (type_id, ed) = dispatch.entity_from_name(&entity).expect("entity not found");
    println!("{entity} (type 0x{type_id:02X}): {} properties\n", ed.properties.len());

    for hex in args {
        let data: Vec<u8> = (0..hex.len()).step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();

        let UpdateEntity(decoded) =
            UpdateEntity::read(&mut &data[..], &ed.properties, data.len(), 0).unwrap();

        println!("{} bytes -> {} properties{}", data.len(), decoded.values.len(),
            if decoded.is_complete() { ", complete" } else { "" });
        for (id, name, value) in &decoded.values {
            let shown = format!("{value:?}");
            let shown = if shown.len() > 110 { format!("{}...", &shown[..110]) } else { shown };
            println!("  {id:>3}  {name:<28} {shown}");
        }
        if let Some(stop) = &decoded.stopped {
            println!("  STOPPED after {} of {}: {}", decoded.values.len(), stop.expected, stop.reason);
            println!("  rest: {}", stop.rest.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        println!();
    }
}
