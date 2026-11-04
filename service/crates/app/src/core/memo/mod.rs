use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use sha3::Keccak256;

pub fn make_memo(recipient: &str) -> Result<String> {
    let recipient = recipient.to_ascii_lowercase();
    validate_recipient(&recipient)?;
    let body = format!("cwzec:v1:11155111:{recipient}");
    Ok(format!(
        "{body}:{}",
        hex::encode(&Sha256::digest(body.as_bytes())[..4])
    ))
}

pub fn parse_memo(bytes: &[u8; 512]) -> Result<[u8; 20]> {
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&bytes[..end])?;
    ensure!(text.is_ascii(), "memo must be ASCII");
    let fields: Vec<_> = text.split(':').collect();
    ensure!(
        fields.len() == 5 && fields[0] == "cwzec" && fields[1] == "v1" && fields[2] == "11155111",
        "unsupported memo grammar or network"
    );
    let recipient = validate_recipient(fields[3])?;
    ensure!(
        make_memo(fields[3])? == text,
        "invalid memo checksum or trailing content"
    );
    Ok(recipient)
}

fn validate_recipient(recipient: &str) -> Result<[u8; 20]> {
    ensure!(
        recipient.len() == 42 && recipient.starts_with("0x"),
        "recipient must be a lowercase EVM address"
    );
    ensure!(
        recipient[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "recipient must be lowercase hex"
    );
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(&recipient[2..], &mut bytes)?;
    Ok(bytes)
}

/// Canonical single-testnet Ironwood tag; inputs come from a chain-accepted V3 note.
pub fn deposit_id(rho: &[u8; 32], rseed: &[u8; 32]) -> [u8; 32] {
    let mut hash = Keccak256::new();
    hash.update(b"cwzec/deposit/v1");
    hash.update(rho);
    hash.update(rseed);
    hash.finalize().into()
}

#[cfg(test)]
mod tests;
