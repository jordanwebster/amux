pub(crate) mod amux {
    pub(crate) mod v1 {
        #![allow(dead_code)]
        include!(concat!(env!("OUT_DIR"), "/amux.v1.rs"));
    }
}

pub(crate) use amux::v1::*;

#[cfg(test)]
#[allow(dead_code)]
pub(crate) const DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/amux.v1.bin"));

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::DESCRIPTOR_SET;

    #[test]
    fn descriptor_set_contains_core_protocol_messages() {
        let descriptor = prost_types::FileDescriptorSet::decode(DESCRIPTOR_SET)
            .expect("descriptor set should decode");
        let file = descriptor
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("amux.v1"))
            .expect("amux.v1 descriptor should be present");

        for message_name in ["ConnectRequest", "ConnectResponse", "TransportMessage"] {
            assert!(
                file.message_type
                    .iter()
                    .any(|message| message.name.as_deref() == Some(message_name)),
                "{message_name} should be in the descriptor"
            );
        }
    }
}
