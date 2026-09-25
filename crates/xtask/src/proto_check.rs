//! The protobuf only-add check.
//!
//! Every amux protocol surface (the journal, the store's opaque bodies, the
//! client and peer services) is read by binaries of other versions, so a
//! shipped field may never be removed, renumbered, retyped or made required.
//! The current protos are compiled to a descriptor set and compared with a
//! committed baseline; anything the baseline has that the current schema
//! lost or changed fails with its full name. Additions pass, and
//! `--update` rewrites the baseline once a change is deliberate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message as _;
use prost_types::field_descriptor_proto::Label;
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorSet,
    ServiceDescriptorProto,
};

/// The proto files that make up the wire, relative to the proto root.
pub const PROTO_FILES: &[&str] = &[
    "amux/v1/amux.proto",
    "amux/v1/agent.proto",
    "amux/v1/claude.proto",
    "amux/v1/codex.proto",
    "amux/v1/records.proto",
];

fn wire_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under crates/")
        .join("wire")
}

/// `xtask proto-check [--update]`.
pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let update = match args {
        [] => false,
        [flag] if flag == "--update" => true,
        _ => {
            eprintln!("usage: xtask proto-check [--update]");
            std::process::exit(2);
        }
    };
    let proto_dir = wire_dir().join("proto");
    let baseline_path = proto_dir.join("baseline.binpb");
    let current = compile(&proto_dir, PROTO_FILES)?;
    if update {
        std::fs::write(&baseline_path, current.encode_to_vec())?;
        println!("rewrote {}", baseline_path.display());
        return Ok(());
    }
    let baseline = FileDescriptorSet::decode(std::fs::read(&baseline_path)?.as_slice())?;
    let violations = compare(&baseline, &current);
    if violations.is_empty() {
        println!("protos only add to {}", baseline_path.display());
        return Ok(());
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    eprintln!(
        "{} change(s) break the only-add rule; if the break is deliberate, run `just proto-check --update` and commit the baseline",
        violations.len()
    );
    std::process::exit(1);
}

/// Compiles `files` under `proto_dir` with the vendored protoc.
pub fn compile(
    proto_dir: &Path,
    files: &[&str],
) -> Result<FileDescriptorSet, Box<dyn std::error::Error>> {
    let out = tempfile_path("amux-proto-check");
    let status = Command::new(protoc_bin_vendored::protoc_bin_path()?)
        .arg("--include_imports")
        .arg(format!("--descriptor_set_out={}", out.display()))
        .arg(format!("-I{}", proto_dir.display()))
        .args(files)
        .status()?;
    if !status.success() {
        return Err(format!("protoc failed with {status}").into());
    }
    let bytes = std::fs::read(&out)?;
    let _ = std::fs::remove_file(&out);
    Ok(FileDescriptorSet::decode(bytes.as_slice())?)
}

fn tempfile_path(stem: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{stem}-{}-{}.binpb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ))
}

/// Every way `current` fails to be `baseline` plus additions, by full name.
pub fn compare(baseline: &FileDescriptorSet, current: &FileDescriptorSet) -> Vec<String> {
    let old = Index::of(baseline);
    let new = Index::of(current);
    let mut violations = Vec::new();

    for (name, message) in &old.messages {
        let Some(now) = new.messages.get(name) else {
            violations.push(format!("{name}: message removed"));
            continue;
        };
        let fields = now
            .field
            .iter()
            .map(|field| (field.name(), field))
            .collect::<BTreeMap<_, _>>();
        for field in &message.field {
            let full = format!("{name}.{}", field.name());
            let Some(current) = fields.get(field.name()) else {
                violations.push(format!("{full}: field removed"));
                continue;
            };
            if current.number() != field.number() {
                violations.push(format!(
                    "{full}: renumbered from {} to {}",
                    field.number(),
                    current.number()
                ));
            }
            if field_type(current) != field_type(field) {
                violations.push(format!(
                    "{full}: retyped from {} to {}",
                    field_type(field),
                    field_type(current)
                ));
            }
        }
    }
    for (name, message) in &new.messages {
        for field in &message.field {
            if field.label() == Label::Required {
                violations.push(format!("{name}.{}: field is required", field.name()));
            }
        }
    }

    for (name, enumeration) in &old.enums {
        let Some(now) = new.enums.get(name) else {
            violations.push(format!("{name}: enum removed"));
            continue;
        };
        let values = now
            .value
            .iter()
            .map(|value| (value.name(), value.number()))
            .collect::<BTreeMap<_, _>>();
        for value in &enumeration.value {
            match values.get(value.name()) {
                None => violations.push(format!("{name}.{}: enum value removed", value.name())),
                Some(number) if *number != value.number() => violations.push(format!(
                    "{name}.{}: renumbered from {} to {number}",
                    value.name(),
                    value.number()
                )),
                Some(_) => {}
            }
        }
    }

    for (name, service) in &old.services {
        let Some(now) = new.services.get(name) else {
            violations.push(format!("{name}: service removed"));
            continue;
        };
        let methods = now
            .method
            .iter()
            .map(|method| (method.name(), method))
            .collect::<BTreeMap<_, _>>();
        for method in &service.method {
            let full = format!("{name}.{}", method.name());
            let Some(current) = methods.get(method.name()) else {
                violations.push(format!("{full}: rpc removed"));
                continue;
            };
            let signature = |m: &prost_types::MethodDescriptorProto| {
                (
                    m.input_type().to_owned(),
                    m.output_type().to_owned(),
                    m.client_streaming(),
                    m.server_streaming(),
                )
            };
            if signature(current) != signature(method) {
                violations.push(format!("{full}: signature changed"));
            }
        }
    }
    violations
}

