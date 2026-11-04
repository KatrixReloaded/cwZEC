use anyhow::{Result, bail};
use orchard::Address;
use zcash_address::unified::{self, Container, Encoding};
use zcash_protocol::consensus::NetworkType;

pub fn encode_address(receiver: Address) -> Result<String> {
    Ok(unified::Address::try_from_items(
        unified::Revision::R0,
        vec![unified::Uitem::Data(unified::Receiver::Orchard(
            receiver.to_raw_address_bytes(),
        ))],
    )?
    .encode(&NetworkType::Test))
}

pub fn decode_address(encoded: &str) -> Result<Address> {
    let (network, _, address) = unified::Address::decode(encoded)?;
    if network != NetworkType::Test {
        bail!("destination must be a testnet unified address");
    }
    for item in address.items_as_parsed() {
        if let unified::Uitem::Data(unified::Receiver::Orchard(bytes)) = item {
            return Option::<Address>::from(Address::from_raw_address_bytes(bytes))
                .ok_or_else(|| anyhow::anyhow!("invalid shielded receiver"));
        }
    }
    bail!("destination lacks an Orchard-protocol receiver for Ironwood")
}