/// A field's wire identity: label, scalar type and message or enum name.
fn field_type(field: &FieldDescriptorProto) -> String {
    let label = if field.label() == Label::Repeated {
        "repeated "
    } else {
        ""
    };
    let kind = format!("{:?}", field.r#type()).to_lowercase();
    match field.type_name.as_deref() {
        Some(type_name) if !type_name.is_empty() => format!("{label}{kind} {type_name}"),
        _ => format!("{label}{kind}"),
    }
}

/// Every message, enum and service in a descriptor set by full name,
/// nested ones included.
#[derive(Default)]
struct Index<'a> {
    messages: BTreeMap<String, &'a DescriptorProto>,
    enums: BTreeMap<String, &'a EnumDescriptorProto>,
    services: BTreeMap<String, &'a ServiceDescriptorProto>,
}

impl<'a> Index<'a> {
    fn of(set: &'a FileDescriptorSet) -> Self {
        let mut index = Self::default();
        for file in &set.file {
            let package = file.package();
            for message in &file.message_type {
                index.message(package, message);
            }
            for enumeration in &file.enum_type {
                index
                    .enums
                    .insert(format!("{package}.{}", enumeration.name()), enumeration);
            }
            for service in &file.service {
                index
                    .services
                    .insert(format!("{package}.{}", service.name()), service);
            }
        }
        index
    }

    fn message(&mut self, scope: &str, message: &'a DescriptorProto) {
        let name = format!("{scope}.{}", message.name());
        // Map entries are generated by protoc and change only with their map.
        if message
            .options
            .as_ref()
            .is_some_and(|options| options.map_entry())
        {
            return;
        }
        for nested in &message.nested_type {
            self.message(&name, nested);
        }
        for enumeration in &message.enum_type {
            self.enums
                .insert(format!("{name}.{}", enumeration.name()), enumeration);
        }
        self.messages.insert(name, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile_source(source: &str) -> FileDescriptorSet {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("test.proto"), source).expect("write proto");
        compile(dir.path(), &["test.proto"]).expect("compile")
    }

    const BASE: &str = r#"
        syntax = "proto3";
        package t;
        message Item { string key = 1; uint64 revision = 2; repeated string tags = 3; }
        enum Phase { STARTING = 0; IDLE = 1; }
        service S { rpc Get(Item) returns (Item); }
    "#;

    #[test]
    fn the_same_schema_passes() {
        assert_eq!(
            compare(&compile_source(BASE), &compile_source(BASE)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn adding_fields_values_messages_and_rpcs_passes() {
        let added = r#"
            syntax = "proto3";
            package t;
            message Item { string key = 1; uint64 revision = 2; repeated string tags = 3; bytes body = 4; }
            message Extra { string note = 1; }
            enum Phase { STARTING = 0; IDLE = 1; WORKING = 2; }
            service S { rpc Get(Item) returns (Item); rpc Put(Item) returns (Extra); }
        "#;
        assert_eq!(
            compare(&compile_source(BASE), &compile_source(added)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn removing_a_field_fails_with_its_name() {
        let removed = r#"
            syntax = "proto3";
            package t;
            message Item { string key = 1; repeated string tags = 3; }
            enum Phase { STARTING = 0; IDLE = 1; }
            service S { rpc Get(Item) returns (Item); }
        "#;
        assert_eq!(
            compare(&compile_source(BASE), &compile_source(removed)),
            vec!["t.Item.revision: field removed".to_owned()]
        );
    }

    #[test]
    fn renumbering_retyping_and_removals_elsewhere_fail() {
        let changed = r#"
            syntax = "proto3";
            package t;
            message Item { string key = 5; int64 revision = 2; string tags = 3; }
            enum Phase { STARTING = 0; }
            service S { }
        "#;
        assert_eq!(
            compare(&compile_source(BASE), &compile_source(changed)),
            vec![
                "t.Item.key: renumbered from 1 to 5".to_owned(),
                "t.Item.revision: retyped from uint64 to int64".to_owned(),
                "t.Item.tags: retyped from repeated string to string".to_owned(),
                "t.Phase.IDLE: enum value removed".to_owned(),
                "t.S.Get: rpc removed".to_owned(),
            ]
        );
    }

    #[test]
    fn a_required_field_fails() {
        let base =
            compile_source("syntax = \"proto2\"; package t; message M { optional string a = 1; }");
        let required =
            compile_source("syntax = \"proto2\"; package t; message M { required string a = 1; }");
        assert_eq!(
            compare(&base, &required),
            vec!["t.M.a: field is required".to_owned()]
        );
    }

    #[test]
    fn the_committed_baseline_matches_the_current_protos() {
        let proto_dir = wire_dir().join("proto");
        let baseline = FileDescriptorSet::decode(
            std::fs::read(proto_dir.join("baseline.binpb"))
                .expect("baseline")
                .as_slice(),
        )
        .expect("decode baseline");
        let current = compile(&proto_dir, PROTO_FILES).expect("compile wire");
        assert_eq!(compare(&baseline, &current), Vec::<String>::new());
    }
}
